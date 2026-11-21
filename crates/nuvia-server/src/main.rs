use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::{Parser, Subcommand};
use nuvia_protocol::{
    decode_relay, hex_string, parse_hex, peer_tag, Hello, HelloAck, PeerInfo, PublicRoom,
    PublicRoomsResponse, RegisterRequest, RegisterResponse, RelayPacket, BROADCAST_TAG,
    DEFAULT_MAX_PEERS, MAX_PEERS_PER_ROOM, PEER_ID_LEN, PROTOCOL_VERSION, ROOM_ID_LEN, TOKEN_LEN,
};
use serde::Deserialize;
use serde_json::json;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
struct Config {
    /// HTTP API bind. In production Caddy terminates TLS and proxies here.
    http_bind: String,
    /// Public UDP port for endpoint discovery and, later, relay.
    udp_bind: String,
    /// Public URL, only used in logs and health output.
    public_url: String,
    peer_ttl_secs: u64,
    room_ttl_secs: u64,
    /// Hard cap for any room. A client may request a lower per-room limit.
    max_peers_per_room: usize,
    max_rooms: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            http_bind: "127.0.0.1:4646".into(),
            udp_bind: "0.0.0.0:8443".into(),
            public_url: String::new(),
            peer_ttl_secs: 60,
            room_ttl_secs: 86_400,
            max_peers_per_room: MAX_PEERS_PER_ROOM as usize,
            max_rooms: 1024,
        }
    }
}

#[derive(Debug)]
struct Peer {
    endpoint: Option<SocketAddr>,
    local_endpoints: Vec<SocketAddr>,
    name: Option<String>,
    last_seen: Instant,
}

#[derive(Debug)]
struct Room {
    token_hash: [u8; 32],
    created: Instant,
    peers: HashMap<[u8; PEER_ID_LEN], Peer>,
    public: bool,
    has_password: bool,
    name: Option<String>,
    /// Capacity this room was created with, always `<=` the server cap.
    max_peers: usize,
}

struct AppState {
    rooms: RwLock<HashMap<[u8; ROOM_ID_LEN], Room>>,
    config: Config,
}

#[derive(Parser)]
#[command(
    name = "nuvia-server",
    version,
    about = "nuvia rendezvous server (M5a: discovery and direct internet)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        http_bind: Option<String>,
        #[arg(long)]
        udp_bind: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    match cli.command {
        Command::Serve {
            config,
            http_bind,
            udp_bind,
        } => serve(config.as_deref(), http_bind, udp_bind).await,
    }
}

async fn serve(
    config_path: Option<&Path>,
    http_bind_override: Option<String>,
    udp_bind_override: Option<String>,
) -> Result<()> {
    let mut config = load_config(config_path)?;
    if let Some(value) = http_bind_override {
        config.http_bind = value;
    }
    if let Some(value) = udp_bind_override {
        config.udp_bind = value;
    }

    let state = Arc::new(AppState {
        rooms: RwLock::new(HashMap::new()),
        config: config.clone(),
    });

    let udp = UdpSocket::bind(&config.udp_bind)
        .await
        .with_context(|| format!("binding UDP {}", config.udp_bind))?;
    tokio::spawn(udp_loop(state.clone(), udp));
    tokio::spawn(cleanup_loop(state.clone()));

    let app = Router::new()
        .route("/v1/register", post(register))
        .route("/v1/rooms", get(public_rooms))
        .route("/v1/healthz", get(health))
        .with_state(state);

    let listener = TcpListener::bind(&config.http_bind)
        .await
        .with_context(|| format!("binding HTTP {}", config.http_bind))?;

    info!(
        http = %config.http_bind,
        udp = %config.udp_bind,
        public_url = %config.public_url,
        "nuvia-server listening"
    );

    axum::serve(listener, app).await?;
    Ok(())
}

fn load_config(path: Option<&Path>) -> Result<Config> {
    match path {
        Some(path) if path.exists() => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
        }
        _ => Ok(Config::default()),
    }
}

/// Resolves the capacity of a new room.
///
/// A missing value preserves the legacy default of 8, while an explicit value
/// is clamped to the server's hard cap. The GUI omits the field, so it keeps
/// creating rooms with 8; the CLI can request up to 32.
fn room_max_peers(requested: Option<u16>, cap: usize) -> usize {
    requested
        .map(usize::from)
        .unwrap_or(DEFAULT_MAX_PEERS as usize)
        .clamp(2, cap.max(2))
}

