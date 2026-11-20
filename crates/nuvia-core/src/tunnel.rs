use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot};
use tokio::time::MissedTickBehavior;
use tracing::{debug, info, warn};
use tun_rs::{AsyncDevice, DeviceBuilder, Layer};

use crate::mesh::GroupSession;
use crate::noise::{Handshake, Role};
use crate::rendezvous::RendezvousClient;
use crate::wire::{packet_type, Session, MAGIC, MAX_PACKET, TYPE_DATA, TYPE_HANDSHAKE, TYPE_PUNCH};
use crate::{Error, Identity, Result, RoomKey};
use nuvia_protocol::{encode_relay, PeerInfo, BROADCAST_TAG, PEER_TAG_LEN};

#[derive(Debug, Clone)]
pub struct TapConfig {
    pub name: String,
    pub ipv4: Ipv4Addr,
    pub prefix: u8,
    pub mtu: u16,
}

#[derive(Debug, Clone)]
pub struct TunnelConfig {
    pub bind_addr: SocketAddr,
    pub peer_addr: SocketAddr,
    pub peer_public: [u8; 32],
    pub tap: TapConfig,
    pub keepalive: Duration,
    pub idle_timeout: Duration,
    /// Route IPv4 multicast and limited broadcast through the TAP so games
    /// join their discovery groups on the virtual interface.
    pub broadcast_routes: bool,
    /// Optional notification used by the auto-fallback logic and the GUI.
    pub status: Option<tokio::sync::mpsc::Sender<TunnelStatus>>,
}

/// One room with more than two peers: a single TAP and UDP socket, flooded
/// through the relay with a room-wide group key.
pub struct MeshConfig {
    pub bind_addr: SocketAddr,
    pub server_addr: SocketAddr,
    pub direct_peers: Vec<SocketAddr>,
    pub room_id: [u8; 16],
    pub token: [u8; 16],
    pub status: Option<tokio::sync::mpsc::Sender<TunnelStatus>>,
    pub peers: Option<tokio::sync::mpsc::Sender<Vec<PeerInfo>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelStatus {
    Established,
    RelayEstablished,
    /// Path used by one specific room member (`true` = direct, `false` =
    /// relay). Sent when the path changes and periodically while the peer is
    /// alive so the GUI can tell active peers from stale entries.
    Peer {
        tag: [u8; PEER_TAG_LEN],
        direct: bool,
    },
    /// Round-trip time measured with an encrypted control frame.
    Ping {
        tag: [u8; PEER_TAG_LEN],
        ms: u32,
    },
}

pub const MESH_PREFIX: u8 = 16;

const PING_ETHERTYPE: [u8; 2] = [0x88, 0xB5];
const PING_MAGIC: [u8; 4] = *b"NUVP";
const PING_VERSION: u8 = 1;
const PING_REQUEST: u8 = 1;
const PING_REPLY: u8 = 2;
const PING_FRAME_LEN: usize = 28;

fn ping_frame(kind: u8, nonce: u64) -> Vec<u8> {
    let mut frame = vec![0u8; PING_FRAME_LEN];
    frame[..6].fill(0xff);
    frame[6..12].copy_from_slice(&[0x02, 0, 0, 0, 0, 1]);
    frame[12..14].copy_from_slice(&PING_ETHERTYPE);
    frame[14..18].copy_from_slice(&PING_MAGIC);
    frame[18] = PING_VERSION;
    frame[19] = kind;
    frame[20..28].copy_from_slice(&nonce.to_be_bytes());
    frame
}

fn parse_ping(frame: &[u8]) -> Option<(u8, u64)> {
    if frame.len() != PING_FRAME_LEN
        || frame[12..14] != PING_ETHERTYPE
        || frame[14..18] != PING_MAGIC
        || frame[18] != PING_VERSION
    {
        return None;
    }
    let kind = frame[19];
    if kind != PING_REQUEST && kind != PING_REPLY {
        return None;
    }
    Some((kind, u64::from_be_bytes(frame[20..28].try_into().ok()?)))
}

pub fn shared_tap_name() -> &'static str {
    if cfg!(windows) {
        "nuvia0"
    } else {
        "nuvia-vpn0"
    }
}

/// Frames exchanged between a room session and the single shared TAP.
pub struct TapIo {
    pub outbound: mpsc::Receiver<Vec<u8>>,
    pub inbound: mpsc::Sender<Vec<u8>>,
}

#[derive(Clone)]
pub struct TapHub {
    tx: mpsc::Sender<HubCommand>,
    inbound: mpsc::Sender<Vec<u8>>,
}

enum HubCommand {
    Register {
        room: String,
        ip: Ipv4Addr,
        outbound: mpsc::Sender<Vec<u8>>,
        reply: oneshot::Sender<Result<()>>,
    },
}

impl TapHub {
    pub fn spawn(tap_name: impl Into<String>, mtu: u16) -> Self {
        let (tx, commands) = mpsc::channel(64);
        let (inbound, inbound_rx) = mpsc::channel(512);
        tokio::spawn(hub_loop(tap_name.into(), mtu, commands, inbound_rx));
        Self { tx, inbound }
    }

    pub async fn register(&self, room: String, ip: Ipv4Addr) -> Result<TapIo> {
        let (outbound_tx, outbound) = mpsc::channel(512);
        let (reply, reply_rx) = oneshot::channel();
        self.tx
            .send(HubCommand::Register {
                room,
                ip,
                outbound: outbound_tx,
                reply,
            })
            .await
            .map_err(|_| Error::config("shared TAP hub stopped"))?;
        reply_rx
            .await
            .map_err(|_| Error::config("shared TAP hub dropped the request"))??;
        Ok(TapIo {
            outbound,
            inbound: self.inbound.clone(),
        })
    }
}

enum TapEvent {
    Frame(Vec<u8>),
    Closed,
}

