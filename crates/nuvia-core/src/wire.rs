use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use zeroize::Zeroize;

use crate::{Error, Result};

pub const MAGIC: [u8; 4] = *b"NUV1";
pub const TYPE_HANDSHAKE: u8 = 1;
pub const TYPE_DATA: u8 = 2;
/// Plaintext NAT punch. Opens the peer's mapping; payload is ignored.
pub const TYPE_PUNCH: u8 = 3;

/// `MAGIC` + type + session id + counter.
pub const DATA_HEADER_LEN: usize = 4 + 1 + 8 + 8;
pub const TAG_LEN: usize = 16;

/// Ethernet frames are at most 1514 bytes; leave room for headers/options.
pub const MAX_PACKET: usize = 65_535;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataHeader {
    pub session_id: u64,
    pub counter: u64,
}

impl DataHeader {
    pub fn new(session_id: u64, counter: u64) -> Self {
        Self {
            session_id,
            counter,
        }
    }

    pub fn encode(&self) -> [u8; DATA_HEADER_LEN] {
        let mut buf = [0u8; DATA_HEADER_LEN];
        buf[..4].copy_from_slice(&MAGIC);
        buf[4] = TYPE_DATA;
        buf[5..13].copy_from_slice(&self.session_id.to_be_bytes());
        buf[13..21].copy_from_slice(&self.counter.to_be_bytes());
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < DATA_HEADER_LEN {
            return Err(Error::protocol("packet too short for data header"));
        }
        if buf[..4] != MAGIC {
            return Err(Error::protocol("bad magic"));
        }
        if buf[4] != TYPE_DATA {
            return Err(Error::protocol(format!(
                "expected data packet, got type {}",
                buf[4]
            )));
        }
        let session_id = u64::from_be_bytes(buf[5..13].try_into().unwrap());
        let counter = u64::from_be_bytes(buf[13..21].try_into().unwrap());
        Ok(Self {
            session_id,
            counter,
        })
    }
}

/// Returns the packet type if the magic is valid.
pub fn packet_type(buf: &[u8]) -> Result<u8> {
    if buf.len() < 5 {
        return Err(Error::protocol("packet too short"));
    }
    if buf[..4] != MAGIC {
        return Err(Error::protocol("bad magic"));
    }
    Ok(buf[4])
}

/// Sliding window replay protection, sized at 64 packets.
///
/// UDP can reorder packets freely, so a strict monotonic counter is not
/// enough. A packet is accepted once, as long as it is not older than the
/// window.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReplayWindow {
    highest: Option<u64>,
    bitmap: u64,
}

impl ReplayWindow {
    /// Marks `counter` as seen. Returns `false` for duplicates and packets
    /// older than the window.
    pub fn check_and_mark(&mut self, counter: u64) -> bool {
        match self.highest {
            None => {
                self.highest = Some(counter);
                self.bitmap = 1;
                true
            }
            Some(highest) if counter > highest => {
                let shift = counter - highest;
                self.bitmap = if shift >= 64 {
                    1
                } else {
                    (self.bitmap << shift) | 1
                };
                self.highest = Some(counter);
                true
            }
            Some(highest) => {
                let diff = highest - counter;
                if diff >= 64 {
                    return false;
                }
                let mask = 1u64 << diff;
                if self.bitmap & mask != 0 {
                    return false;
                }
                self.bitmap |= mask;
                true
            }
        }
    }
}

/// An established, encrypted session with one peer.
pub struct Session {
    session_id: u64,
    send_cipher: ChaCha20Poly1305,
    recv_cipher: ChaCha20Poly1305,
    send_counter: u64,
    recv_window: ReplayWindow,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("session_id", &self.session_id)
            .field("send_counter", &self.send_counter)
            .finish_non_exhaustive()
    }
}

impl Session {
    pub fn new(session_id: u64, send_key: &[u8; 32], recv_key: &[u8; 32]) -> Self {
        Self {
            session_id,
            send_cipher: ChaCha20Poly1305::new(Key::from_slice(send_key)),
            recv_cipher: ChaCha20Poly1305::new(Key::from_slice(recv_key)),
            send_counter: 0,
            recv_window: ReplayWindow::default(),
        }
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    /// Encrypts one frame. The returned buffer is a complete UDP payload.
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        if self.send_counter == u64::MAX {
            return Err(Error::protocol("send counter exhausted"));
        }

        let header = DataHeader::new(self.session_id, self.send_counter);
        let aad = header.encode();
        let nonce = nonce_for(self.send_counter);
        let ciphertext = self
            .send_cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(Error::from)?;

        self.send_counter += 1;

        let mut packet = Vec::with_capacity(aad.len() + ciphertext.len());
        packet.extend_from_slice(&aad);
        packet.extend_from_slice(&ciphertext);
        Ok(packet)
    }

