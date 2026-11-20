//! Verifies the per-participant path report produced by the mesh loop without
//! touching a real TAP. The mesh only needs the channel pair that normally
//! connects it to the shared TAP hub.

use std::net::SocketAddr;
use std::time::Duration;

use nuvia_core::firewall::FirewallProfile;
use nuvia_core::mesh::GroupSession;
use nuvia_core::rendezvous::{RendezvousClient, RendezvousConfig};
use nuvia_core::tunnel::{run_mesh, MeshConfig, TapIo, TunnelStatus};
use nuvia_protocol::decode_relay;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const ROOM: [u8; 16] = [0x11; 16];
const TOKEN: [u8; 16] = [0x22; 16];
const GROUP_KEY: [u8; 32] = [0x33; 32];
const TAG_A: [u8; 8] = [0xA1; 8];
const TAG_B: [u8; 8] = [0xB2; 8];

struct Peer {
    outbound: mpsc::Sender<Vec<u8>>,
    inbound: mpsc::Receiver<Vec<u8>>,
    status: mpsc::Receiver<TunnelStatus>,
    task: JoinHandle<nuvia_core::Result<()>>,
}

fn spawn_peer(
    socket: UdpSocket,
    server_addr: SocketAddr,
    direct_peers: Vec<SocketAddr>,
    tag: [u8; 8],
) -> Peer {
    let bind_addr = socket.local_addr().expect("local address");
    let (outbound_tx, outbound_rx) = mpsc::channel(8);
    let (inbound_tx, inbound_rx) = mpsc::channel(8);
    let (status_tx, status_rx) = mpsc::channel(16);
    let config = MeshConfig {
        bind_addr,
        server_addr,
        direct_peers,
        room_id: ROOM,
        token: TOKEN,
        status: Some(status_tx),
        peers: None,
        firewall: FirewallProfile::default(),
        firewall_updates: None,
    };
    let rendezvous = RendezvousClient::new(RendezvousConfig::new(
        "http://127.0.0.1:1",
        "127.0.0.1:9",
        ROOM,
        TOKEN,
        [tag[0]; 32],
    ))
    .expect("rendezvous client");
    let task = tokio::spawn(run_mesh(
        config,
        socket,
        rendezvous,
        GroupSession::new(&GROUP_KEY, tag),
        TapIo {
            outbound: outbound_rx,
            inbound: inbound_tx,
        },
    ));
    Peer {
        outbound: outbound_tx,
        inbound: inbound_rx,
        status: status_rx,
        task,
    }
}

async fn wait_for_frame(peer: &mut Peer, expected: &[u8]) {
    let frame = timeout(Duration::from_secs(3), peer.inbound.recv())
        .await
        .expect("mesh frame timeout")
        .expect("mesh inbound channel closed");
    assert_eq!(frame, expected);
}

async fn wait_for_path(peer: &mut Peer, tag: [u8; 8], direct: bool) {
    timeout(Duration::from_secs(3), async {
        while let Some(status) = peer.status.recv().await {
            if let TunnelStatus::Peer {
                tag: seen,
                direct: seen_direct,
            } = status
            {
                if seen == tag && seen_direct == direct {
                    return;
                }
            }
        }
        panic!("mesh status channel closed before reporting the peer path");
    })
    .await
    .expect("mesh status timeout");
}

async fn wait_for_ping(peer: &mut Peer, tag: [u8; 8]) {
    timeout(Duration::from_secs(6), async {
        while let Some(status) = peer.status.recv().await {
            if let TunnelStatus::Ping { tag: seen, ms } = status {
                if seen == tag {
                    assert!(ms < 5_000, "unexpected ping: {ms} ms");
                    return;
                }
            }
        }
        panic!("mesh status channel closed before reporting a ping");
    })
    .await
    .expect("mesh ping timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn marks_a_direct_peer_as_direct() {
    let socket_a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let socket_b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr_a = socket_a.local_addr().unwrap();
    let addr_b = socket_b.local_addr().unwrap();
    let unused_relay: SocketAddr = "127.0.0.1:9".parse().unwrap();

    let peer_a = spawn_peer(socket_a, unused_relay, vec![addr_b], TAG_A);
    let mut peer_b = spawn_peer(socket_b, unused_relay, vec![addr_a], TAG_B);

    peer_a.outbound.send(vec![0xAA; 60]).await.unwrap();
    wait_for_frame(&mut peer_b, &[0xAA; 60]).await;
    wait_for_path(&mut peer_b, TAG_A, true).await;
    wait_for_ping(&mut peer_b, TAG_A).await;

    peer_a.task.abort();
    peer_b.task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn marks_a_relayed_peer_as_relay() {
    let relay = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let socket_a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let socket_b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr_a = socket_a.local_addr().unwrap();
    let addr_b = socket_b.local_addr().unwrap();

    let relay_task = tokio::spawn(async move {
        let mut buffer = vec![0u8; u16::MAX as usize];
        loop {
            let Ok((length, _)) = relay.recv_from(&mut buffer).await else {
                return;
            };
            let Ok(packet) = decode_relay(&buffer[..length]) else {
                continue;
            };
            if packet.dst != nuvia_protocol::BROADCAST_TAG {
                continue;
            }
            let target = if packet.src == TAG_A { addr_b } else { addr_a };
            let _ = relay.send_to(packet.inner, target).await;
        }
    });

    let peer_a = spawn_peer(socket_a, relay_addr, Vec::new(), TAG_A);
    let mut peer_b = spawn_peer(socket_b, relay_addr, Vec::new(), TAG_B);

    peer_a.outbound.send(vec![0xBB; 60]).await.unwrap();
    wait_for_frame(&mut peer_b, &[0xBB; 60]).await;
    wait_for_path(&mut peer_b, TAG_A, false).await;
    wait_for_ping(&mut peer_b, TAG_A).await;

    peer_a.task.abort();
    peer_b.task.abort();
    relay_task.abort();
}
