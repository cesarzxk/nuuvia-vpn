use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use nuvia_core::identity::default_identity_path;
use nuvia_core::tunnel::{
    run, run_auto, run_relay_with_socket, run_with_socket, RelayConfig, TapConfig, TunnelConfig,
};
use nuvia_core::{
    default_server_udp, generate_token, parse_token, Identity, RendezvousClient, RendezvousConfig,
    RoomKey,
};
use nuvia_protocol::{hex_string, peer_tag, Invite, PROTOCOL_VERSION};
use tokio::net::UdpSocket;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "nuvia",
    version,
    about = "Virtual LAN for games and local multiplayer",
    long_about = "nuvia builds an encrypted Layer 2 tunnel between computers, so games \
                  and LAN discovery protocols behave as if every peer were on the same \
                  physical network."
)]
struct Cli {
    /// Path to the device identity file.
    #[arg(long, env = "NUVIA_IDENTITY", global = true)]
    identity: Option<PathBuf>,

    /// Log filter (overridden by RUST_LOG).
    #[arg(long, default_value = "info", global = true)]
    log: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate a new device identity.
    Keygen {
        /// Human readable device name stored with the identity.
        #[arg(long)]
        name: Option<String>,
        /// Overwrite an existing identity.
        #[arg(long)]
        force: bool,
    },

    /// Print this device's public key.
    Pubkey,

    /// Generate a shareable internet invite (server + room + token).
    Invite {
        /// Rendezvous URL, e.g. https://relay.example.com.
        #[arg(long)]
        server: String,
        /// UDP endpoint of the rendezvous server. Defaults to host:8443.
        #[arg(long)]
        server_udp: Option<String>,
        /// Room name.
        #[arg(long)]
        room: String,
    },

    /// Bring up the encrypted tunnel.
    Up(UpArgs),

    /// Check the local environment.
    Doctor,
}

#[derive(Parser)]
struct UpArgs {
    /// Room name. Not needed when --invite is used.
    #[arg(long, required_unless_present = "invite")]
    room: Option<String>,

    /// Room password. Never sent over the network.
    #[arg(long)]
    password: String,

    /// TAP interface name.
    #[arg(long, default_value = "nuvia-vpn0")]
    tap_name: String,

    /// Local virtual address in CIDR form, e.g. 10.240.0.1/24. In internet
    /// mode it is derived from the room when omitted.
    #[arg(long)]
    ip: Option<String>,

    /// UDP listen address.
    #[arg(long, default_value = "0.0.0.0:42424")]
    bind: SocketAddr,

    /// Maximum participants when creating an internet room (2-32). The GUI
    /// keeps creating rooms with 8; use this from the CLI for larger rooms.
    #[arg(long, default_value_t = nuvia_protocol::DEFAULT_MAX_PEERS, value_parser = clap::value_parser!(u16).range(2..=32))]
    max_peers: u16,

    /// Peer in the form <public_key_hex>@<ip:port> (LAN mode).
    #[arg(long, conflicts_with = "server")]
    peer: Option<String>,

    /// Rendezvous server URL (internet mode).
    #[arg(long)]
    server: Option<String>,

    /// Rendezvous UDP endpoint. Defaults to the server host on port 8443.
    #[arg(long, requires = "server")]
    server_udp: Option<String>,

    /// Room token from `nuvia invite` (internet mode). When omitted, the token
    /// is derived from the room name and password, matching the GUI.
    #[arg(long, requires = "server")]
    room_token: Option<String>,

    /// Invite code produced by `nuvia invite`.
    #[arg(long, conflicts_with_all = ["peer", "server", "room", "room_token"])]
    invite: Option<String>,

    /// Force the relay path (needed when both sides are behind symmetric NAT).
    #[arg(long, conflicts_with = "direct")]
    relay: bool,

    /// Force the direct path, without relay fallback.
    #[arg(long)]
    direct: bool,

    /// TAP MTU. 1360 leaves room for the tunnel envelope.
    #[arg(long, default_value_t = 1360)]
    mtu: u16,

    /// Keepalive interval in seconds.
    #[arg(long, default_value_t = 10)]
    keepalive: u64,

    /// Tear down the session after this many seconds without packets.
    #[arg(long, default_value_t = 30)]
    idle_timeout: u64,

