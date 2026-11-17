use rand::rngs::OsRng;
use rand::RngCore;
use zeroize::Zeroize;

use crate::wire::{derive_traffic_keys, Session};
use crate::{Error, Result};

pub const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const SEED_LEN: usize = 32;
const MAX_MESSAGE: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Initiator,
    Responder,
}

impl Role {
    pub fn is_initiator(self) -> bool {
        matches!(self, Role::Initiator)
    }

    /// Deterministic role selection so two peers never initiate at the same
    /// time. The smaller static public key initiates.
    pub fn for_peers(local_public: &[u8; 32], peer_public: &[u8; 32]) -> Self {
        if local_public < peer_public {
            Role::Initiator
        } else {
            Role::Responder
        }
    }
}

/// One side of a Noise XX handshake.
///
/// XX authenticates both static public keys without either side knowing the
/// other's key in advance. The seeds exchanged in the encrypted payloads are
/// combined with the handshake hash and the room PSK to derive the traffic
/// keys.
pub struct Handshake {
    state: snow::HandshakeState,
    role: Role,
    local_seed: [u8; SEED_LEN],
    remote_seed: Option<[u8; SEED_LEN]>,
    sent: u8,
    received: u8,
}

impl std::fmt::Debug for Handshake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Handshake")
            .field("role", &self.role)
            .field("sent", &self.sent)
            .field("received", &self.received)
            .finish_non_exhaustive()
    }
}