async fn hub_loop(
    tap_name: String,
    mtu: u16,
    mut commands: mpsc::Receiver<HubCommand>,
    mut inbound: mpsc::Receiver<Vec<u8>>,
) {
    let mut tap: Option<Arc<AsyncDevice>> = None;
    let mut tap_reader: Option<tokio::task::JoinHandle<()>> = None;
    let mut rooms: HashMap<String, mpsc::Sender<Vec<u8>>> = HashMap::new();
    let (tap_tx, mut tap_rx) = mpsc::channel::<TapEvent>(512);

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                match command {
                    HubCommand::Register { room, ip, outbound, reply } => {
                        if tap.is_none() {
                            let config = TapConfig {
                                name: tap_name.clone(),
                                ipv4: ip,
                                prefix: MESH_PREFIX,
                                mtu,
                            };
                            match open_tap(&config).await {
                                Ok(device) => {
                                    #[cfg(windows)]
                                    configure_windows_interface(&tap_name);
                                    let device = Arc::new(device);
                                    tap = Some(device.clone());
                                    let reader = device.clone();
                                    let sender = tap_tx.clone();
                                    tap_reader = Some(tokio::spawn(async move {
                                        let mut buffer = vec![0u8; MAX_PACKET];
                                        loop {
                                            match reader.recv(&mut buffer).await {
                                                Ok(0) => continue,
                                                Ok(length) => {
                                                    if sender
                                                        .send(TapEvent::Frame(buffer[..length].to_vec()))
                                                        .await
                                                        .is_err()
                                                    {
                                                        break;
                                                    }
                                                }
                                                Err(error) => {
                                                    warn!(%error, "shared TAP read failed");
                                                    break;
                                                }
                                            }
                                        }
                                        let _ = sender.send(TapEvent::Closed).await;
                                    }));
                                    info!(tap = %tap_name, ip = %ip, mtu, "shared TAP ready");
                                }
                                Err(error) => {
                                    let _ = reply.send(Err(error));
                                    continue;
                                }
                            }
                        }
                        rooms.insert(room, outbound);
                        let _ = reply.send(Ok(()));
                    }
                }
            }
            Some(frame) = inbound.recv() => {
                if let Some(device) = &tap {
                    match tokio::time::timeout(Duration::from_secs(2), device.send(&frame)).await {
                        Ok(Ok(_)) => {}
                        Ok(Err(error)) => {
                            warn!(%error, "shared TAP write failed");
                            tap = None;
                            if let Some(reader) = tap_reader.take() {
                                reader.abort();
                            }
                        }
                        Err(_) => {
                            warn!("shared TAP write timed out");
                            tap = None;
                            if let Some(reader) = tap_reader.take() {
                                reader.abort();
                            }
                        }
                    }
                }
            }
            Some(event) = tap_rx.recv() => {
                match event {
                    TapEvent::Frame(frame) => {
                        let mut closed = Vec::new();
                        for (room, sender) in &rooms {
                            if let Err(TrySendError::Closed(_)) = sender.try_send(frame.clone()) {
                                closed.push(room.clone());
                            }
                        }
                        for room in closed {
                            rooms.remove(&room);
                        }
                    }
                    TapEvent::Closed => {
                        warn!("shared TAP closed");
                        tap = None;
                        tap_reader = None;
                    }
                }
            }
        }
    }
}

