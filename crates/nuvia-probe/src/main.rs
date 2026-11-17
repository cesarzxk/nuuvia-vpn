use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tokio::net::UdpSocket;

#[derive(Parser)]
#[command(
    name = "lanprobe",
    version,
    about = "Broadcast/multicast probe used to validate nuvia L2 forwarding"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Listen for broadcast or multicast datagrams.
    Listen {
        #[arg(long, default_value_t = 42425)]
        port: u16,
        /// IPv4 multicast group to join, e.g. 224.0.2.60.
        #[arg(long)]
        group: Option<Ipv4Addr>,
        /// Local interface address used for the multicast join.
        #[arg(long)]
        interface: Option<Ipv4Addr>,
    },

    /// Send broadcast or multicast datagrams.
    Send {
        #[arg(long, default_value_t = 42425)]
        port: u16,
        #[arg(long, default_value = "nuvia-probe")]
        message: String,
        /// Unicast, broadcast or multicast destination.
        #[arg(long, default_value = "255.255.255.255")]
        target: Ipv4Addr,
        /// Convenience alias for a multicast target.
        #[arg(long)]
        group: Option<Ipv4Addr>,
        #[arg(long, default_value_t = 1)]
        count: u32,
        #[arg(long, default_value_t = 1000)]
        interval_ms: u64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Listen {
            port,
            group,
            interface,
        } => listen(port, group, interface).await,
        Command::Send {
            port,
            message,
            target,
            group,
            count,
            interval_ms,
        } => send(port, message, group.unwrap_or(target), count, interval_ms).await,
    }
}

async fn listen(port: u16, group: Option<Ipv4Addr>, interface: Option<Ipv4Addr>) -> Result<()> {
    let socket = UdpSocket::bind(("0.0.0.0", port))
        .await
        .with_context(|| format!("binding UDP port {port}"))?;

    if let Some(group) = group {
        let interface = interface.unwrap_or(Ipv4Addr::UNSPECIFIED);
        socket
            .join_multicast_v4(group, interface)
            .with_context(|| format!("joining multicast group {group} on {interface}"))?;
        println!("listening on 0.0.0.0:{port} multicast {group} via {interface}");
    } else {
        println!("listening on 0.0.0.0:{port} broadcast");
    }

    let mut buffer = vec![0u8; 65_535];
    loop {
        let (length, source) = socket.recv_from(&mut buffer).await?;
        let payload = String::from_utf8_lossy(&buffer[..length]);
        println!("{source} ({length} bytes): {payload}");
    }
}

async fn send(
    port: u16,
    message: String,
    target: Ipv4Addr,
    count: u32,
    interval_ms: u64,
) -> Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.set_broadcast(true)?;
    socket.set_multicast_ttl_v4(1)?;

    println!("sending {count}x to {target}:{port}");
    for index in 0..count {
        let body = format!("{message} #{index}");
        socket.send_to(body.as_bytes(), (target, port)).await?;
        if interval_ms > 0 {
            tokio::time::sleep(Duration::from_millis(interval_ms)).await;
        }
    }
    Ok(())
}
