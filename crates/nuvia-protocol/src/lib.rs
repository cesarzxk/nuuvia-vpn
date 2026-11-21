//! Shared wire types between the nuvia client and the rendezvous server.
//!
//! The server never sees the room password or the traffic keys. It only sees
//! opaque room ids, peer public keys, the relay token and UDP endpoints.

use base64::Engine;
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;
pub const ROOM_ID_LEN: usize = 16;
pub const PEER_ID_LEN: usize = 32;
pub const TOKEN_LEN: usize = 16;
pub const DEFAULT_SERVER_UDP_PORT: u16 = 8443;
/// Room capacity used when a client does not request one. The GUI keeps this
/// default so normal rooms stay small and easy to manage.
pub const DEFAULT_MAX_PEERS: u16 = 8;
/// Maximum room capacity the stock CLI accepts. A server can impose a lower
/// hard cap in its configuration.
pub const MAX_PEERS_PER_ROOM: u16 = 32;

pub const HELLO_MAGIC: [u8; 4] = *b"NUVH";
pub const HELLO_ACK_MAGIC: [u8; 4] = *b"NUVA";
pub const RELAY_MAGIC: [u8; 4] = *b"NUVR";
pub const PEER_TAG_LEN: usize = 8;
pub const GROUP_MAGIC: [u8; 4] = *b"NUVG";
/// Destination tag used by the mesh mode to ask the server to flood an inner
/// packet to every other peer in the room.
pub const BROADCAST_TAG: [u8; PEER_TAG_LEN] = [0u8; PEER_TAG_LEN];
pub const HELLO_LEN: usize = 4 + 2 + ROOM_ID_LEN + PEER_ID_LEN + TOKEN_LEN;
pub const HELLO_ACK_LEN: usize = 4 + 2 + 1;
pub const RELAY_HEADER_LEN: usize = 4 + 2 + ROOM_ID_LEN + PEER_TAG_LEN + PEER_TAG_LEN + TOKEN_LEN;