impl TunnelConfig {
    pub fn new(
        bind_addr: SocketAddr,
        peer_addr: SocketAddr,
        peer_public: [u8; 32],
        tap: TapConfig,
    ) -> Self {
        Self {
            bind_addr,
            peer_addr,
            peer_public,
            tap,
            keepalive: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(30),
            broadcast_routes: true,
            status: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RelayConfig {
    pub server_addr: SocketAddr,
    pub room_id: [u8; 16],
    pub token: [u8; 16],
    pub self_tag: [u8; 8],
    pub peer_tag: [u8; 8],
}

/// Runs the tunnel until cancelled by the caller (for example on Ctrl-C).
///
/// Re-establishes the encrypted session automatically when the peer restarts
/// or the link goes silent.
pub async fn run(config: TunnelConfig, identity: &Identity, room: &RoomKey) -> Result<()> {
    let socket = UdpSocket::bind(config.bind_addr).await?;
    socket.connect(config.peer_addr).await?;
    run_with_socket(config, identity, room, socket).await
}

/// Same as [`run`], but with a UDP socket that the caller already bound (and
/// connected to the peer). The rendezvous client uses this so its public
/// endpoint stays the same for discovery and for the tunnel.
pub async fn run_with_socket(
    config: TunnelConfig,
    identity: &Identity,
    room: &RoomKey,
    socket: UdpSocket,
) -> Result<()> {
    let tap = open_tap(&config.tap).await?;
    #[cfg(windows)]
    configure_windows_interface(&config.tap.name);
    let _routes = if config.broadcast_routes {
        Some(RouteGuard::install(&config.tap.name))
    } else {
        None
    };

    let role = Role::for_peers(identity.public_key(), &config.peer_public);
    info!(
        tap = %config.tap.name,
        ip = %config.tap.ipv4,
        mtu = config.tap.mtu,
        peer = %config.peer_addr,
        peer_key = %hex::encode(config.peer_public),
        role = ?role,
        "tunnel ready"
    );

    loop {
        debug!("starting handshake");
        let session =
            match establish(&socket, role, identity, &config.peer_public, room.psk()).await {
                Ok(session) => session,
                Err(error) => {
                    warn!("handshake failed: {error}");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
            };

        info!(session = %format!("{:016x}", session.session_id()), "session established");
        if let Some(status) = &config.status {
            let _ = status.send(TunnelStatus::Established).await;
        }

        match data_loop(
            &tap,
            &socket,
            session,
            config.keepalive,
            config.idle_timeout,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(error) => {
                warn!("session ended: {error}");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// Relay variant of [`run_with_socket`]. The socket is bound but **not**
/// connected: every packet is wrapped in a relay envelope and sent to the
/// rendezvous server, which forwards the inner bytes to the peer.
pub async fn run_relay_with_socket(
    config: TunnelConfig,
    identity: &Identity,
    room: &RoomKey,
    socket: UdpSocket,
    relay: RelayConfig,
) -> Result<()> {
    let tap = open_tap(&config.tap).await?;
    #[cfg(windows)]
    configure_windows_interface(&config.tap.name);
    let _routes = if config.broadcast_routes {
        Some(RouteGuard::install(&config.tap.name))
    } else {
        None
    };

    let role = Role::for_peers(identity.public_key(), &config.peer_public);
    info!(
        tap = %config.tap.name,
        ip = %config.tap.ipv4,
        mtu = config.tap.mtu,
        relay = %relay.server_addr,
        peer_key = %hex::encode(config.peer_public),
        role = ?role,
        "tunnel ready (relay)"
    );

    loop {
        debug!("starting relay handshake");
        let session = match establish_relay(
            &socket,
            role,
            identity,
            &config.peer_public,
            room.psk(),
            &relay,
        )
        .await
        {
            Ok(session) => session,
            Err(error) => {
                warn!("relay handshake failed: {error}");
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };

        info!(
            session = %format!("{:016x}", session.session_id()),
            "session established (relay)"
        );
        if let Some(status) = &config.status {
            let _ = status.send(TunnelStatus::RelayEstablished).await;
        }

        match data_loop_relay(
            &tap,
            &socket,
            session,
            &relay,
            config.keepalive,
            config.idle_timeout,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(error) => {
                warn!("relay session ended: {error}");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// Tries the direct path for `direct_timeout`, then recreates the socket and
/// falls back to the relay. Used by the CLI/GUI for internet mode.
pub async fn run_auto(
    config: TunnelConfig,
    identity: &Identity,
    room: &RoomKey,
    socket: UdpSocket,
    relay: RelayConfig,
    direct_timeout: Duration,
) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let mut direct_config = config.clone();
    direct_config.status = Some(tx);

    let mut direct = Box::pin(run_with_socket(direct_config, identity, room, socket));
    let mut direct_established = false;

    tokio::select! {
        result = &mut direct => {
            match result {
                Ok(()) => return Ok(()),
                Err(error) => warn!("direct tunnel failed: {error}"),
            }
        }
        status = rx.recv() => {
            if status == Some(TunnelStatus::Established) {
                direct_established = true;
            } else {
                warn!("direct tunnel closed before establishing");
            }
        }
        _ = tokio::time::sleep(direct_timeout) => {
            info!(?direct_timeout, "direct path timed out; falling back to relay");
        }
    }

    if direct_established {
        if let Some(status) = &config.status {
            let _ = status.send(TunnelStatus::Established).await;
        }
        return direct.await;
    }

    drop(direct);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let socket = UdpSocket::bind(config.bind_addr).await?;
    info!("starting relay fallback");
    run_relay_with_socket(config, identity, room, socket, relay).await
}

/// Runs a room-wide L2 mesh. Every frame read from the TAP is encrypted with
/// the room group key and flooded by the relay to all other peers. The TAP and
/// virtual IP stay the same while players join or leave.
pub async fn run_mesh(
    config: MeshConfig,
    socket: UdpSocket,
    rendezvous: RendezvousClient,
    mut group: GroupSession,
    tap: TapIo,
) -> Result<()> {
    let mut outbound = tap.outbound;
    let inbound = tap.inbound;
    let mut direct_peers = config.direct_peers;
    let mut direct_announced = false;
    // Last known path for each room member. The timestamp lets the GUI mark a
    // peer as offline when its keepalives stop.
    let mut peer_paths: HashMap<[u8; PEER_TAG_LEN], (bool, Instant)> = HashMap::new();
    // Direct source address learned from a peer, used to answer pings on the
    // low-latency path.
    let mut peer_sources: HashMap<[u8; PEER_TAG_LEN], SocketAddr> = HashMap::new();
    let mut pending_pings: HashMap<u64, Instant> = HashMap::new();

    info!(
        server = %config.server_addr,
        direct_peers = direct_peers.len(),
        endpoints = ?direct_peers,
        "mesh ready"
    );
    let mut udp_buffer = vec![0u8; MAX_PACKET];
    let mut keepalive = tokio::time::interval(Duration::from_secs(10));
    keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut ping = tokio::time::interval(Duration::from_secs(2));
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut refresh = tokio::time::interval(Duration::from_secs(5));
    refresh.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut fallback = Box::pin(tokio::time::sleep(Duration::from_secs(8)));
    let mut fallback_sent = false;

    loop {
        tokio::select! {
            _ = &mut fallback, if !fallback_sent => {
                fallback_sent = true;
                if !direct_announced {
                    if let Some(status) = &config.status {
                        let _ = status.send(TunnelStatus::RelayEstablished).await;
                    }
                }
            }
            Some(frame) = outbound.recv() => {
                let inner = group.seal(&frame)?;
                for peer in &direct_peers {
                    if let Err(error) = socket.send_to(&inner, peer).await {
                        debug!(%peer, %error, "could not send direct mesh frame");
                    }
                }
                let envelope = encode_relay(
                    &config.room_id,
                    group.self_tag(),
                    &BROADCAST_TAG,
                    &config.token,
                    &inner,
                );
                if let Err(error) = socket.send_to(&envelope, config.server_addr).await {
                    warn!(%error, "could not send mesh frame");
                }
            }
            result = socket.recv_from(&mut udp_buffer) => {
                let (length, source) = result?;
                match group.open(&udp_buffer[..length]) {
                    Ok((src, frame)) => {
                        let direct = source != config.server_addr;
                        if direct {
                            peer_sources.insert(src, source);
                        }
                        let now = Instant::now();
                        let announce = match peer_paths.get_mut(&src) {
                            Some((previous, last_seen)) => {
                                // Refresh active peers even when the path did
                                // not change so they never look stale.
                                let announce = *previous != direct
                                    || last_seen.elapsed() >= Duration::from_secs(5);
                                *previous = direct;
                                *last_seen = now;
                                announce
                            }
                            None => {
                                peer_paths.insert(src, (direct, now));
                                true
                            }
                        };
                        if announce {
                            if let Some(status) = &config.status {
                                let _ = status
                                    .send(TunnelStatus::Peer { tag: src, direct })
                                    .await;
                            }
                        }
                        if direct && !direct_announced {
                            direct_announced = true;
                            if let Some(status) = &config.status {
                                let _ = status.send(TunnelStatus::Established).await;
                            }
                        }

                        if let Some((kind, nonce)) = parse_ping(&frame) {
                            if kind == PING_REQUEST {
                                let reply = group.seal(&ping_frame(PING_REPLY, nonce))?;
                                if direct {
                                    let _ = socket.send_to(&reply, source).await;
                                }
                                if let Some(endpoint) = peer_sources.get(&src) {
                                    if *endpoint != source {
                                        let _ = socket.send_to(&reply, *endpoint).await;
                                    }
                                }
                                let envelope = encode_relay(
                                    &config.room_id,
                                    group.self_tag(),
                                    &BROADCAST_TAG,
                                    &config.token,
                                    &reply,
                                );
                                let _ = socket.send_to(&envelope, config.server_addr).await;
                            } else if let Some(sent) = pending_pings.remove(&nonce) {
                                let ms = sent.elapsed().as_millis().min(u32::MAX as u128) as u32;
                                if let Some(status) = &config.status {
                                    let _ = status
                                        .send(TunnelStatus::Ping { tag: src, ms })
                                        .await;
                                }
                            }
                        } else if !frame.is_empty() {
                            inbound.send(frame).await.map_err(|_| Error::config("shared TAP hub stopped"))?;
                        }
                    }
                    Err(error) => debug!(%error, "mesh frame rejected"),
                }
            }
            _ = keepalive.tick() => {
                let inner = group.seal(&[])?;
                for peer in &direct_peers {
                    let _ = socket.send_to(&inner, peer).await;
                }
                let envelope = encode_relay(
                    &config.room_id,
                    group.self_tag(),
                    &BROADCAST_TAG,
                    &config.token,
                    &inner,
                );
                if let Err(error) = socket.send_to(&envelope, config.server_addr).await {
                    warn!(%error, "could not send mesh keepalive");
                }
            }
            _ = ping.tick() => {
                pending_pings.retain(|_, sent| sent.elapsed() < Duration::from_secs(10));
                let nonce = rand::random::<u64>();
                pending_pings.insert(nonce, Instant::now());
                let inner = group.seal(&ping_frame(PING_REQUEST, nonce))?;
                for peer in &direct_peers {
                    let _ = socket.send_to(&inner, peer).await;
                }
                let envelope = encode_relay(
                    &config.room_id,
                    group.self_tag(),
                    &BROADCAST_TAG,
                    &config.token,
                    &inner,
                );
                if let Err(error) = socket.send_to(&envelope, config.server_addr).await {
                    debug!(%error, "could not send mesh ping");
                }
            }
            _ = refresh.tick() => {
                match rendezvous.register().await {
                    Ok(response) => {
                        let refreshed = crate::rendezvous::peer_endpoints(&response.peers);
                        if refreshed != direct_peers {
                            direct_peers = refreshed;
                            info!(endpoints = ?direct_peers, "mesh direct endpoints updated");
                            // Announce ourselves to the new endpoints right
                            // away instead of waiting for the keepalive tick.
                            let inner = group.seal(&[])?;
                            for peer in &direct_peers {
                                let _ = socket.send_to(&inner, peer).await;
                            }
                        }
                        if let Some(peers) = &config.peers {
                            let _ = peers.send(response.peers).await;
                        }
                    }
                    Err(error) => warn!(%error, "mesh rendezvous refresh failed"),
                }
            }
        }
    }
}

async fn open_tap(config: &TapConfig) -> Result<AsyncDevice> {
    #[cfg(windows)]
    let device = match build_tap(config) {
        Ok(device) => device,
        Err(first_error) => {
            warn!(%first_error, "TAP open failed; trying to repair the adapter");
            match repair_windows_tap(&config.name) {
                Ok(()) => {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    build_tap(config).map_err(|error| {
                        Error::config(format!(
                            "TAP indisponível ({error}). O driver foi instalado, mas o \
                             adaptador nuvia0 não abriu. Rode reset-tap.ps1 e \
                             install-tap.ps1 -DriverDir .\\driver -Force."
                        ))
                    })?
                }
                Err(repair_error) => {
                    return Err(Error::config(format!(
                        "TAP indisponível ({first_error}). Reparo automático falhou: \
                         {repair_error}. Rode install-tap.ps1 como administrador."
                    )))
                }
            }
        }
    };

    #[cfg(not(windows))]
    let device = build_tap(config)?;

    #[cfg(windows)]
    if let Ok(actual) = device.name() {
        if actual != config.name {
            info!(from = %actual, to = %config.name, "renaming TAP adapter");
            rename_windows_adapter(&actual, &config.name)?;
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    Ok(device)
}

#[cfg(windows)]
fn build_tap(config: &TapConfig) -> std::io::Result<AsyncDevice> {
    DeviceBuilder::new()
        .name(&config.name)
        .layer(Layer::L2)
        .ipv4(config.ipv4, config.prefix, None)
        .mtu(config.mtu)
        .build_async()
}

#[cfg(not(windows))]
fn build_tap(config: &TapConfig) -> std::io::Result<AsyncDevice> {
    DeviceBuilder::new()
        .name(&config.name)
        .layer(Layer::L2)
        .ipv4(config.ipv4, config.prefix, None)
        .mtu(config.mtu)
        .build_async()
}

/// Reuses or creates a working adapter called `nuviaN` when the driver is
/// present but no usable device exists (common after removing old adapters).
#[cfg(windows)]
fn repair_windows_tap(name: &str) -> Result<()> {
    if ensure_windows_adapter(name).is_ok() {
        return Ok(());
    }

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.to_path_buf()));
    let Some(exe_dir) = exe_dir else {
        return Err(Error::config("não foi possível localizar a pasta do nuvia"));
    };
    let driver = exe_dir.join("driver");
    let inf = driver.join("OemVista.inf");
    let devcon = driver.join("devcon.exe");
    if !inf.exists() || !devcon.exists() {
        return Err(Error::config(
            r"driver\OemVista.inf ou devcon.exe ausente ao lado do nuvia-gui.exe",
        ));
    }

    let inf = inf.to_string_lossy();
    let devcon = devcon.to_string_lossy();
    let output = run_hidden(&devcon, &["install", &inf, "TAP0901"])
        .map_err(|error| Error::config(format!("executando devcon: {error}")))?;

    std::thread::sleep(Duration::from_secs(1));
    if ensure_windows_adapter(name).is_ok() {
        return Ok(());
    }
    if !output.status.success() {
        return Err(Error::config(format!(
            "devcon falhou: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Err(Error::config(
        "devcon terminou, mas nenhum adaptador nuvia0 apareceu",
    ))
}

#[cfg(windows)]
fn ensure_windows_adapter(name: &str) -> Result<()> {
    let name = name.replace('\'', "''");
    let script = format!(
        "$ErrorActionPreference='Stop'; \
         $target='{name}'; \
         $existing = Get-NetAdapter -IncludeHidden -Name $target -ErrorAction SilentlyContinue; \
         if ($existing) {{ Enable-NetAdapter -Name $target -Confirm:$false -ErrorAction SilentlyContinue; exit 0 }} \
         $tap = Get-NetAdapter -IncludeHidden -ErrorAction SilentlyContinue | \
             Where-Object {{ $_.InterfaceDescription -like '*TAP-Windows*' -and $_.Status -ne 'Up' }} | \
             Select-Object -First 1; \
         if (-not $tap) {{ \
             $tap = Get-NetAdapter -IncludeHidden -ErrorAction SilentlyContinue | \
                 Where-Object {{ $_.InterfaceDescription -like '*TAP-Windows*' }} | \
                 Select-Object -First 1 \
         }} \
         if (-not $tap) {{ exit 2 }} \
         Rename-NetAdapter -Name $tap.Name -NewName $target -ErrorAction Stop; \
         Enable-NetAdapter -Name $target -Confirm:$false -ErrorAction SilentlyContinue; \
         exit 0"
    );
    let output =
        run_powershell(&script).map_err(|error| Error::config(format!("powershell: {error}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::config(format!(
            "sem adaptador TAP utilizável: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

#[cfg(windows)]
fn run_hidden(program: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    std::process::Command::new(program)
        .creation_flags(CREATE_NO_WINDOW)
        .args(args)
        .output()
}

#[cfg(windows)]
fn rename_windows_adapter(from: &str, to: &str) -> Result<()> {
    let from = from.replace('\'', "''");
    let to = to.replace('\'', "''");
    let script = format!("Rename-NetAdapter -Name '{from}' -NewName '{to}' -ErrorAction Stop");
    let output = run_powershell(&script)
        .map_err(|error| Error::config(format!("renomear adaptador TAP: {error}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::config(format!(
            "não foi possível renomear o TAP para {}: {}",
            to,
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

#[cfg(windows)]
fn run_powershell(script: &str) -> std::io::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    std::process::Command::new("powershell.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .output()
}

/// Stable location for the route backup. A crashed session leaves the file
/// behind and the next install restores it before taking a fresh snapshot.
#[cfg(windows)]
fn discovery_backup_path() -> std::path::PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("nuvia").join("discovery-routes.json")
}

/// Adds or removes the three discovery routes on Windows using `New-NetRoute`.
/// Installing temporarily removes competing multicast/broadcast routes from
/// other adapters (saving them to a backup file) so the shared TAP wins. The
/// backup is restored when the session ends. PowerShell exits non-zero when the
/// TAP does not end up as the selected discovery interface, so the failure is
/// visible in `nuvia.log`.
#[cfg(windows)]
fn route_script(
    interface: &str,
    add: bool,
    backup: &std::path::Path,
) -> std::io::Result<std::process::Output> {
    let interface = interface.replace('\'', "''");
    let backup = backup.to_string_lossy().replace('\'', "''");

    let script = if add {
        format!(
            r#"$ErrorActionPreference = 'Stop';
$tap = '{interface}';
$backupPath = '{backup}';
$prefixes = @('224.0.0.0/4', '224.0.2.60/32', '255.255.255.255/32');
$warnings = @();
# Restore routes left behind by a session that ended abruptly.
try {{
  $previous = Get-Content -Raw -Path $backupPath -ErrorAction Stop;
  if ($previous) {{
    foreach ($r in @($previous | ConvertFrom-Json)) {{
      if ($prefixes -notcontains $r.DestinationPrefix) {{ continue }}
      try {{
        New-NetRoute -DestinationPrefix $r.DestinationPrefix `
          -InterfaceAlias $r.InterfaceAlias -NextHop $r.NextHop `
          -RouteMetric $r.RouteMetric -PolicyStore ActiveStore `
          -ErrorAction Stop | Out-Null;
      }} catch {{
        # The route or its adapter is already gone; nothing to restore.
      }}
    }}
  }}
}} catch {{
  # No previous backup.
}}
# Snapshot every competing route before changing anything.
$saved = @();
foreach ($p in $prefixes) {{
  foreach ($r in @(Get-NetRoute -DestinationPrefix $p -ErrorAction SilentlyContinue |
    Where-Object {{ $_.InterfaceAlias -ne $tap }})) {{
    $saved += [pscustomobject]@{{
      DestinationPrefix = $r.DestinationPrefix;
      InterfaceAlias = $r.InterfaceAlias;
      NextHop = $r.NextHop;
      RouteMetric = $r.RouteMetric
    }};
  }}
}}
ConvertTo-Json -InputObject @($saved) -Compress |
  Set-Content -Path $backupPath -Encoding UTF8;
try {{
  Set-NetIPInterface -InterfaceAlias $tap -InterfaceMetric 1 `
    -AddressFamily IPv4 -ErrorAction Stop;
}} catch {{
  $warnings += "interface metric: $($_.Exception.Message)";
}}
foreach ($r in $saved) {{
  try {{
    Remove-NetRoute -DestinationPrefix $r.DestinationPrefix `
      -InterfaceAlias $r.InterfaceAlias -NextHop $r.NextHop `
      -Confirm:$false -ErrorAction Stop | Out-Null;
  }} catch {{
    $warnings += "remove $($r.DestinationPrefix) on $($r.InterfaceAlias): $($_.Exception.Message)";
  }}
}}
foreach ($p in $prefixes) {{
  Remove-NetRoute -DestinationPrefix $p -InterfaceAlias $tap `
    -Confirm:$false -ErrorAction SilentlyContinue | Out-Null;
  try {{
    New-NetRoute -DestinationPrefix $p -InterfaceAlias $tap `
      -RouteMetric 1 -PolicyStore ActiveStore -ErrorAction Stop | Out-Null;
  }} catch {{
    $warnings += "add $p on $tap: $($_.Exception.Message)";
  }}
}}
$missing = @($prefixes | Where-Object {{
  -not (Get-NetRoute -DestinationPrefix $_ -InterfaceAlias $tap `
    -ErrorAction SilentlyContinue)
}});
$selected = @(Find-NetRoute -RemoteIPAddress '224.0.2.60' `
  -ErrorAction SilentlyContinue |
  Select-Object IPAddress,InterfaceAlias,RouteMetric);
ConvertTo-Json -InputObject $selected -Compress;
if ($warnings.Count -gt 0) {{
  [Console]::Error.WriteLine(($warnings -join ' | '));
}}
if ($missing.Count -gt 0) {{
  [Console]::Error.WriteLine("missing routes on $($tap): $($missing -join ', ')");
  exit 1;
}}
if (-not ($selected | Where-Object {{ $_.InterfaceAlias -eq $tap }})) {{
  [Console]::Error.WriteLine("discovery route did not select $tap");
  exit 1;
}}"#
        )
    } else {
        format!(
            r#"$ErrorActionPreference = 'SilentlyContinue';
$tap = '{interface}';
$backupPath = '{backup}';
$prefixes = @('224.0.0.0/4', '224.0.2.60/32', '255.255.255.255/32');
foreach ($p in $prefixes) {{
  Remove-NetRoute -DestinationPrefix $p -InterfaceAlias $tap `
    -Confirm:$false -ErrorAction SilentlyContinue | Out-Null;
}}
try {{ $json = Get-Content -Raw -Path $backupPath -ErrorAction Stop }} catch {{ $json = $null }}
if ($json) {{
  foreach ($r in @($json | ConvertFrom-Json)) {{
    if ($prefixes -notcontains $r.DestinationPrefix) {{ continue }}
    New-NetRoute -DestinationPrefix $r.DestinationPrefix `
      -InterfaceAlias $r.InterfaceAlias -NextHop $r.NextHop `
      -RouteMetric $r.RouteMetric -PolicyStore ActiveStore `
      -ErrorAction SilentlyContinue | Out-Null;
  }}
}}"#
        )
    };

    run_powershell(&script)
}

/// Installs the multicast and limited-broadcast routes that make games run
/// their discovery on the virtual interface. Removed when the tunnel stops.
struct RouteGuard {
    #[cfg(any(target_os = "linux", windows))]
    interface: String,
    #[cfg(windows)]
    backup: std::path::PathBuf,
}

impl RouteGuard {
    fn install(interface: &str) -> Self {
        #[cfg(target_os = "linux")]
        {
            const ROUTES: [&str; 3] = ["224.0.0.0/4", "224.0.2.60/32", "255.255.255.255/32"];
            for destination in ROUTES {
                match std::process::Command::new("ip")
                    .args(["route", "replace", destination, "dev", interface])
                    .output()
                {
                    Ok(output) if output.status.success() => {
                        info!(destination, interface, "installed discovery route");
                    }
                    Ok(output) => {
                        warn!(
                            destination,
                            interface,
                            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                            "failed to install discovery route"
                        );
                    }
                    Err(error) => {
                        warn!(destination, interface, %error, "could not run `ip route`");
                    }
                }
            }
            Self {
                interface: interface.to_string(),
            }
        }

        #[cfg(windows)]
        {
            let backup = discovery_backup_path();
            if let Some(parent) = backup.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match route_script(interface, true, &backup) {
                Ok(output) if output.status.success() => {
                    let selected = String::from_utf8_lossy(&output.stdout);
                    info!(
                        interface,
                        selected = %selected.trim(),
                        "installed discovery routes"
                    );
                }
                Ok(output) => {
                    let text = format!(
                        "{}{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                    warn!(
                        interface,
                        output = %text.trim(),
                        "failed to install discovery routes"
                    );
                }
                Err(error) => warn!(interface, %error, "could not run PowerShell"),
            }
            Self {
                interface: interface.to_string(),
                backup,
            }
        }

        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = interface;
            warn!("multicast/broadcast routes are not implemented on this platform yet");
            Self {}
        }
    }
}

impl Drop for RouteGuard {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        {
            for destination in ["224.0.0.0/4", "224.0.2.60/32", "255.255.255.255/32"] {
                let _ = std::process::Command::new("ip")
                    .args(["route", "del", destination, "dev", &self.interface])
                    .output();
            }
        }

        #[cfg(windows)]
        {
            let _ = route_script(&self.interface, false, &self.backup);
            let _ = std::fs::remove_file(&self.backup);
        }
    }
}

/// Windows needs two extra pieces of setup:
///
/// * the TAP adapter is classified as a new network, usually "Public", which
///   blocks inbound game traffic;
/// * an explicit inbound firewall rule is needed for the tunnel interface.
#[cfg(windows)]
fn configure_windows_interface(interface: &str) {
    let escaped = interface.replace('\'', "''");
    let executable = std::env::current_exe()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default()
        .replace('\'', "''");

    // One rule per TAP adapter: several rooms can be connected at once and a
    // shared display name would make each room delete the previous rule.
    let rule = format!("nuvia-{interface}").replace('\'', "''");

    let mut script = format!(
        "$ErrorActionPreference = 'SilentlyContinue';\
         Start-Sleep -Milliseconds 500;\
         Set-NetIPInterface -InterfaceAlias '{escaped}' -InterfaceMetric 1;\
         Set-NetConnectionProfile -InterfaceAlias '{escaped}' -NetworkCategory Private;\
         Remove-NetFirewallRule -DisplayName '{rule}';\
         New-NetFirewallRule -DisplayName '{rule}' -Direction Inbound \
         -InterfaceAlias '{escaped}' -Action Allow -Profile Any | Out-Null;\
         Set-NetConnectionProfile -InterfaceAlias '{escaped}' -NetworkCategory Private"
    );
    if !executable.is_empty() {
        script.push_str(&format!(
            ";Remove-NetFirewallRule -DisplayName 'nuvia-app';\
             New-NetFirewallRule -DisplayName 'nuvia-app' -Direction Inbound \
             -Program '{executable}' -Action Allow -Profile Any | Out-Null"
        ));
    }

    match run_powershell(&script) {
        Ok(output) if output.status.success() => {
            info!(interface, "configured firewall and network profile");
        }
        Ok(output) => {
            warn!(
                interface,
                stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                "could not fully configure firewall/profile"
            );
        }
        Err(error) => warn!(interface, %error, "could not run powershell.exe"),
    }
}

/// Public handle for the GUI: installs the game discovery routes on a given
/// TAP and removes them when dropped. The room tunnels themselves run with
/// `broadcast_routes = false`; only the active room owns these routes.
pub struct DiscoveryRoutes {
    _guard: RouteGuard,
}

impl DiscoveryRoutes {
    pub fn install(interface: &str) -> Self {
        Self {
            _guard: RouteGuard::install(interface),
        }
    }
}

/// Completes a Noise handshake and confirms the peer can actually decrypt our
/// traffic. Retries forever, which is what a long lived tunnel wants.
pub async fn establish(
    socket: &UdpSocket,
    role: Role,
    identity: &Identity,
    expected_peer: &[u8; 32],
    psk: &[u8; 32],
) -> Result<Session> {
    loop {
        let result = match role {
            Role::Initiator => initiator_once(socket, identity, expected_peer, psk).await,
            Role::Responder => responder_once(socket, identity, expected_peer, psk).await,
        };

        match result {
            Ok(session) => return Ok(session),
            Err(error) => {
                debug!("handshake attempt failed: {error}");
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        }
    }
}

async fn initiator_once(
    socket: &UdpSocket,
    identity: &Identity,
    expected_peer: &[u8; 32],
    psk: &[u8; 32],
) -> Result<Session> {
    let mut handshake = Handshake::new(Role::Initiator, identity.private_key())?;

    // Open our NAT mapping before the first handshake message.
    send_punch(socket).await?;

    let message = handshake.write(&[])?;
    send_handshake(socket, &message).await?;

    let message = recv_handshake(socket, Duration::from_secs(1))
        .await?
        .ok_or_else(|| Error::Timeout("waiting for noise message 2".into()))?;
    handshake.read(&message)?;

    let seed = *handshake.local_seed();
    let message = handshake.write(&seed)?;
    send_handshake(socket, &message).await?;

    let session = handshake.finish(psk, expected_peer)?;
    confirm(socket, session, Duration::from_secs(3)).await
}

async fn responder_once(
    socket: &UdpSocket,
    identity: &Identity,
    expected_peer: &[u8; 32],
    psk: &[u8; 32],
) -> Result<Session> {
    let message = recv_handshake_with_punch(socket, Duration::from_secs(30))
        .await?
        .ok_or_else(|| Error::Timeout("waiting for noise message 1".into()))?;

    let mut handshake = Handshake::new(Role::Responder, identity.private_key())?;
    handshake.read(&message)?;

    let seed = *handshake.local_seed();
    let message = handshake.write(&seed)?;
    send_handshake(socket, &message).await?;

    let message = recv_handshake(socket, Duration::from_secs(2))
        .await?
        .ok_or_else(|| Error::Timeout("waiting for noise message 3".into()))?;
    handshake.read(&message)?;

    let session = handshake.finish(psk, expected_peer)?;
    confirm(socket, session, Duration::from_secs(3)).await
}

async fn send_handshake(socket: &UdpSocket, message: &[u8]) -> Result<()> {
    if message.len() > MAX_PACKET - 5 {
        return Err(Error::protocol("handshake message too large"));
    }
    let mut packet = Vec::with_capacity(5 + message.len());
    packet.extend_from_slice(&MAGIC);
    packet.push(TYPE_HANDSHAKE);
    packet.extend_from_slice(message);
    socket.send(&packet).await?;
    Ok(())
}

async fn recv_handshake(socket: &UdpSocket, timeout: Duration) -> Result<Option<Vec<u8>>> {
    let deadline = Instant::now() + timeout;
    let mut buffer = vec![0u8; MAX_PACKET];

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }

        let received = match tokio::time::timeout(remaining, socket.recv(&mut buffer)).await {
            Ok(result) => result?,
            Err(_) => return Ok(None),
        };

        if matches!(packet_type(&buffer[..received]), Ok(TYPE_HANDSHAKE)) {
            return Ok(Some(buffer[5..received].to_vec()));
        }
        // Data packets from an old session or junk are ignored while the new
        // handshake is in flight.
    }
}

/// Sends a plaintext punch packet. The peer ignores the payload; its only job
/// is to open the local NAT mapping so a simultaneous handshake can cross.
async fn send_punch(socket: &UdpSocket) -> Result<()> {
    let mut packet = Vec::with_capacity(5);
    packet.extend_from_slice(&MAGIC);
    packet.push(TYPE_PUNCH);
    socket.send(&packet).await?;
    Ok(())
}

async fn recv_handshake_with_punch(
    socket: &UdpSocket,
    timeout: Duration,
) -> Result<Option<Vec<u8>>> {
    let deadline = Instant::now() + timeout;
    loop {
        send_punch(socket).await?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        let wait = remaining.min(Duration::from_secs(1));
        if let Some(message) = recv_handshake(socket, wait).await? {
            return Ok(Some(message));
        }
    }
}

/// Sends authenticated empty frames until the peer proves it holds the same
/// session keys. This catches a lost final handshake message.
async fn confirm(socket: &UdpSocket, mut session: Session, timeout: Duration) -> Result<Session> {
    let deadline = Instant::now() + timeout;
    let mut buffer = vec![0u8; MAX_PACKET];

    loop {
        let packet = session.seal(&[])?;
        socket.send(&packet).await?;

        let wait =
            Duration::from_millis(300).min(deadline.saturating_duration_since(Instant::now()));
        if wait.is_zero() {
            return Err(Error::Timeout("confirming session with peer".into()));
        }

        match tokio::time::timeout(wait, socket.recv(&mut buffer)).await {
            Ok(Ok(received)) => {
                let packet = &buffer[..received];
                if matches!(packet_type(packet), Ok(TYPE_DATA)) && session.open(packet).is_ok() {
                    return Ok(session);
                }
            }
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => {
                if Instant::now() >= deadline {
                    return Err(Error::Timeout("confirming session with peer".into()));
                }
            }
        }
    }
}

pub async fn establish_relay(
    socket: &UdpSocket,
    role: Role,
    identity: &Identity,
    expected_peer: &[u8; 32],
    psk: &[u8; 32],
    relay: &RelayConfig,
) -> Result<Session> {
    loop {
        let result = match role {
            Role::Initiator => {
                relay_initiator_once(socket, identity, expected_peer, psk, relay).await
            }
            Role::Responder => {
                relay_responder_once(socket, identity, expected_peer, psk, relay).await
            }
        };

        match result {
            Ok(session) => return Ok(session),
            Err(error) => {
                debug!("relay handshake attempt failed: {error}");
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        }
    }
}

async fn relay_initiator_once(
    socket: &UdpSocket,
    identity: &Identity,
    expected_peer: &[u8; 32],
    psk: &[u8; 32],
    relay: &RelayConfig,
) -> Result<Session> {
    let mut handshake = Handshake::new(Role::Initiator, identity.private_key())?;

    let message = handshake.write(&[])?;
    send_relay_handshake(socket, relay, &message).await?;

    let message = recv_relay_handshake(socket, relay, Duration::from_secs(1))
        .await?
        .ok_or_else(|| Error::Timeout("waiting for relay noise message 2".into()))?;
    handshake.read(&message)?;

    let seed = *handshake.local_seed();
    let message = handshake.write(&seed)?;
    send_relay_handshake(socket, relay, &message).await?;

    let session = handshake.finish(psk, expected_peer)?;
    confirm_relay(socket, relay, session, Duration::from_secs(3)).await
}

async fn relay_responder_once(
    socket: &UdpSocket,
    identity: &Identity,
    expected_peer: &[u8; 32],
    psk: &[u8; 32],
    relay: &RelayConfig,
) -> Result<Session> {
    let message = recv_relay_handshake_with_punch(socket, relay, Duration::from_secs(30))
        .await?
        .ok_or_else(|| Error::Timeout("waiting for relay noise message 1".into()))?;

    let mut handshake = Handshake::new(Role::Responder, identity.private_key())?;
    handshake.read(&message)?;

    let seed = *handshake.local_seed();
    let message = handshake.write(&seed)?;
    send_relay_handshake(socket, relay, &message).await?;

    let message = recv_relay_handshake(socket, relay, Duration::from_secs(2))
        .await?
        .ok_or_else(|| Error::Timeout("waiting for relay noise message 3".into()))?;
    handshake.read(&message)?;

    let session = handshake.finish(psk, expected_peer)?;
    confirm_relay(socket, relay, session, Duration::from_secs(3)).await
}

async fn send_relay_inner(socket: &UdpSocket, relay: &RelayConfig, inner: &[u8]) -> Result<()> {
    let packet = encode_relay(
        &relay.room_id,
        &relay.self_tag,
        &relay.peer_tag,
        &relay.token,
        inner,
    );
    socket.send_to(&packet, relay.server_addr).await?;
    Ok(())
}

async fn send_relay_handshake(
    socket: &UdpSocket,
    relay: &RelayConfig,
    message: &[u8],
) -> Result<()> {
    let mut inner = Vec::with_capacity(5 + message.len());
    inner.extend_from_slice(&MAGIC);
    inner.push(TYPE_HANDSHAKE);
    inner.extend_from_slice(message);
    send_relay_inner(socket, relay, &inner).await
}

async fn send_relay_punch(socket: &UdpSocket, relay: &RelayConfig) -> Result<()> {
    let mut inner = Vec::with_capacity(5);
    inner.extend_from_slice(&MAGIC);
    inner.push(TYPE_PUNCH);
    send_relay_inner(socket, relay, &inner).await
}

async fn recv_relay_inner(
    socket: &UdpSocket,
    relay: &RelayConfig,
    timeout: Duration,
) -> Result<Option<Vec<u8>>> {
    let deadline = Instant::now() + timeout;
    let mut buffer = vec![0u8; 2048];

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }

        let (received, source) =
            match tokio::time::timeout(remaining, socket.recv_from(&mut buffer)).await {
                Ok(result) => result?,
                Err(_) => return Ok(None),
            };

        if source != relay.server_addr {
            continue;
        }
        if received < 5 || buffer[..4] != MAGIC {
            continue;
        }
        return Ok(Some(buffer[..received].to_vec()));
    }
}

async fn recv_relay_handshake(
    socket: &UdpSocket,
    relay: &RelayConfig,
    timeout: Duration,
) -> Result<Option<Vec<u8>>> {
    let deadline = Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }

        let Some(inner) = recv_relay_inner(socket, relay, remaining).await? else {
            return Ok(None);
        };
        if matches!(packet_type(&inner), Ok(TYPE_HANDSHAKE)) && inner.len() > 5 {
            return Ok(Some(inner[5..].to_vec()));
        }
        // Data/punch from the peer or a stale handshake is ignored.
    }
}

async fn recv_relay_handshake_with_punch(
    socket: &UdpSocket,
    relay: &RelayConfig,
    timeout: Duration,
) -> Result<Option<Vec<u8>>> {
    let deadline = Instant::now() + timeout;

    loop {
        send_relay_punch(socket, relay).await?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        let wait = remaining.min(Duration::from_secs(1));
        if let Some(message) = recv_relay_handshake(socket, relay, wait).await? {
            return Ok(Some(message));
        }
    }
}

async fn confirm_relay(
    socket: &UdpSocket,
    relay: &RelayConfig,
    mut session: Session,
    timeout: Duration,
) -> Result<Session> {
    let deadline = Instant::now() + timeout;

    loop {
        let packet = session.seal(&[])?;
        send_relay_inner(socket, relay, &packet).await?;

        let wait =
            Duration::from_millis(300).min(deadline.saturating_duration_since(Instant::now()));
        if wait.is_zero() {
            return Err(Error::Timeout("confirming relay session with peer".into()));
        }

        if let Some(inner) = recv_relay_inner(socket, relay, wait).await? {
            if matches!(packet_type(&inner), Ok(TYPE_DATA)) && session.open(&inner).is_ok() {
                return Ok(session);
            }
        }

        if Instant::now() >= deadline {
            return Err(Error::Timeout("confirming relay session with peer".into()));
        }
    }
}

async fn data_loop_relay(
    tap: &AsyncDevice,
    socket: &UdpSocket,
    mut session: Session,
    relay: &RelayConfig,
    keepalive: Duration,
    idle_timeout: Duration,
) -> Result<()> {
    let mut tap_buffer = vec![0u8; MAX_PACKET];
    let mut udp_buffer = vec![0u8; 2048];

    let mut keepalive_tick = tokio::time::interval(keepalive);
    keepalive_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut stats_tick = tokio::time::interval(Duration::from_secs(30));

    let mut last_rx = Instant::now();
    let mut tx_frames = 0u64;
    let mut rx_frames = 0u64;
    let mut tx_bytes = 0u64;
    let mut rx_bytes = 0u64;

    loop {
        tokio::select! {
            result = tap.recv(&mut tap_buffer) => {
                let length = result?;
                if length == 0 {
                    continue;
                }
                let packet = session.seal(&tap_buffer[..length])?;
                send_relay_inner(socket, relay, &packet).await?;
                tx_frames += 1;
                tx_bytes += length as u64;
            }

            result = socket.recv_from(&mut udp_buffer) => {
                let (length, source) = result?;
                if source != relay.server_addr {
                    continue;
                }
                let packet = &udp_buffer[..length];
                if matches!(packet_type(packet), Ok(TYPE_DATA)) {
                    match session.open(packet) {
                        Ok(payload) => {
                            last_rx = Instant::now();
                            if !payload.is_empty() {
                                let written = tap.send(&payload).await?;
                                if written != payload.len() {
                                    return Err(Error::protocol(format!(
                                        "short tap write: {written}/{}",
                                        payload.len()
                                    )));
                                }
                            }
                            rx_frames += 1;
                            rx_bytes += payload.len() as u64;
                        }
                        Err(error) => debug!("dropped relay data packet: {error}"),
                    }
                }
            }

            _ = keepalive_tick.tick() => {
                let packet = session.seal(&[])?;
                send_relay_inner(socket, relay, &packet).await?;
                if last_rx.elapsed() > idle_timeout {
                    return Err(Error::Timeout(format!(
                        "no relay packets from peer for {:?}",
                        idle_timeout
                    )));
                }
            }

            _ = stats_tick.tick() => {
                debug!(tx_frames, rx_frames, tx_bytes, rx_bytes, "relay stats");
            }
        }
    }
}

async fn data_loop(
    tap: &AsyncDevice,
    socket: &UdpSocket,
    mut session: Session,
    keepalive: Duration,
    idle_timeout: Duration,
) -> Result<()> {
    let mut tap_buffer = vec![0u8; MAX_PACKET];
    let mut udp_buffer = vec![0u8; MAX_PACKET];

    let mut keepalive_tick = tokio::time::interval(keepalive);
    keepalive_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut stats_tick = tokio::time::interval(Duration::from_secs(30));

    let mut last_rx = Instant::now();
    let mut tx_frames = 0u64;
    let mut rx_frames = 0u64;
    let mut tx_bytes = 0u64;
    let mut rx_bytes = 0u64;

    loop {
        tokio::select! {
            result = tap.recv(&mut tap_buffer) => {
                let length = result?;
                if length == 0 {
                    continue;
                }
                let packet = session.seal(&tap_buffer[..length])?;
                socket.send(&packet).await?;
                tx_frames += 1;
                tx_bytes += length as u64;
            }

            result = socket.recv(&mut udp_buffer) => {
                let length = result?;
                let packet = &udp_buffer[..length];

                if let Ok(TYPE_DATA) = packet_type(packet) {
                    match session.open(packet) {
                        Ok(payload) => {
                            last_rx = Instant::now();
                            if !payload.is_empty() {
                                let written = tap.send(&payload).await?;
                                if written != payload.len() {
                                    return Err(Error::protocol(format!(
                                        "short tap write: {written}/{}",
                                        payload.len()
                                    )));
                                }
                            }
                            rx_frames += 1;
                            rx_bytes += payload.len() as u64;
                        }
                        Err(error) => debug!("dropped data packet: {error}"),
                    }
                }
            }

            _ = keepalive_tick.tick() => {
                let packet = session.seal(&[])?;
                socket.send(&packet).await?;
                if last_rx.elapsed() > idle_timeout {
                    return Err(Error::Timeout(format!(
                        "no packets from peer for {:?}",
                        idle_timeout
                    )));
                }
            }

            _ = stats_tick.tick() => {
                debug!(tx_frames, rx_frames, tx_bytes, rx_bytes, "tunnel stats");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn socket_pair() -> (Arc<UdpSocket>, Arc<UdpSocket>) {
        let a = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let b = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        a.connect(b.local_addr().unwrap()).unwrap();
        b.connect(a.local_addr().unwrap()).unwrap();
        a.set_nonblocking(true).unwrap();
        b.set_nonblocking(true).unwrap();
        (
            Arc::new(UdpSocket::from_std(a).unwrap()),
            Arc::new(UdpSocket::from_std(b).unwrap()),
        )
    }

    #[tokio::test]
    async fn udp_handshake_establishes_session() {
        let (a_identity, b_identity) = (Identity::generate(None), Identity::generate(None));
        let a_public = *a_identity.public_key();
        let b_public = *b_identity.public_key();
        let psk = [0x51u8; 32];
        let (socket_a, socket_b) = socket_pair();

        let responder = {
            let socket = socket_b.clone();
            let identity = b_identity;
            tokio::spawn(async move {
                establish(&socket, Role::Responder, &identity, &a_public, &psk).await
            })
        };

        let mut initiator = tokio::time::timeout(
            Duration::from_secs(15),
            establish(&socket_a, Role::Initiator, &a_identity, &b_public, &psk),
        )
        .await
        .expect("initiator timed out")
        .expect("initiator failed");

        let mut responder = tokio::time::timeout(Duration::from_secs(15), responder)
            .await
            .expect("responder timed out")
            .expect("responder task panicked")
            .expect("responder failed");

        assert_eq!(initiator.session_id(), responder.session_id());

        let packet = initiator.seal(b"ping").unwrap();
        assert_eq!(responder.open(&packet).unwrap(), b"ping");

        let packet = responder.seal(b"pong").unwrap();
        assert_eq!(initiator.open(&packet).unwrap(), b"pong");
    }

    #[tokio::test]
    async fn handshake_rejects_wrong_peer() {
        let (a_identity, b_identity, other) = (
            Identity::generate(None),
            Identity::generate(None),
            Identity::generate(None),
        );
        let a_public = *a_identity.public_key();
        let b_public = *b_identity.public_key();
        let other_public = *other.public_key();
        let psk = [0x52u8; 32];
        let (socket_a, socket_b) = socket_pair();

        let responder = {
            let socket = socket_b.clone();
            let identity = b_identity;
            tokio::spawn(async move {
                establish(&socket, Role::Responder, &identity, &a_public, &psk).await
            })
        };

        // The initiator is configured with the wrong peer key. It must never
        // report success; the responder keeps waiting and is aborted below.
        let initiator = tokio::time::timeout(
            Duration::from_secs(4),
            establish(&socket_a, Role::Initiator, &a_identity, &other_public, &psk),
        )
        .await;
        assert!(initiator.is_err(), "handshake should not succeed");

        responder.abort();

        // Silence the unused warning while keeping the test intent explicit.
        let _ = b_public;
    }
}