async fn register(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, (StatusCode, String)> {
    if request.protocol != PROTOCOL_VERSION {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("unsupported protocol {}", request.protocol),
        ));
    }

    let room_id = parse_hex::<ROOM_ID_LEN>(&request.room_id)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("room_id: {e}")))?;
    let peer_id = parse_hex::<PEER_ID_LEN>(&request.peer_id)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("peer_id: {e}")))?;
    let token = parse_hex::<TOKEN_LEN>(&request.token)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("token: {e}")))?;
    let token_hash = *blake3::hash(&token).as_bytes();

    let mut rooms = state.rooms.write().await;

    if !rooms.contains_key(&room_id) {
        if !request.create {
            return Err((StatusCode::NOT_FOUND, "room not found".into()));
        }
        if rooms.len() >= state.config.max_rooms {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "server room limit reached".into(),
            ));
        }
        let room_name = request.room_name.as_deref().and_then(sanitize_room_name);
        if request.public && room_name.is_none() {
            return Err((StatusCode::BAD_REQUEST, "public room needs a name".into()));
        }
        if request.public
            && room_name
                .as_ref()
                .is_some_and(|name| name.chars().count() > 64)
        {
            return Err((StatusCode::BAD_REQUEST, "room name is too long".into()));
        }
        let max_peers = room_max_peers(request.max_peers, state.config.max_peers_per_room);
        rooms.insert(
            room_id,
            Room {
                token_hash,
                created: Instant::now(),
                peers: HashMap::new(),
                public: request.public && room_name.is_some(),
                has_password: request.has_password,
                name: room_name,
                max_peers,
            },
        );
        info!(room = %hex_string(&room_id), max_peers, "room created");
    }

    let room = rooms.get_mut(&room_id).expect("room inserted above");
    if room.token_hash != token_hash {
        return Err((StatusCode::FORBIDDEN, "invalid room token".into()));
    }

    let now = Instant::now();
    if !room.peers.contains_key(&peer_id) && room.peers.len() >= room.max_peers {
        return Err((StatusCode::CONFLICT, "room is full".into()));
    }

    let peer = room.peers.entry(peer_id).or_insert_with(|| Peer {
        endpoint: None,
        local_endpoints: Vec::new(),
        name: None,
        last_seen: now,
    });
    peer.name = request.name.clone();
    peer.local_endpoints = request
        .local_endpoints
        .iter()
        .filter_map(|value| value.parse::<SocketAddr>().ok())
        .filter(|endpoint| {
            endpoint.is_ipv4() && !endpoint.ip().is_loopback() && !endpoint.ip().is_unspecified()
        })
        .take(8)
        .collect();
    peer.last_seen = now;

    let self_endpoint = peer.endpoint.map(|value| value.to_string());
    let peers = room
        .peers
        .iter()
        .filter(|(id, _)| **id != peer_id)
        .filter_map(|(id, peer)| {
            peer.endpoint.map(|endpoint| PeerInfo {
                peer_id: hex_string(id),
                endpoint: Some(endpoint.to_string()),
                local_endpoints: peer
                    .local_endpoints
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                name: peer.name.clone(),
            })
        })
        .collect::<Vec<_>>();

    debug!(
        room = %hex_string(&room_id),
        peer = %hex_string(&peer_id),
        peers = peers.len(),
        "register"
    );

    Ok(Json(RegisterResponse {
        protocol: PROTOCOL_VERSION,
        room_id: hex_string(&room_id),
        self_endpoint,
        peers,
        ttl_secs: state.config.peer_ttl_secs,
    }))
}

async fn public_rooms(State(state): State<Arc<AppState>>) -> Json<PublicRoomsResponse> {
    let now = Instant::now();
    let peer_ttl = Duration::from_secs(state.config.peer_ttl_secs);
    let rooms = state.rooms.read().await;
    let mut public: Vec<PublicRoom> = rooms
        .values()
        .filter(|room| room.public)
        .filter_map(|room| {
            let name = room.name.as_ref()?;
            let peers = room
                .peers
                .values()
                .filter(|peer| now.duration_since(peer.last_seen) < peer_ttl)
                .count();
            (peers > 0).then(|| PublicRoom {
                name: name.clone(),
                has_password: room.has_password,
                peers,
                max_peers: room.max_peers,
            })
        })
        .collect();
    public.sort_by(|left, right| {
        right
            .peers
            .cmp(&left.peers)
            .then_with(|| left.name.cmp(&right.name))
    });
    public.truncate(100);
    Json(PublicRoomsResponse {
        protocol: PROTOCOL_VERSION,
        rooms: public,
    })
}

fn sanitize_room_name(value: &str) -> Option<String> {
    let name: String = value.trim().chars().filter(|ch| !ch.is_control()).collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let rooms = state.rooms.read().await;
    let peers: usize = rooms.values().map(|room| room.peers.len()).sum();
    let public = rooms.values().filter(|room| room.public).count();
    Json(json!({
        "status": "ok",
        "protocol": PROTOCOL_VERSION,
        "rooms": rooms.len(),
        "public_rooms": public,
        "peers": peers,
        "public_url": state.config.public_url,
    }))
}