    /// Do not route 224.0.0.0/4 and 255.255.255.255 through the TAP. Use this
    /// if local discovery (Chromecast, printers) must keep working.
    #[arg(long)]
    no_broadcast_routes: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cli.log));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    let identity_path = cli.identity.unwrap_or_else(default_identity_path);

    match cli.command {
        Command::Keygen { name, force } => keygen(&identity_path, name, force),
        Command::Pubkey => pubkey(&identity_path),
        Command::Invite {
            server,
            server_udp,
            room,
        } => invite(server, server_udp, room),
        Command::Up(args) => up(&identity_path, args).await,
        Command::Doctor => doctor(&identity_path),
    }
}

fn keygen(path: &std::path::Path, name: Option<String>, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "identity already exists at {} (use --force to replace it)",
            path.display()
        );
    }

    let identity = Identity::generate(name);
    identity.save(path)?;

    println!("identity:   {}", path.display());
    println!("public_key: {}", identity.public_key_hex());
    Ok(())
}

fn pubkey(path: &std::path::Path) -> Result<()> {
    if !path.exists() {
        bail!(
            "no identity at {} (run `nuvia keygen` first)",
            path.display()
        );
    }
    let identity = Identity::load(path)?;
    println!("identity:   {}", path.display());
    if let Some(name) = identity.name() {
        println!("name:       {name}");
    }
    println!("public_key: {}", identity.public_key_hex());
    Ok(())
}