pub const ACK_OK: u8 = 0;
pub const ACK_INVALID_TOKEN: u8 = 1;
pub const ACK_ROOM_NOT_FOUND: u8 = 2;
pub const ACK_ROOM_FULL: u8 = 3;

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("invalid hex: {0}")]
    Hex(#[from] hex::FromHexError),
    #[error("expected {expected} bytes, got {actual}")]
    Length { expected: usize, actual: usize },
    #[error("bad magic")]
    BadMagic,
    #[error("unsupported protocol version {0}")]
    Version(u16),
    #[error("invalid invite: {0}")]
    Invite(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub protocol: u16,
    pub room_id: String,
    pub peer_id: String,
    pub token: String,
    /// `false` joins an existing room only; the server returns 404 when the
    /// room is unknown. Omitted/`true` preserves the original create-if-missing
    /// behavior for older clients.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub create: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Human-readable room name. Used only when the room is created, so the
    /// server can list public rooms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_name: Option<String>,
    /// Advertise the room in the public directory.
    #[serde(default, skip_serializing_if = "is_false")]
    pub public: bool,
    /// Directory hint: the creator set a password. The server never sees the
    /// password itself.
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_password: bool,
    /// Requested capacity when creating a room. The server clamps it to its
    /// configured hard cap. Omitted means [`DEFAULT_MAX_PEERS`], which keeps
    /// older clients compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_peers: Option<u16>,
    /// LAN addresses (`ip:port`) where this peer's UDP socket can also be
    /// reached. They are hints for direct paths between peers behind the same
    /// NAT, where the public endpoint is not hairpin-reachable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_endpoints: Vec<String>,
}

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicRoom {
    pub name: String,
    pub has_password: bool,
    pub peers: usize,
    pub max_peers: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicRoomsResponse {
    pub protocol: u16,
    pub rooms: Vec<PublicRoom>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub peer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_endpoints: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub protocol: u16,
    pub room_id: String,
    pub self_endpoint: Option<String>,
    pub peers: Vec<PeerInfo>,
    pub ttl_secs: u64,
}

/// Unauthenticated UDP hello. The room token proves the peer knows the invite.
///
/// The token is a bearer capability. It is separate from the room password:
/// the server can route/authenticate discovery without ever being able to
/// derive the end-to-end traffic keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hello {
    pub room_id: [u8; ROOM_ID_LEN],
    pub peer_id: [u8; PEER_ID_LEN],
    pub token: [u8; TOKEN_LEN],
}

impl Hello {
    pub fn encode(&self) -> [u8; HELLO_LEN] {
        let mut buf = [0u8; HELLO_LEN];
        buf[..4].copy_from_slice(&HELLO_MAGIC);
        buf[4..6].copy_from_slice(&PROTOCOL_VERSION.to_be_bytes());
        buf[6..22].copy_from_slice(&self.room_id);
        buf[22..54].copy_from_slice(&self.peer_id);
        buf[54..70].copy_from_slice(&self.token);
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        if buf.len() < HELLO_LEN {
            return Err(ProtocolError::Length {
                expected: HELLO_LEN,
                actual: buf.len(),
            });
        }
        if buf[..4] != HELLO_MAGIC {
            return Err(ProtocolError::BadMagic);
        }
        let version = u16::from_be_bytes(buf[4..6].try_into().unwrap());
        if version != PROTOCOL_VERSION {
            return Err(ProtocolError::Version(version));
        }
        let mut room_id = [0u8; ROOM_ID_LEN];
        room_id.copy_from_slice(&buf[6..22]);
        let mut peer_id = [0u8; PEER_ID_LEN];
        peer_id.copy_from_slice(&buf[22..54]);
        let mut token = [0u8; TOKEN_LEN];
        token.copy_from_slice(&buf[54..70]);
        Ok(Self {
            room_id,
            peer_id,
            token,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelloAck {
    pub status: u8,
}

impl HelloAck {
    pub fn ok() -> Self {
        Self { status: ACK_OK }
    }

    pub fn invalid_token() -> Self {
        Self {
            status: ACK_INVALID_TOKEN,
        }
    }

    pub fn room_not_found() -> Self {
        Self {
            status: ACK_ROOM_NOT_FOUND,
        }
    }

    pub fn room_full() -> Self {
        Self {
            status: ACK_ROOM_FULL,
        }
    }

    pub fn encode(&self) -> [u8; HELLO_ACK_LEN] {
        let mut buf = [0u8; HELLO_ACK_LEN];
        buf[..4].copy_from_slice(&HELLO_ACK_MAGIC);
        buf[4..6].copy_from_slice(&PROTOCOL_VERSION.to_be_bytes());
        buf[6] = self.status;
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        if buf.len() < HELLO_ACK_LEN {
            return Err(ProtocolError::Length {
                expected: HELLO_ACK_LEN,
                actual: buf.len(),
            });
        }
        if buf[..4] != HELLO_ACK_MAGIC {
            return Err(ProtocolError::BadMagic);
        }
        let version = u16::from_be_bytes(buf[4..6].try_into().unwrap());
        if version != PROTOCOL_VERSION {
            return Err(ProtocolError::Version(version));
        }
        Ok(Self { status: buf[6] })
    }
}

/// Shareable invite. The password is intentionally **not** part of it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invite {
    pub version: u16,
    pub server: String,
    pub server_udp: String,
    pub room: String,
    pub token: String,
}

impl Invite {
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("invite is always serializable");
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
    }

    pub fn decode(value: &str) -> Result<Self, ProtocolError> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value.trim())
            .map_err(|e| ProtocolError::Invite(e.to_string()))?;
        let invite: Self = serde_json::from_slice(&bytes)?;
        if invite.version != PROTOCOL_VERSION {
            return Err(ProtocolError::Version(invite.version));
        }
        Ok(invite)
    }
}

pub fn peer_tag(peer_id: &[u8; PEER_ID_LEN]) -> [u8; PEER_TAG_LEN] {
    let mut tag = [0u8; PEER_TAG_LEN];
    tag.copy_from_slice(&peer_id[..PEER_TAG_LEN]);
    tag
}

#[derive(Debug)]
pub struct RelayPacket<'a> {
    pub room_id: [u8; ROOM_ID_LEN],
    pub src: [u8; PEER_TAG_LEN],
    pub dst: [u8; PEER_TAG_LEN],
    pub token: [u8; TOKEN_LEN],
    pub inner: &'a [u8],
}

pub fn encode_relay(
    room_id: &[u8; ROOM_ID_LEN],
    src: &[u8; PEER_TAG_LEN],
    dst: &[u8; PEER_TAG_LEN],
    token: &[u8; TOKEN_LEN],
    inner: &[u8],
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(RELAY_HEADER_LEN + inner.len());
    buf.extend_from_slice(&RELAY_MAGIC);
    buf.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    buf.extend_from_slice(room_id);
    buf.extend_from_slice(src);
    buf.extend_from_slice(dst);
    buf.extend_from_slice(token);
    buf.extend_from_slice(inner);
    buf
}

