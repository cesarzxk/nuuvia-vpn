use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

use anyhow::{anyhow, bail, Context, Result};
use nuvia_core::identity::default_identity_path;
use nuvia_core::mesh::GroupSession;
use nuvia_core::room::mesh_ip;
use nuvia_core::tunnel::{run_mesh, shared_tap_name, MeshConfig, TapHub, TunnelStatus};
use nuvia_core::{default_server_udp, Identity, RendezvousClient, RendezvousConfig, RoomKey};
use nuvia_protocol::{peer_tag, PeerInfo};
use tokio::net::UdpSocket;

use crate::config::SERVER_URL;
use crate::i18n::{render, Lang};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Starting,
    Searching,
    Connecting,
}

#[derive(Debug, Clone)]
pub struct PeerView {
    pub name: String,
    pub ip: String,
    pub tag: [u8; nuvia_protocol::PEER_TAG_LEN],
}

#[derive(Debug, Clone)]
pub enum Event {
    Phase {
        room: String,
        phase: Phase,
    },
    Participants {
        room: String,
        own_ip: String,
        peers: Vec<PeerView>,
    },
    PeerStatus {
        room: String,
        tag: [u8; nuvia_protocol::PEER_TAG_LEN],
        direct: bool,
    },
    PeerPing {
        room: String,
        tag: [u8; nuvia_protocol::PEER_TAG_LEN],
        ms: u32,
    },
    Connected {
        room: String,
        relay: bool,
    },
    Error {
        room: String,
        message: String,
    },
    Stopped {
        room: String,
    },
}

#[derive(Debug, Clone)]
pub struct RoomConfig {
    pub name: String,
    pub password: String,
    /// Display/route name; all rooms share the same physical TAP.
    #[allow(dead_code)]
    pub tap_name: String,
    pub bind_port: u16,
    pub identity_path: PathBuf,
    pub lang: Lang,
}

pub struct Backend {
    runtime: tokio::runtime::Runtime,
    events_tx: Sender<Event>,
    events_rx: Receiver<Event>,
    sessions: std::collections::HashMap<String, tokio::task::JoinHandle<()>>,
    hub: TapHub,
    next_index: u32,
}

impl Backend {
    pub fn new() -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("creating tokio runtime")?;
        let (events_tx, events_rx) = mpsc::channel();
        let hub = {
            let _guard = runtime.enter();
            TapHub::spawn(shared_tap_name(), 1360)
        };
        Ok(Self {
            runtime,
            events_tx,
            events_rx,
            sessions: std::collections::HashMap::new(),
            hub,
            next_index: 0,
        })
    }

    pub fn allocate_slot(&mut self) -> (String, u16) {
        let index = self.next_index;
        self.next_index += 1;
        (shared_tap_name().to_string(), 42424 + index as u16)
    }

    pub fn join(&mut self, config: RoomConfig) -> Result<()> {
        let t = config.lang.strings();
        if self.sessions.contains_key(&config.name) {
            bail!(
                "{}",
                render(t.backend_already_connected, &[("name", &config.name)])
            );
        }

        let room = config.name.clone();
        let session_name = room.clone();
        let events = self.events_tx.clone();
        let hub = self.hub.clone();
        let handle = self.runtime.spawn(async move {
            match run_room(config, &events, hub).await {
                Ok(()) => {
                    let _ = events.send(Event::Stopped { room });
                }
                Err(error) => {
                    tracing::error!(room = %room, error = %format!("{error:#}"), "room failed");
                    let _ = events.send(Event::Error {
                        room,
                        message: format!("{error:#}"),
                    });
                }
            }
        });

        self.sessions.insert(session_name, handle);
        Ok(())
    }

    pub fn leave(&mut self, room: &str) {
        if let Some(handle) = self.sessions.remove(room) {
            handle.abort();
            let _ = self.events_tx.send(Event::Stopped {
                room: room.to_string(),
            });
        }
    }

    pub fn poll(&self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Ok(event) = self.events_rx.try_recv() {
            events.push(event);
        }
        events
    }
}