async fn up(path: &std::path::Path, args: UpArgs) -> Result<()> {
    let identity = Identity::load_or_create(path, hostname())
        .with_context(|| format!("loading identity from {}", path.display()))?;

    let (room_name, server_url, server_udp, room_token) = resolve_mode(&args)?;
    let room = RoomKey::derive(&room_name, &args.password).context("deriving room key")?;
    if server_url.is_none() && args.ip.is_none() {
        bail!("--ip is required in LAN mode");
    }

    #[cfg(unix)]
    if unsafe { libc::geteuid() } != 0 {
        tracing::warn!("creating a TAP interface normally requires root or CAP_NET_ADMIN");
    }

    let (peer_key, peer_addr, socket, relay) = if let Some(server) = server_url {
        let token = match room_token {
            Some(token) => token,
            // Same derivation as the GUI: no invite code is needed when both
            // sides know the room name and password.
            None => hex::encode(room.relay_token()),
        };
        let token = parse_token(&token)?;
        let server_udp = match server_udp {
            Some(value) => value,
            None => default_server_udp(&server)?,
        };

        let socket = UdpSocket::bind(args.bind)
            .await
            .with_context(|| format!("binding UDP {}", args.bind))?;
        let mut rendezvous = RendezvousConfig::new(
            server.clone(),
            server_udp.clone(),
            *room.id(),
            token,
            *identity.public_key(),
        );
        rendezvous.set_local_endpoints(socket.local_addr()?);
        rendezvous.name = hostname();
        rendezvous.max_peers = Some(args.max_peers);
        let client = RendezvousClient::new(rendezvous).context("creating rendezvous client")?;

        println!("server:        {server}");
        println!("server_udp:    {server_udp}");
        println!("room_id:       {}", room.id_hex());
        println!("waiting for another peer in the room...");
        println!();

        let (peer, peer_addr) = client
            .wait_for_peer(&socket)
            .await
            .context("waiting for a peer")?;
        let peer_key = parse_peer_id(&peer.peer_id)?;
        if &peer_key == identity.public_key() {
            bail!("server returned our own public key as the peer");
        }

        let relay = if args.relay {
            Some(RelayConfig {
                server_addr: nuvia_core::rendezvous::resolve_udp(&server_udp)
                    .await
                    .with_context(|| format!("resolving relay endpoint {server_udp}"))?,
                room_id: *room.id(),
                token,
                self_tag: peer_tag(identity.public_key()),
                peer_tag: peer_tag(&peer_key),
            })
        } else {
            None
        };

        if args.relay {
            (peer_key, peer_addr, Some(socket), relay)
        } else {
            socket
                .connect(peer_addr)
                .await
                .with_context(|| format!("connecting UDP socket to {peer_addr}"))?;

            if args.direct {
                (peer_key, peer_addr, Some(socket), None)
            } else {
                let relay = Some(RelayConfig {
                    server_addr: nuvia_core::rendezvous::resolve_udp(&server_udp)
                        .await
                        .with_context(|| format!("resolving relay endpoint {server_udp}"))?,
                    room_id: *room.id(),
                    token,
                    self_tag: peer_tag(identity.public_key()),
                    peer_tag: peer_tag(&peer_key),
                });
                (peer_key, peer_addr, Some(socket), relay)
            }
        }
    } else {
        if args.relay {
            bail!("--relay requires --server or --invite");
        }
        let peer = args
            .peer
            .as_deref()
            .ok_or_else(|| anyhow!("provide --peer (LAN) or --server/--invite (internet)"))?;
        let (peer_key, peer_addr) = parse_peer(peer)?;
        if &peer_key == identity.public_key() {
            bail!("--peer is this device's own public key; pass the other computer's public key");
        }
        (peer_key, peer_addr, None, None)
    };

    let (ipv4, prefix) = match &args.ip {
        Some(value) => parse_ipv4_cidr(value)?,
        None => {
            // Same rule as the GUI: smaller public key gets .1, larger gets
            // .2, inside a subnet derived from the room.
            let last: u8 = if identity.public_key() < &peer_key {
                1
            } else {
                2
            };
            (Ipv4Addr::new(10, 240, room.subnet_octet(), last), 24)
        }
    };

    println!("identity:      {}", path.display());
    println!("public_key:    {}", identity.public_key_hex());
    println!("peer_public:   {}", hex::encode(peer_key));
    println!("room_id:       {}", room.id_hex());
    println!(
        "tap:           {} {}/{} mtu {}",
        args.tap_name, ipv4, prefix, args.mtu
    );
    println!("udp:           {} -> {}", args.bind, peer_addr);
    println!();

    let tap = TapConfig {
        name: args.tap_name,
        ipv4,
        prefix,
        mtu: args.mtu,
    };

    let mut config = TunnelConfig::new(args.bind, peer_addr, peer_key, tap);
    config.keepalive = std::time::Duration::from_secs(args.keepalive);
    config.idle_timeout = std::time::Duration::from_secs(args.idle_timeout);
    config.broadcast_routes = !args.no_broadcast_routes;

    let result = match (socket, relay) {
        (Some(socket), Some(relay)) => {
            if args.relay {
                tokio::select! {
                    result = run_relay_with_socket(config, &identity, &room, socket, relay) => result,
                    _ = tokio::signal::ctrl_c() => {
                        info!("shutting down");
                        Ok(())
                    }
                }
            } else {
                tokio::select! {
                    result = run_auto(
                        config,
                        &identity,
                        &room,
                        socket,
                        relay,
                        std::time::Duration::from_secs(8),
                    ) => result,
                    _ = tokio::signal::ctrl_c() => {
                        info!("shutting down");
                        Ok(())
                    }
                }
            }
        }
        (Some(socket), None) => {
            tokio::select! {
                result = run_with_socket(config, &identity, &room, socket) => result,
                _ = tokio::signal::ctrl_c() => {
                    info!("shutting down");
                    Ok(())
                }
            }
        }
        (None, None) => {
            tokio::select! {
                result = run(config, &identity, &room) => result,
                _ = tokio::signal::ctrl_c() => {
                    info!("shutting down");
                    Ok(())
                }
            }
        }
        (None, Some(_)) => unreachable!("relay without a socket"),
    };

    result.with_context(|| {
        if cfg!(windows) {
            "tunnel failed (install the tap-windows6 driver and run as Administrator)"
        } else {
            "tunnel failed (creating a TAP normally requires root or CAP_NET_ADMIN)"
        }
    })
}

type UpMode = (String, Option<String>, Option<String>, Option<String>);

fn resolve_mode(args: &UpArgs) -> Result<UpMode> {
    if let Some(code) = &args.invite {
        let invite = Invite::decode(code).context("decoding invite")?;
        return Ok((
            invite.room,
            Some(invite.server),
            Some(invite.server_udp),
            Some(invite.token),
        ));
    }

    let room = args
        .room
        .clone()
        .ok_or_else(|| anyhow!("--room is required"))?;

    if let Some(server) = &args.server {
        return Ok((
            room,
            Some(server.clone()),
            args.server_udp.clone(),
            args.room_token.clone(),
        ));
    }

    if args.room_token.is_some() || args.server_udp.is_some() {
        bail!("--room-token/--server-udp require --server or --invite");
    }

    Ok((room, None, None, None))
}