impl Handshake {
    pub fn new(role: Role, private_key: &[u8; 32]) -> Result<Self> {
        let params = NOISE_PARAMS
            .parse()
            .map_err(|e| Error::config(format!("invalid noise params: {e}")))?;
        let builder = snow::Builder::new(params).local_private_key(private_key);

        let state = match role {
            Role::Initiator => builder.build_initiator()?,
            Role::Responder => builder.build_responder()?,
        };

        let mut local_seed = [0u8; SEED_LEN];
        OsRng.fill_bytes(&mut local_seed);

        Ok(Self {
            state,
            role,
            local_seed,
            remote_seed: None,
            sent: 0,
            received: 0,
        })
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn local_seed(&self) -> &[u8; SEED_LEN] {
        &self.local_seed
    }

    /// Writes the next handshake message. `payload` must be the local seed for
    /// the encrypted messages (responder message 2, initiator message 3), or
    /// empty for message 1.
    pub fn write(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        let mut message = vec![0u8; MAX_MESSAGE];
        let len = self.state.write_message(payload, &mut message)?;
        message.truncate(len);
        self.sent += 1;
        Ok(message)
    }

    /// Reads a handshake message and captures the peer's seed if present.
    pub fn read(&mut self, message: &[u8]) -> Result<Vec<u8>> {
        let mut payload = vec![0u8; MAX_MESSAGE];
        let len = self.state.read_message(message, &mut payload)?;
        payload.truncate(len);
        self.received += 1;

        match payload.len() {
            0 => {}
            SEED_LEN => {
                let mut seed = [0u8; SEED_LEN];
                seed.copy_from_slice(&payload);
                self.remote_seed = Some(seed);
            }
            other => {
                return Err(Error::handshake(format!(
                    "unexpected handshake payload length {other}"
                )));
            }
        }

        Ok(payload)
    }

    pub fn is_finished(&self) -> bool {
        match self.role {
            Role::Initiator => self.sent == 2 && self.received == 1,
            Role::Responder => self.received == 2 && self.sent == 1,
        }
    }

    /// Verifies the peer's static key and derives the session keys.
    pub fn finish(self, psk: &[u8; 32], expected_peer: &[u8; 32]) -> Result<Session> {
        if !self.is_finished() {
            return Err(Error::handshake("handshake is not complete"));
        }

        let remote_static = self
            .state
            .get_remote_static()
            .ok_or_else(|| Error::handshake("noise did not expose a remote static key"))?;
        if remote_static.len() != 32 {
            return Err(Error::handshake("unexpected remote static key length"));
        }
        let mut remote = [0u8; 32];
        remote.copy_from_slice(remote_static);
        if &remote != expected_peer {
            return Err(Error::handshake(
                "peer static key does not match the configured key",
            ));
        }

        let hash = self.state.get_handshake_hash();
        if hash.len() != 32 {
            return Err(Error::handshake("unexpected handshake hash length"));
        }
        let mut handshake_hash = [0u8; 32];
        handshake_hash.copy_from_slice(hash);

        let remote_seed = self
            .remote_seed
            .ok_or_else(|| Error::handshake("peer did not send a handshake seed"))?;

        let (initiator_seed, responder_seed) = if self.role.is_initiator() {
            (&self.local_seed, &remote_seed)
        } else {
            (&remote_seed, &self.local_seed)
        };

        let (send_key, recv_key) = derive_traffic_keys(
            &handshake_hash,
            initiator_seed,
            responder_seed,
            psk,
            self.role.is_initiator(),
        );

        let session_id = u64::from_be_bytes(handshake_hash[..8].try_into().unwrap());
        Ok(Session::new(session_id, &send_key, &recv_key))
    }
}

impl Drop for Handshake {
    fn drop(&mut self) {
        self.local_seed.zeroize();
        if let Some(seed) = self.remote_seed.as_mut() {
            seed.zeroize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PSK: [u8; 32] = [0x42; 32];

    fn complete_handshake(
        a: &crate::Identity,
        b: &crate::Identity,
        a_role: Role,
    ) -> Result<(Session, Session)> {
        let mut initiator = Handshake::new(Role::Initiator, a.private_key())?;
        let mut responder = Handshake::new(Role::Responder, b.private_key())?;

        let msg1 = initiator.write(&[])?;
        responder.read(&msg1)?;

        let responder_seed = *responder.local_seed();
        let msg2 = responder.write(&responder_seed)?;
        initiator.read(&msg2)?;

        let initiator_seed = *initiator.local_seed();
        let msg3 = initiator.write(&initiator_seed)?;
        responder.read(&msg3)?;

        assert!(initiator.is_finished());
        assert!(responder.is_finished());

        let initiator_session = initiator.finish(&PSK, b.public_key())?;
        let responder_session = responder.finish(&PSK, a.public_key())?;

        assert_eq!(
            initiator_session.session_id(),
            responder_session.session_id()
        );
        let _ = a_role;
        Ok((initiator_session, responder_session))
    }

    #[test]
    fn handshake_derives_working_session() {
        let a = crate::Identity::generate(None);
        let b = crate::Identity::generate(None);
        let (mut ia, mut ib) = complete_handshake(&a, &b, Role::Initiator).unwrap();

        let packet = ia.seal(b"hello").unwrap();
        assert_eq!(ib.open(&packet).unwrap(), b"hello");

        let packet = ib.seal(b"world").unwrap();
        assert_eq!(ia.open(&packet).unwrap(), b"world");
    }

    #[test]
    fn handshake_rejects_wrong_peer() {
        let a = crate::Identity::generate(None);
        let b = crate::Identity::generate(None);
        let other = crate::Identity::generate(None);

        let mut initiator = Handshake::new(Role::Initiator, a.private_key()).unwrap();
        let mut responder = Handshake::new(Role::Responder, b.private_key()).unwrap();
        let msg1 = initiator.write(&[]).unwrap();
        responder.read(&msg1).unwrap();
        let seed = *responder.local_seed();
        let msg2 = responder.write(&seed).unwrap();
        initiator.read(&msg2).unwrap();
        let seed = *initiator.local_seed();
        let msg3 = initiator.write(&seed).unwrap();
        responder.read(&msg3).unwrap();

        assert!(initiator.finish(&PSK, other.public_key()).is_err());
    }

    #[test]
    fn different_psk_cannot_exchange_data() {
        let a = crate::Identity::generate(None);
        let b = crate::Identity::generate(None);

        let mut initiator = Handshake::new(Role::Initiator, a.private_key()).unwrap();
        let mut responder = Handshake::new(Role::Responder, b.private_key()).unwrap();
        let msg1 = initiator.write(&[]).unwrap();
        responder.read(&msg1).unwrap();
        let seed = *responder.local_seed();
        let msg2 = responder.write(&seed).unwrap();
        initiator.read(&msg2).unwrap();
        let seed = *initiator.local_seed();
        let msg3 = initiator.write(&seed).unwrap();
        responder.read(&msg3).unwrap();

        let mut ia = initiator.finish(&PSK, b.public_key()).unwrap();
        let mut ib = responder.finish(&[0x43; 32], a.public_key()).unwrap();

        let packet = ia.seal(b"nope").unwrap();
        assert!(ib.open(&packet).is_err());
    }

    #[test]
    fn role_selection_is_opposite() {
        let low = [1u8; 32];
        let high = [2u8; 32];
        assert_eq!(Role::for_peers(&low, &high), Role::Initiator);
        assert_eq!(Role::for_peers(&high, &low), Role::Responder);
    }
}