async fn run_room(config: RoomConfig, events: &Sender<Event>, hub: TapHub) -> Result<()> {
    let t = config.lang.strings();
    if config.password.is_empty() {
        bail!("{}", t.backend_empty_password);
    }

    let identity =
        Identity::load_or_create(&config.identity_path, hostname()).with_context(|| {
            render(
                t.backend_loading_identity,
                &[("path", &config.identity_path.display().to_string())],
            )
        })?;
    let room = RoomKey::derive(&config.name, &config.password).context(t.backend_deriving_room)?;
    let token = room.relay_token();
    let server_udp = default_server_udp(SERVER_URL)?;
    tracing::info!(room = %config.name, room_id = %room.id_hex(), "joining room");

    let _ = events.send(Event::Phase {
        room: config.name.clone(),
        phase: Phase::Starting,
    });

    let socket = UdpSocket::bind(("0.0.0.0", config.bind_port))
        .await
        .with_context(|| {
            render(
                t.backend_port_busy,
                &[("port", &config.bind_port.to_string())],
            )
        })?;

    let mut rendezvous = RendezvousConfig::new(
        SERVER_URL.to_string(),
        server_udp.clone(),
        *room.id(),
        token,
        *identity.public_key(),
    );
    if let Ok(bind) = socket.local_addr() {
        rendezvous.set_local_endpoints(bind);
    }
    rendezvous.name = hostname();
    let client = RendezvousClient::new(rendezvous).context(t.backend_rendezvous_client)?;

    let _ = events.send(Event::Phase {
        room: config.name.clone(),
        phase: Phase::Searching,
    });
    let response = client
        .announce(&socket)
        .await
        .context(t.backend_joining_room)?;

    let own_ip = mesh_ip(identity.public_key());
    let participants = participant_list(&response.peers);
    let direct_peers = nuvia_core::rendezvous::peer_endpoints(&response.peers);
    let _ = events.send(Event::Participants {
        room: config.name.clone(),
        own_ip: own_ip.to_string(),
        peers: participants,
    });
    let _ = events.send(Event::Phase {
        room: config.name.clone(),
        phase: Phase::Connecting,
    });

    let server_addr = nuvia_core::rendezvous::resolve_udp(&server_udp)
        .await
        .with_context(|| render(t.backend_resolving_relay, &[("server", &server_udp)]))?;
    let tap = hub
        .register(config.name.clone(), own_ip)
        .await
        .context(t.backend_preparing_tap)?;

    let (status_tx, mut status_rx) = tokio::sync::mpsc::channel(64);
    let status_events = events.clone();
    let room_name = config.name.clone();
    let status_task = tokio::spawn(async move {
        while let Some(status) = status_rx.recv().await {
            let event = match status {
                TunnelStatus::Established => Event::Connected {
                    room: room_name.clone(),
                    relay: false,
                },
                TunnelStatus::RelayEstablished => Event::Connected {
                    room: room_name.clone(),
                    relay: true,
                },
                TunnelStatus::Peer { tag, direct } => Event::PeerStatus {
                    room: room_name.clone(),
                    tag,
                    direct,
                },
                TunnelStatus::Ping { tag, ms } => Event::PeerPing {
                    room: room_name.clone(),
                    tag,
                    ms,
                },
            };
            let _ = status_events.send(event);
        }
    });

    let (peers_tx, mut peers_rx) = tokio::sync::mpsc::channel::<Vec<PeerInfo>>(4);
    let peers_events = events.clone();
    let peers_room = config.name.clone();
    let peers_own_ip = own_ip.to_string();
    let peers_task = tokio::spawn(async move {
        while let Some(peers) = peers_rx.recv().await {
            let _ = peers_events.send(Event::Participants {
                room: peers_room.clone(),
                own_ip: peers_own_ip.clone(),
                peers: participant_list(&peers),
            });
        }
    });

    let group = GroupSession::new(&room.group_key(), peer_tag(identity.public_key()));
    let mesh = MeshConfig {
        bind_addr: socket.local_addr()?,
        server_addr,
        direct_peers,
        room_id: *room.id(),
        token,
        status: Some(status_tx),
        peers: Some(peers_tx),
    };

    let result = run_mesh(mesh, socket, client, group, tap).await;

    status_task.abort();
    peers_task.abort();
    result.map_err(|error| anyhow!("{error}"))
}

fn participant_list(peers: &[PeerInfo]) -> Vec<PeerView> {
    peers
        .iter()
        .filter_map(|peer| {
            let key = parse_peer_id(&peer.peer_id).ok()?;
            Some(PeerView {
                name: peer.name.clone().unwrap_or_else(|| "peer".into()),
                ip: mesh_ip(&key).to_string(),
                tag: peer_tag(&key),
            })
        })
        .collect()
}

pub fn user_identity_path() -> PathBuf {
    #[cfg(unix)]
    {
        if let Ok(user) = std::env::var("SUDO_USER") {
            if !user.is_empty() && user != "root" {
                if let Some(home) = home_for_account(&user) {
                    return home.join(".config/nuvia/identity.toml");
                }
            }
        }
        if let Ok(uid) = std::env::var("PKEXEC_UID") {
            if let Some(home) = home_for_account(&uid) {
                return home.join(".config/nuvia/identity.toml");
            }
        }
    }
    default_identity_path()
}

#[cfg(unix)]
fn home_for_account(account: &str) -> Option<PathBuf> {
    let output = std::process::Command::new("getent")
        .arg("passwd")
        .arg(account)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let home = text.trim().split(':').nth(5)?;
    if home.is_empty() {
        None
    } else {
        Some(PathBuf::from(home))
    }
}

pub fn is_elevated() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(windows)]
    {
        unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
    }
    #[cfg(not(any(unix, windows)))]
    {
        true
    }
}

fn parse_peer_id(value: &str) -> Result<[u8; 32]> {
    let bytes = hex::decode(value.trim()).context("peer id inválido")?;
    if bytes.len() != 32 {
        bail!("peer id deve ter 32 bytes");
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/proc/sys/kernel/hostname").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
