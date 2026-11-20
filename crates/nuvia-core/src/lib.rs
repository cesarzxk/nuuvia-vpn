//! Core networking primitives for nuvia.
//!
//! Layering, from bottom to top:
//!
//! * `identity` - long lived X25519 device key pair.
//! * `room`     - room id + pre-shared key derived from name/password.
//! * `noise`    - Noise XX handshake, authenticates both static keys.
//! * `wire`     - UDP framing, ChaCha20-Poly1305 sessions, replay window.
//! * `tunnel`   - async runner gluing a TAP device to a UDP peer.
//!
//! The L2 switch / discovery layers will sit on top of `tunnel`.

pub mod error;
pub mod firewall;
pub mod identity;
pub mod mesh;
pub mod noise;
pub mod rendezvous;
pub mod room;
pub mod tunnel;
pub mod wire;

pub use error::{Error, Result};
pub use identity::Identity;
pub use noise::{Handshake, Role};
pub use rendezvous::{
    default_server_udp, generate_token, list_public_rooms, parse_token, RendezvousClient,
    RendezvousConfig,
};
pub use room::RoomKey;
pub use wire::{DataHeader, ReplayWindow, Session};
