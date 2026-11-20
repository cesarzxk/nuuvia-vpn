use std::net::Ipv4Addr;

use argon2::{Algorithm, Argon2, Params, Version};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{Error, Result};

pub const ROOM_ID_LEN: usize = 16;
pub const PSK_LEN: usize = 32;

/// Material shared by every member of a nuvia room.
///
/// * `id` is public and can be advertised in beacons.
/// * `psk` is secret and never leaves the device; the relay server never sees
///   it. It is mixed into the traffic key derivation, so knowing a peer's
///   static public key is not enough to exchange data.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct RoomKey {
    id: [u8; ROOM_ID_LEN],
    psk: [u8; PSK_LEN],
}

impl std::fmt::Debug for RoomKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomKey")
            .field("id", &hex::encode(self.id))
            .field("psk", &"<redacted>")
            .finish()
    }
}

impl RoomKey {
    /// Derive the room id and pre-shared key from a room name and password.
    ///
    /// Argon2id makes brute forcing the password from a leaked PSK expensive.
    /// The password itself is never stored or transmitted. An empty password
    /// makes an open room: every peer derives the same key from the public
    /// room name.
    pub fn derive(room: &str, password: &str) -> Result<Self> {
        let room = room.trim();
        if room.is_empty() {
            return Err(Error::config("room name must not be empty"));
        }

        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nuvia room id v1\0");
        hasher.update(room.as_bytes());
        let room_hash = hasher.finalize();
        let mut id = [0u8; ROOM_ID_LEN];
        id.copy_from_slice(&room_hash.as_bytes()[..ROOM_ID_LEN]);

        // Explicit parameters make the cost part of the protocol. Changing
        // them later is a breaking wire change.
        let params =
            Params::new(19_456, 2, 1, Some(PSK_LEN)).map_err(|e| Error::Argon2(e.to_string()))?;
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

        let mut psk = [0u8; PSK_LEN];
        argon
            .hash_password_into(password.as_bytes(), &id, &mut psk)
            .map_err(|e| Error::Argon2(e.to_string()))?;

        Ok(Self { id, psk })
    }

    pub fn id(&self) -> &[u8; ROOM_ID_LEN] {
        &self.id
    }

    pub fn id_hex(&self) -> String {
        hex::encode(self.id)
    }

    pub fn psk(&self) -> &[u8; PSK_LEN] {
        &self.psk
    }

    /// Deterministic bearer token for the rendezvous/relay server.
    ///
    /// It is derived from the room PSK with a separate domain separator, so it
    /// is not the traffic key. The server sees this token (needed for routing)
    /// and can therefore attempt offline password guessing; the room password
    /// is still never transmitted and the E2E traffic keys never leave the
    /// peers.
    pub fn relay_token(&self) -> [u8; 16] {
        let derived = blake3::derive_key("nuvia v1 relay token", &self.psk);
        let mut token = [0u8; 16];
        token.copy_from_slice(&derived[..16]);
        token
    }

    /// Deterministic third octet for the room virtual subnet
    /// (`10.240.<octet>.0/24`).
    pub fn subnet_octet(&self) -> u8 {
        (self.id[0] % 250) + 1
    }

    /// Symmetric key used by the mesh (relay-flooded) mode.
    ///
    /// Every member of the room derives the same key, so the relay server sees
    /// only ciphertext. Unlike the pairwise Noise mode, this authenticates
    /// membership in the room, not an individual peer: any member with the
    /// password can inject L2 frames, just like on a physical LAN.
    pub fn group_key(&self) -> [u8; 32] {
        blake3::derive_key("nuvia v1 group data", &self.psk)
    }
}

/// Virtual IP shared by all rooms on one machine (compact L2 VPN style).
///
/// The mesh GUI uses a single adapter and a single IP for every room. The IP is
/// stable for the identity and lives in `10.240.0.0/16`; different rooms are
/// isolated by their relay/group keys, not by the local IP.
pub fn mesh_ip(public_key: &[u8; 32]) -> Ipv4Addr {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nuvia v1 mesh ip v2\0");
    hasher.update(public_key);
    let hash = hasher.finalize();
    let a = (hash.as_bytes()[0] % 254) + 1;
    let b = (hash.as_bytes()[1] % 254) + 1;
    Ipv4Addr::new(10, 240, a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_credentials_same_key() {
        let a = RoomKey::derive("casa", "segredo").unwrap();
        let b = RoomKey::derive("casa", "segredo").unwrap();
        assert_eq!(a.id(), b.id());
        assert_eq!(a.psk(), b.psk());
    }

    #[test]
    fn credentials_are_bound() {
        let a = RoomKey::derive("casa", "segredo").unwrap();
        let b = RoomKey::derive("casa", "outro").unwrap();
        let c = RoomKey::derive("escritorio", "segredo").unwrap();
        assert_eq!(a.id(), b.id());
        assert_ne!(a.psk(), b.psk());
        assert_ne!(a.id(), c.id());
        assert_ne!(a.psk(), c.psk());
    }

    #[test]
    fn rejects_empty_input() {
        assert!(RoomKey::derive("", "x").is_err());
        assert!(RoomKey::derive("x", "").is_ok());
    }

    #[test]
    fn open_room_depends_only_on_the_name() {
        let a = RoomKey::derive("casa", "").unwrap();
        let b = RoomKey::derive(" casa ", "").unwrap();
        let locked = RoomKey::derive("casa", "segredo").unwrap();
        assert_eq!(a.id(), b.id());
        assert_eq!(a.psk(), b.psk());
        assert_eq!(a.id(), locked.id());
        assert_ne!(a.psk(), locked.psk());
    }

    #[test]
    fn relay_token_and_subnet_are_deterministic() {
        let a = RoomKey::derive("casa", "segredo").unwrap();
        let b = RoomKey::derive("casa", "segredo").unwrap();
        assert_eq!(a.relay_token(), b.relay_token());
        assert_eq!(a.subnet_octet(), b.subnet_octet());
        assert!((1..=250).contains(&a.subnet_octet()));
        assert_ne!(a.relay_token(), [0u8; 16]);
    }
}