    /// Decrypts and authenticates one frame.
    pub fn open(&mut self, packet: &[u8]) -> Result<Vec<u8>> {
        let header = DataHeader::decode(packet)?;
        if header.session_id != self.session_id {
            return Err(Error::protocol("packet belongs to another session"));
        }

        let nonce = nonce_for(header.counter);
        let plaintext = self.recv_cipher.decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &packet[DATA_HEADER_LEN..],
                aad: &packet[..DATA_HEADER_LEN],
            },
        )?;

        if !self.recv_window.check_and_mark(header.counter) {
            return Err(Error::protocol("replayed or too old packet"));
        }
        Ok(plaintext)
    }
}

/// Derives the two directional traffic keys from the Noise handshake hash,
/// both random seeds and the room PSK.
///
/// Mixing the PSK here means a peer must know the room password to produce
/// valid frames, even if it knows the other side's static public key.
pub fn derive_traffic_keys(
    handshake_hash: &[u8; 32],
    initiator_seed: &[u8; 32],
    responder_seed: &[u8; 32],
    psk: &[u8; 32],
    local_is_initiator: bool,
) -> ([u8; 32], [u8; 32]) {
    let mut ikm = Vec::with_capacity(32 * 4);
    ikm.extend_from_slice(handshake_hash);
    ikm.extend_from_slice(initiator_seed);
    ikm.extend_from_slice(responder_seed);
    ikm.extend_from_slice(psk);

    let i2r = blake3::derive_key("nuvia v1 traffic i2r", &ikm);
    let r2i = blake3::derive_key("nuvia v1 traffic r2i", &ikm);
    ikm.zeroize();

    if local_is_initiator {
        (i2r, r2i)
    } else {
        (r2i, i2r)
    }
}

fn nonce_for(counter: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(&counter.to_be_bytes());
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEND: [u8; 32] = [7u8; 32];
    const RECV: [u8; 32] = [9u8; 32];
    const SESSION: u64 = 0x1122_3344_5566_7788;

    fn pair() -> (Session, Session) {
        (
            Session::new(SESSION, &SEND, &RECV),
            Session::new(SESSION, &RECV, &SEND),
        )
    }

    #[test]
    fn data_header_roundtrip() {
        let header = DataHeader::new(SESSION, 42);
        let encoded = header.encode();
        assert_eq!(DataHeader::decode(&encoded).unwrap(), header);
    }

    #[test]
    fn encrypts_and_decrypts() {
        let (mut a, mut b) = pair();
        let packet = a.seal(b"hello nuvia").unwrap();
        assert_eq!(b.open(&packet).unwrap(), b"hello nuvia");
    }

    #[test]
    fn rejects_replay() {
        let (mut a, mut b) = pair();
        let packet = a.seal(b"once").unwrap();
        assert_eq!(b.open(&packet).unwrap(), b"once");
        assert!(b.open(&packet).is_err());
    }

    #[test]
    fn allows_out_of_order_within_window() {
        let (mut a, mut b) = pair();
        let packets: Vec<_> = (0..3).map(|i| a.seal(&[i as u8]).unwrap()).collect();
        assert_eq!(b.open(&packets[2]).unwrap(), [2]);
        assert_eq!(b.open(&packets[0]).unwrap(), [0]);
        assert_eq!(b.open(&packets[1]).unwrap(), [1]);
    }

    #[test]
    fn rejects_packets_older_than_window() {
        let (mut a, mut b) = pair();
        let packets: Vec<_> = (0..100).map(|i| a.seal(&[i as u8]).unwrap()).collect();
        assert_eq!(b.open(&packets[99]).unwrap(), [99]);
        assert!(b.open(&packets[0]).is_err());
    }

    #[test]
    fn rejects_tampering() {
        let (mut a, mut b) = pair();
        let mut packet = a.seal(b"secret").unwrap();
        let last = packet.len() - 1;
        packet[last] ^= 0x01;
        assert!(b.open(&packet).is_err());
    }

    #[test]
    fn rejects_other_session() {
        let (mut a, _) = pair();
        let mut other = Session::new(SESSION + 1, &SEND, &RECV);
        let packet = a.seal(b"secret").unwrap();
        assert!(other.open(&packet).is_err());
    }

    #[test]
    fn directions_are_symmetric() {
        let hash = [1u8; 32];
        let iseed = [2u8; 32];
        let rseed = [3u8; 32];
        let psk = [4u8; 32];

        let (i_send, i_recv) = derive_traffic_keys(&hash, &iseed, &rseed, &psk, true);
        let (r_send, r_recv) = derive_traffic_keys(&hash, &iseed, &rseed, &psk, false);

        assert_eq!(i_send, r_recv);
        assert_eq!(i_recv, r_send);
        assert_ne!(i_send, i_recv);
    }

    #[test]
    fn same_handshake_different_psk_different_keys() {
        let hash = [1u8; 32];
        let iseed = [2u8; 32];
        let rseed = [3u8; 32];
        let (a, _) = derive_traffic_keys(&hash, &iseed, &rseed, &[4u8; 32], true);
        let (b, _) = derive_traffic_keys(&hash, &iseed, &rseed, &[5u8; 32], true);
        assert_ne!(a, b);
    }
}
