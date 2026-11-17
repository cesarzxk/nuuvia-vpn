//! Room-wide group framing used by the relay mesh.
//!
//! Mesh peers share the room PSK, so the relay can flood one ciphertext to
//! every member without decrypting it. This mode is used when a room has more
//! than two players; the pairwise Noise mode remains available for 1:1 CLI
//! sessions.

use std::collections::HashMap;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use nuvia_protocol::{GROUP_MAGIC, PEER_TAG_LEN};

use crate::wire::ReplayWindow;
use crate::{Error, Result};

/// `GROUP_MAGIC` + src tag + epoch + counter.
pub const GROUP_HEADER_LEN: usize = 4 + PEER_TAG_LEN + 8 + 8;
pub const GROUP_TAG_LEN: usize = 16;

pub struct GroupSession {
    send_cipher: XChaCha20Poly1305,
    recv_cipher: XChaCha20Poly1305,
    self_tag: [u8; PEER_TAG_LEN],
    epoch: u64,
    send_counter: u64,
    recv: HashMap<([u8; PEER_TAG_LEN], u64), ReplayWindow>,
}

impl std::fmt::Debug for GroupSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupSession")
            .field("self_tag", &hex::encode(self.self_tag))
            .field("epoch", &self.epoch)
            .field("send_counter", &self.send_counter)
            .field("senders", &self.recv.len())
            .finish()
    }
}

impl GroupSession {
    pub fn new(key: &[u8; 32], self_tag: [u8; PEER_TAG_LEN]) -> Self {
        Self {
            send_cipher: XChaCha20Poly1305::new(Key::from_slice(key)),
            recv_cipher: XChaCha20Poly1305::new(Key::from_slice(key)),
            self_tag,
            // A new epoch on every process start avoids nonce reuse with the
            // long-lived room group key and lets receivers reset their replay
            // windows after a restart.
            epoch: rand::random(),
            send_counter: 0,
            recv: HashMap::new(),
        }
    }

    pub fn self_tag(&self) -> &[u8; PEER_TAG_LEN] {
        &self.self_tag
    }

    /// Encrypts one Ethernet frame (empty payload means keepalive).
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        if self.send_counter == u64::MAX {
            return Err(Error::protocol("group send counter exhausted"));
        }

        let mut header = Vec::with_capacity(GROUP_HEADER_LEN);
        header.extend_from_slice(&GROUP_MAGIC);
        header.extend_from_slice(&self.self_tag);
        header.extend_from_slice(&self.epoch.to_be_bytes());
        header.extend_from_slice(&self.send_counter.to_be_bytes());

        let nonce = nonce_for(self.epoch, self.send_counter);
        let ciphertext = self
            .send_cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &header,
                },
            )
            .map_err(Error::from)?;

        self.send_counter += 1;
        header.extend_from_slice(&ciphertext);
        Ok(header)
    }

    /// Decrypts one frame and returns `(sender_tag, plaintext)`.
    pub fn open(&mut self, packet: &[u8]) -> Result<([u8; PEER_TAG_LEN], Vec<u8>)> {
        if packet.len() < GROUP_HEADER_LEN + GROUP_TAG_LEN {
            return Err(Error::protocol("short group packet"));
        }
        if packet[..4] != GROUP_MAGIC {
            return Err(Error::protocol("bad group magic"));
        }

        let mut src = [0u8; PEER_TAG_LEN];
        src.copy_from_slice(&packet[4..12]);
        if src == self.self_tag {
            return Err(Error::protocol("group packet from self"));
        }
        let epoch = u64::from_be_bytes(packet[12..20].try_into().unwrap());
        let counter = u64::from_be_bytes(packet[20..28].try_into().unwrap());

        let nonce = nonce_for(epoch, counter);
        let plaintext = self.recv_cipher.decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &packet[GROUP_HEADER_LEN..],
                aad: &packet[..GROUP_HEADER_LEN],
            },
        )?;

        let window = self.recv.entry((src, epoch)).or_default();
        if !window.check_and_mark(counter) {
            return Err(Error::protocol("replayed or too old group packet"));
        }
        Ok((src, plaintext))
    }
}

fn nonce_for(epoch: u64, counter: u64) -> [u8; 24] {
    let mut nonce = [0u8; 24];
    nonce[..8].copy_from_slice(&epoch.to_be_bytes());
    nonce[8..16].copy_from_slice(&counter.to_be_bytes());
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(n: u8) -> [u8; PEER_TAG_LEN] {
        [n; PEER_TAG_LEN]
    }

    #[test]
    fn roundtrip_and_replay() {
        let key = [7u8; 32];
        let mut alice = GroupSession::new(&key, tag(1));
        let mut bob = GroupSession::new(&key, tag(2));
        let packet = alice.seal(b"ethernet").unwrap();
        let (src, data) = bob.open(&packet).unwrap();
        assert_eq!(src, tag(1));
        assert_eq!(data, b"ethernet");
        assert!(bob.open(&packet).is_err());
    }

    #[test]
    fn rejects_tamper() {
        let key = [7u8; 32];
        let mut alice = GroupSession::new(&key, tag(1));
        let mut bob = GroupSession::new(&key, tag(2));
        let mut packet = alice.seal(b"ethernet").unwrap();
        let last = packet.len() - 1;
        packet[last] ^= 1;
        assert!(bob.open(&packet).is_err());
    }
}