fn invite(server: String, server_udp: Option<String>, room: String) -> Result<()> {
    let server_udp = match server_udp {
        Some(value) => value,
        None => default_server_udp(&server)?,
    };
    let token = generate_token();
    let invite = Invite {
        version: PROTOCOL_VERSION,
        server,
        server_udp,
        room,
        token: hex_string(&token),
    };

    println!("{}", invite.encode());
    println!();
    println!("server:     {}", invite.server);
    println!("server_udp: {}", invite.server_udp);
    println!("room:       {}", invite.room);
    println!("token:      {}", invite.token);
    println!();
    println!("Envie o código acima. A senha da sala é combinada à parte.");
    Ok(())
}

fn doctor(path: &std::path::Path) -> Result<()> {
    println!("nuvia {}", env!("CARGO_PKG_VERSION"));
    println!("os:            {}", std::env::consts::OS);
    println!("arch:          {}", std::env::consts::ARCH);
    println!("identity:      {}", path.display());

    if path.exists() {
        match Identity::load(path) {
            Ok(identity) => println!("public_key:    {}", identity.public_key_hex()),
            Err(error) => println!("public_key:    ERROR ({error})"),
        }
    } else {
        println!("public_key:    not generated yet");
    }

    #[cfg(unix)]
    {
        let euid = unsafe { libc::geteuid() };
        println!("euid:          {euid}");
        if euid != 0 {
            println!("privileges:    no root; TAP creation will likely fail");
        } else {
            println!("privileges:    root");
        }
    }

    #[cfg(target_os = "linux")]
    {
        let tun = std::path::Path::new("/dev/net/tun");
        println!(
            "/dev/net/tun:  {}",
            if tun.exists() { "present" } else { "missing" }
        );
    }

    #[cfg(target_os = "windows")]
    {
        println!(
            "tap driver:    run `nuvia doctor` as Administrator after installing tap-windows6"
        );
    }

    Ok(())
}

fn parse_peer(value: &str) -> Result<([u8; 32], SocketAddr)> {
    let (key, address) = value
        .split_once('@')
        .with_context(|| format!("peer must be <public_key>@<ip:port>, got {value:?}"))?;

    let peer_key = parse_peer_id(key)?;
    let address = address
        .parse()
        .with_context(|| format!("invalid peer address: {address:?}"))?;

    Ok((peer_key, address))
}

fn parse_peer_id(value: &str) -> Result<[u8; 32]> {
    let bytes = hex::decode(value.trim())
        .with_context(|| format!("peer public key is not valid hex: {value:?}"))?;
    if bytes.len() != 32 {
        bail!("peer public key must be 32 bytes, got {}", bytes.len());
    }
    let mut peer_key = [0u8; 32];
    peer_key.copy_from_slice(&bytes);
    Ok(peer_key)
}

fn parse_ipv4_cidr(value: &str) -> Result<(Ipv4Addr, u8)> {
    let (address, prefix) = value.split_once('/').unwrap_or((value, "24"));
    let address: Ipv4Addr = address
        .parse()
        .with_context(|| format!("invalid IPv4 address: {address:?}"))?;
    let prefix: u8 = prefix
        .parse()
        .with_context(|| format!("invalid prefix length: {prefix:?}"))?;
    if prefix > 32 {
        bail!("prefix length must be between 0 and 32");
    }
    Ok((address, prefix))
}

fn hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/proc/sys/kernel/hostname").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_peer() {
        let key = "11".repeat(32);
        let (peer, address) = parse_peer(&format!("{key}@192.168.1.2:42424")).unwrap();
        assert_eq!(peer, [0x11; 32]);
        assert_eq!(address.port(), 42424);
    }

    #[test]
    fn parses_cidr() {
        assert_eq!(
            parse_ipv4_cidr("10.240.0.1/24").unwrap(),
            (Ipv4Addr::new(10, 240, 0, 1), 24)
        );
        assert_eq!(
            parse_ipv4_cidr("10.240.0.2").unwrap(),
            (Ipv4Addr::new(10, 240, 0, 2), 24)
        );
    }
}