async fn udp_loop(state: Arc<AppState>, socket: UdpSocket) {
    let mut buffer = [0u8; 2048];
    loop {
        match socket.recv_from(&mut buffer).await {
            Ok((length, source)) => {
                if let Ok(hello) = Hello::decode(&buffer[..length]) {
                    let ack = handle_hello(&state, hello, source).await;
                    if let Err(error) = socket.send_to(&ack.encode(), source).await {
                        warn!(%source, %error, "could not send hello ack");
                    }
                    continue;
                }

                if let Ok(relay) = decode_relay(&buffer[..length]) {
                    if relay.inner.len() > 1600 {
                        debug!(%source, length, "ignoring oversized relay packet");
                        continue;
                    }
                    let destinations = handle_relay(&state, &relay, source).await;
                    if destinations.is_empty() {
                        debug!(%source, "relay packet dropped");
                    }
                    for (inner, destination) in destinations {
                        if let Err(error) = socket.send_to(&inner, destination).await {
                            warn!(%destination, %error, "could not forward relay packet");
                        }
                    }
                    continue;
                }

                debug!(%source, length, "ignoring unknown UDP packet");
            }
            Err(error) => {
                warn!(%error, "UDP receive failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn handle_hello(state: &AppState, hello: Hello, source: SocketAddr) -> HelloAck {
    let token_hash = *blake3::hash(&hello.token).as_bytes();
    let mut rooms = state.rooms.write().await;

    let Some(room) = rooms.get_mut(&hello.room_id) else {
        return HelloAck::room_not_found();
    };
    if room.token_hash != token_hash {
        return HelloAck::invalid_token();
    }

    if !room.peers.contains_key(&hello.peer_id) && room.peers.len() >= room.max_peers {
        return HelloAck::room_full();
    }

    let now = Instant::now();
    let peer = room.peers.entry(hello.peer_id).or_insert_with(|| Peer {
        endpoint: None,
        local_endpoints: Vec::new(),
        name: None,
        last_seen: now,
    });
    peer.endpoint = Some(source);
    peer.last_seen = now;

    debug!(
        room = %hex_string(&hello.room_id),
        peer = %hex_string(&hello.peer_id),
        %source,
        "hello"
    );
    HelloAck::ok()
}

async fn handle_relay(
    state: &AppState,
    relay: &RelayPacket<'_>,
    source: SocketAddr,
) -> Vec<(Vec<u8>, SocketAddr)> {
    let token_hash = *blake3::hash(&relay.token).as_bytes();
    let mut rooms = state.rooms.write().await;

    let Some(room) = rooms.get_mut(&relay.room_id) else {
        return Vec::new();
    };
    if room.token_hash != token_hash || relay.dst == relay.src {
        return Vec::new();
    }

    let Some(src_id) = room
        .peers
        .keys()
        .find(|id| peer_tag(id) == relay.src)
        .copied()
    else {
        return Vec::new();
    };

    let now = Instant::now();
    if let Some(peer) = room.peers.get_mut(&src_id) {
        peer.endpoint = Some(source);
        peer.last_seen = now;
    }

    if relay.dst == BROADCAST_TAG {
        let destinations = room
            .peers
            .iter()
            .filter(|(id, _)| **id != src_id)
            .filter_map(|(_, peer)| peer.endpoint)
            .collect::<Vec<_>>();
        debug!(
            room = %hex_string(&relay.room_id),
            src = %hex_string(&relay.src),
            peers = destinations.len(),
            bytes = relay.inner.len(),
            "relay broadcast"
        );
        return destinations
            .into_iter()
            .map(|endpoint| (relay.inner.to_vec(), endpoint))
            .collect();
    }

    let Some(dst_endpoint) = room.peers.iter().find_map(|(id, peer)| {
        if peer_tag(id) == relay.dst {
            peer.endpoint
        } else {
            None
        }
    }) else {
        return Vec::new();
    };

    debug!(
        room = %hex_string(&relay.room_id),
        src = %hex_string(&relay.src),
        dst = %hex_string(&relay.dst),
        %dst_endpoint,
        bytes = relay.inner.len(),
        "relay"
    );

    vec![(relay.inner.to_vec(), dst_endpoint)]
}

async fn cleanup_loop(state: Arc<AppState>) {
    let peer_ttl = Duration::from_secs(state.config.peer_ttl_secs);
    let room_ttl = Duration::from_secs(state.config.room_ttl_secs);

    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        let now = Instant::now();
        let mut rooms = state.rooms.write().await;
        rooms.retain(|_, room| {
            room.peers
                .retain(|_, peer| now.duration_since(peer.last_seen) < peer_ttl);
            !room.peers.is_empty() || now.duration_since(room.created) < room_ttl
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_names_are_trimmed_and_sanitized() {
        assert_eq!(sanitize_room_name("  casa  ").as_deref(), Some("casa"));
        assert_eq!(sanitize_room_name("a\nb").as_deref(), Some("ab"));
        assert_eq!(sanitize_room_name("   "), None);
        assert_eq!(sanitize_room_name(&"x".repeat(80)).unwrap().len(), 80);
    }

    #[test]
    fn room_capacity_keeps_the_legacy_default_and_clamps() {
        assert_eq!(room_max_peers(None, 32), 8);
        assert_eq!(room_max_peers(Some(16), 32), 16);
        assert_eq!(room_max_peers(Some(32), 32), 32);
        assert_eq!(room_max_peers(Some(64), 32), 32);
        assert_eq!(room_max_peers(Some(1), 32), 2);
        assert_eq!(room_max_peers(Some(16), 8), 8);
    }
}