pub fn decode_relay(buf: &[u8]) -> Result<RelayPacket<'_>, ProtocolError> {
    if buf.len() < RELAY_HEADER_LEN {
        return Err(ProtocolError::Length {
            expected: RELAY_HEADER_LEN,
            actual: buf.len(),
        });
    }
    if buf[..4] != RELAY_MAGIC {
        return Err(ProtocolError::BadMagic);
    }
    let version = u16::from_be_bytes(buf[4..6].try_into().unwrap());
    if version != PROTOCOL_VERSION {
        return Err(ProtocolError::Version(version));
    }

    let mut room_id = [0u8; ROOM_ID_LEN];
    room_id.copy_from_slice(&buf[6..22]);
    let mut src = [0u8; PEER_TAG_LEN];
    src.copy_from_slice(&buf[22..30]);
    let mut dst = [0u8; PEER_TAG_LEN];
    dst.copy_from_slice(&buf[30..38]);
    let mut token = [0u8; TOKEN_LEN];
    token.copy_from_slice(&buf[38..54]);

    Ok(RelayPacket {
        room_id,
        src,
        dst,
        token,
        inner: &buf[RELAY_HEADER_LEN..],
    })
}

pub fn parse_hex<const N: usize>(value: &str) -> Result<[u8; N], ProtocolError> {
    let bytes = hex::decode(value.trim())?;
    if bytes.len() != N {
        return Err(ProtocolError::Length {
            expected: N,
            actual: bytes.len(),
        });
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

pub fn hex_string(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_roundtrip() {
        let hello = Hello {
            room_id: [0x11; ROOM_ID_LEN],
            peer_id: [0x22; PEER_ID_LEN],
            token: [0x33; TOKEN_LEN],
        };
        assert_eq!(Hello::decode(&hello.encode()).unwrap(), hello);
    }

    #[test]
    fn ack_roundtrip() {
        let ack = HelloAck::ok();
        assert_eq!(HelloAck::decode(&ack.encode()).unwrap(), ack);
    }

    #[test]
    fn invite_roundtrip() {
        let invite = Invite {
            version: PROTOCOL_VERSION,
            server: "https://relay.example.com".into(),
            server_udp: "relay.example.com:8443".into(),
            room: "casa".into(),
            token: "aa".repeat(TOKEN_LEN),
        };
        let encoded = invite.encode();
        let decoded = Invite::decode(&encoded).unwrap();
        assert_eq!(decoded.room, invite.room);
        assert_eq!(decoded.token, invite.token);
        assert_eq!(decoded.server, invite.server);
    }

    #[test]
    fn public_room_directory_roundtrip() {
        let request = RegisterRequest {
            protocol: PROTOCOL_VERSION,
            room_id: "11".repeat(ROOM_ID_LEN),
            peer_id: "22".repeat(PEER_ID_LEN),
            token: "33".repeat(TOKEN_LEN),
            create: true,
            name: Some("host".into()),
            room_name: Some("casa".into()),
            public: true,
            has_password: false,
            max_peers: Some(16),
            local_endpoints: Vec::new(),
        };
        let encoded = serde_json::to_string(&request).unwrap();
        let decoded: RegisterRequest = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.room_name.as_deref(), Some("casa"));
        assert!(decoded.public);
        assert!(!decoded.has_password);
        assert_eq!(decoded.max_peers, Some(16));

        // Older clients omit the field and must keep working with the default.
        let legacy = encoded.replace(",\"max_peers\":16", "");
        let decoded: RegisterRequest = serde_json::from_str(&legacy).unwrap();
        assert_eq!(decoded.max_peers, None);

        let response = PublicRoomsResponse {
            protocol: PROTOCOL_VERSION,
            rooms: vec![PublicRoom {
                name: "casa".into(),
                has_password: false,
                peers: 2,
                max_peers: 8,
            }],
        };
        let encoded = serde_json::to_string(&response).unwrap();
        let decoded: PublicRoomsResponse = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.rooms.len(), 1);
        assert_eq!(decoded.rooms[0].name, "casa");
        assert_eq!(decoded.rooms[0].peers, 2);
    }

    #[test]
    fn relay_roundtrip() {
        let room = [1u8; ROOM_ID_LEN];
        let src = [2u8; PEER_TAG_LEN];
        let dst = [3u8; PEER_TAG_LEN];
        let token = [4u8; TOKEN_LEN];
        let inner = b"NUV1\x02payload";

        let packet = encode_relay(&room, &src, &dst, &token, inner);
        let decoded = decode_relay(&packet).unwrap();
        assert_eq!(decoded.room_id, room);
        assert_eq!(decoded.src, src);
        assert_eq!(decoded.dst, dst);
        assert_eq!(decoded.token, token);
        assert_eq!(decoded.inner, inner);
    }

    #[test]
    fn peer_tag_is_prefix() {
        let peer = [0x42u8; PEER_ID_LEN];
        assert_eq!(peer_tag(&peer), [0x42u8; PEER_TAG_LEN]);
    }
}
