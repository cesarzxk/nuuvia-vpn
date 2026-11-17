//! Three-peer mesh smoke test against a running nuvia-server.
//!
//!   cargo run -p nuvia-core --example mesh3
//!
//! Uses a random room so it does not collide with real rooms. No TAP/root is
//! required; it only exercises rendezvous, relay broadcast and the group key.

use std::collections::HashSet;
use std::time::Duration;

use nuvia_core::mesh::GroupSession;
use nuvia_core::{default_server_udp, Identity, RendezvousClient, RendezvousConfig, RoomKey};
use nuvia_protocol::{encode_relay, peer_tag, BROADCAST_TAG};
use tokio::net::UdpSocket;

const SERVER: &str = "https://167.234.237.6.sslip.io";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server = std::env::var("NUVIA_SERVER_URL").unwrap_or_else(|_| SERVER.to_string());
    let room_name = std::env::var("NUVIA_TEST_ROOM")
        .unwrap_or_else(|_| format!("mesh3-{}", std::process::id()));
    let password = std::env::var("NUVIA_TEST_PASSWORD").unwrap_or_else(|_| "mesh3-pass".into());
    let peers = std::env::var("NUVIA_TEST_PEERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(3);
    let room = RoomKey::derive(&room_name, &password)?;
    let token = room.relay_token();
    let server_udp = match std::env::var("NUVIA_SERVER_UDP") {
        Ok(value) => value,
        Err(_) => default_server_udp(&server)?,
    };
    let server_addr = nuvia_core::rendezvous::resolve_udp(&server_udp).await?;
    println!("room {room_name} id {}", room.id_hex());

    let mut tasks = Vec::new();
    for index in 0..peers {
        let identity = Identity::generate(Some(format!("mesh3-{index}")));
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        let mut config = RendezvousConfig::new(
            server.clone(),
            server_udp.clone(),
            *room.id(),
            token,
            *identity.public_key(),
        );
        config.name = Some(format!("peer-{index}"));
        config.set_local_endpoints(socket.local_addr()?);
        let locals = config.local_endpoints.clone();
        let client = RendezvousClient::new(config)?;
        let announce = client.announce(&socket).await?;
        println!(
            "peer {index} endpoint {:?} local {:?} peers {:?}",
            announce.self_endpoint,
            locals,
            announce
                .peers
                .iter()
                .map(|peer| (&peer.endpoint, &peer.local_endpoints))
                .collect::<Vec<_>>()
        );

        let group_key = room.group_key();
        let tag = peer_tag(identity.public_key());
        let task_room = room.clone();
        tasks.push(tokio::spawn(async move {
            let mut group = GroupSession::new(&group_key, tag);
            tokio::time::sleep(Duration::from_secs(2)).await;

            let mut frame = vec![0u8; 64];
            frame[..6].copy_from_slice(&[0xff; 6]);
            frame[6..12].copy_from_slice(&[0x02, 0, 0, 0, 0, index as u8]);
            frame[12..14].copy_from_slice(&0x0800u16.to_be_bytes());
            let text = format!("hello-from-{index}");
            let length = text.len().min(frame.len() - 14);
            frame[14..14 + length].copy_from_slice(&text.as_bytes()[..length]);
            let inner = group.seal(&frame)?;
            let envelope = encode_relay(task_room.id(), &tag, &BROADCAST_TAG, &token, &inner);
            socket.send_to(&envelope, server_addr).await?;

            let mut seen = HashSet::new();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
            let mut buffer = vec![0u8; 65535];
            while tokio::time::Instant::now() < deadline {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                let received = tokio::time::timeout(remaining, socket.recv_from(&mut buffer)).await;
                let (length, source) = match received {
                    Ok(Ok(value)) => value,
                    Ok(Err(error)) => {
                        println!("peer {index} recv error: {error}");
                        break;
                    }
                    Err(_) => break,
                };
                if source != server_addr {
                    continue;
                }
                match group.open(&buffer[..length]) {
                    Ok((src, data)) => {
                        println!(
                            "peer {index} got {} from {}",
                            String::from_utf8_lossy(&data),
                            hex::encode(src)
                        );
                        seen.insert(src);
                    }
                    Err(error) => println!("peer {index} group open error: {error}"),
                }
            }
            anyhow::Ok((index, seen))
        }));
    }

    let expected = peers.saturating_sub(1);
    let mut ok = true;
    for task in tasks {
        let (index, seen) = task.await??;
        println!("peer {index}: received from {} peers", seen.len());
        if seen.len() < expected {
            ok = false;
        }
    }
    if ok {
        println!("MESH3 OK: every peer received the other {}", expected);
    } else {
        anyhow::bail!("MESH3 FAILED: a peer did not receive all the others");
    }
    Ok(())
}
