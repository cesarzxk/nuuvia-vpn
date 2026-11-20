use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use getifaddrs::{getifaddrs, InterfaceFlags};

use nuvia_protocol::{
    hex_string, parse_hex, Hello, HelloAck, PeerInfo, RegisterRequest, RegisterResponse,
    ACK_INVALID_TOKEN, ACK_OK, ACK_ROOM_NOT_FOUND, DEFAULT_SERVER_UDP_PORT, PROTOCOL_VERSION,
};
use rand::RngCore;
use tokio::net::UdpSocket;
use tracing::{info, warn};

use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct RendezvousConfig {
    pub server_url: String,
    pub server_udp: String,
    pub room_id: [u8; 16],
    pub token: [u8; 16],
    pub peer_id: [u8; 32],
    pub name: Option<String>,
    /// LAN endpoints advertised to the other room members so peers on the same
    /// network can bypass NAT hairpinning.
    pub local_endpoints: Vec<SocketAddr>,
    pub expected_peer: Option<[u8; 32]>,
    /// Create the room when it does not exist (CLI and Create Network), or
    /// require an existing room (Join Network).
    pub create: bool,
}

impl RendezvousConfig {
    pub fn new(
        server_url: impl Into<String>,
        server_udp: impl Into<String>,
        room_id: [u8; 16],
        token: [u8; 16],
        peer_id: [u8; 32],
    ) -> Self {
        Self {
            server_url: server_url.into(),
            server_udp: server_udp.into(),
            room_id,
            token,
            peer_id,
            name: None,
            local_endpoints: Vec::new(),
            expected_peer: None,
            create: true,
        }
    }

    /// Fills the LAN hints for a socket bound at `bind` (usually
    /// `0.0.0.0:<port>`).
    pub fn set_local_endpoints(&mut self, bind: SocketAddr) {
        self.local_endpoints = local_endpoint_hints(bind);
    }
}

/// HTTP + UDP rendezvous client.
#[derive(Clone)]
pub struct RendezvousClient {
    config: RendezvousConfig,
    http: reqwest::Client,
}

impl RendezvousClient {
    pub fn new(config: RendezvousConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .no_proxy()
            .build()
            .map_err(|error| Error::Http(error.to_string()))?;
        Ok(Self { config, http })
    }

    pub fn config(&self) -> &RendezvousConfig {
        &self.config
    }

    pub async fn register(&self) -> Result<RegisterResponse> {
        let url = format!(
            "{}/v1/register",
            self.config.server_url.trim_end_matches('/')
        );
        let request = RegisterRequest {
            protocol: PROTOCOL_VERSION,
            room_id: hex_string(&self.config.room_id),
            peer_id: hex_string(&self.config.peer_id),
            token: hex_string(&self.config.token),
            create: self.config.create,
            name: self.config.name.clone(),
            local_endpoints: self
                .config
                .local_endpoints
                .iter()
                .map(ToString::to_string)
                .collect(),
        };

        let response = self
            .http
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|error| Error::Http(error.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Http(format!("{status}: {body}")));
        }

        let body = response
            .json::<RegisterResponse>()
            .await
            .map_err(|error| Error::Http(error.to_string()))?;
        if body.protocol != PROTOCOL_VERSION {
            return Err(Error::Http(format!(
                "server protocol {} is not supported",
                body.protocol
            )));
        }
        Ok(body)
    }

    /// Registers, announces the public UDP endpoint and registers again.
    ///
    /// Mesh mode calls this once before it starts flooding frames. It does not
    /// need to wait for a specific peer: every member joins the same room and
    /// learns the others from the server list.
    pub async fn announce(&self, socket: &UdpSocket) -> Result<RegisterResponse> {
        let _ = self.register().await?;
        self.send_hello(socket).await?;
        self.register().await
    }

    /// Sends the UDP hello so the server learns our public endpoint.
    pub async fn send_hello(&self, socket: &UdpSocket) -> Result<()> {
        let server = resolve_udp_for_socket(&self.config.server_udp, socket).await?;
        let hello = Hello {
            room_id: self.config.room_id,
            peer_id: self.config.peer_id,
            token: self.config.token,
        }
        .encode();

        for _ in 0..3 {
            socket.send_to(&hello, server).await?;
            let deadline = Instant::now() + Duration::from_secs(2);
            // A room with active peers can send relay frames to this endpoint
            // immediately after the HELLO. A 128-byte buffer made Windows
            // return WSAEMSGSIZE (10040); ignore non-ACK datagrams instead.
            let mut buffer = [0u8; 65_535];

            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match tokio::time::timeout(remaining, socket.recv_from(&mut buffer)).await {
                    Ok(Ok((length, source))) => {
                        if source != server {
                            continue;
                        }
                        if let Ok(ack) = HelloAck::decode(&buffer[..length]) {
                            match ack.status {
                                ACK_OK => return Ok(()),
                                ACK_INVALID_TOKEN => {
                                    return Err(Error::config("server rejected the room token"))
                                }
                                ACK_ROOM_NOT_FOUND => {
                                    return Err(Error::config("server does not know this room yet"))
                                }
                                other => {
                                    return Err(Error::Http(format!(
                                        "unexpected hello status {other}"
                                    )))
                                }
                            }
                        }
                    }
                    Ok(Err(error)) => return Err(Error::from(error)),
                    Err(_) => break,
                }
            }
        }

        Err(Error::Timeout("waiting for the server hello ack".into()))
    }

    /// Registers, announces the endpoint and polls until another peer appears.
    pub async fn wait_for_peer(&self, socket: &UdpSocket) -> Result<(PeerInfo, SocketAddr)> {
        let mut announced = false;
        let mut last_log = Instant::now() - Duration::from_secs(10);

        loop {
            let mut response = match self.register().await {
                Ok(response) => response,
                Err(error) => {
                    warn!("rendezvous register failed: {error}");
                    if fatal_http(&error) {
                        return Err(error);
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };

            if !announced || response.self_endpoint.is_none() {
                if let Err(error) = self.send_hello(socket).await {
                    warn!("rendezvous hello failed: {error}");
                    if fatal_http(&error) {
                        return Err(error);
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
                announced = true;

                response = match self.register().await {
                    Ok(response) => response,
                    Err(error) => {
                        warn!("rendezvous register failed: {error}");
                        if fatal_http(&error) {
                            return Err(error);
                        }
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        continue;
                    }
                };
            }

            if let Some(peer) = pick_peer(&response, self.config.expected_peer)? {
                info!(
                    peer = %peer.0.peer_id,
                    endpoint = %peer.1,
                    "peer discovered"
                );
                return Ok(peer);
            }

            if last_log.elapsed() >= Duration::from_secs(10) {
                info!(
                    room = %hex_string(&self.config.room_id),
                    "waiting for another peer in the room"
                );
                last_log = Instant::now();
            }

            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

fn fatal_http(error: &Error) -> bool {
    match error {
        Error::Config(_) => true,
        Error::Http(message) => {
            message.starts_with("400") || message.starts_with("401") || message.starts_with("403")
        }
        _ => false,
    }
}

fn pick_peer(
    response: &RegisterResponse,
    expected: Option<[u8; 32]>,
) -> Result<Option<(PeerInfo, SocketAddr)>> {
    for peer in &response.peers {
        if let Some(expected) = expected {
            let expected = hex_string(&expected);
            if peer.peer_id != expected {
                continue;
            }
        }

        let Some(endpoint) = peer.endpoint.as_deref() else {
            continue;
        };
        let endpoint = endpoint
            .parse::<SocketAddr>()
            .map_err(|error| Error::Http(format!("peer endpoint {endpoint:?}: {error}")))?;
        return Ok(Some((peer.clone(), endpoint)));
    }
    Ok(None)
}

/// Returns LAN `ip:port` hints for a bound UDP socket. When the socket is
/// bound to `0.0.0.0`, every active non-loopback IPv4 interface is advertised
/// (except the nuvia overlay itself), which lets peers on the same physical
/// LAN reach each other even when the router does not support NAT hairpinning.
pub fn local_endpoint_hints(bind: SocketAddr) -> Vec<SocketAddr> {
    if !bind.ip().is_unspecified() {
        return vec![bind];
    }

    let port = bind.port();
    let mut endpoints = Vec::new();
    if let Ok(interfaces) = getifaddrs() {
        for interface in interfaces {
            if interface.flags.contains(InterfaceFlags::LOOPBACK) {
                continue;
            }
            let Some(IpAddr::V4(address)) = interface.address.ip_addr() else {
                continue;
            };
            if address.is_loopback() || address.is_unspecified() || is_overlay_address(address) {
                continue;
            }
            let endpoint = SocketAddr::new(IpAddr::V4(address), port);
            if !endpoints.contains(&endpoint) {
                endpoints.push(endpoint);
            }
        }
    }
    endpoints.sort();
    endpoints
}

fn is_overlay_address(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    octets[0] == 10 && octets[1] == 240
}

/// All direct-path candidates for the given peers: the public endpoint plus
/// the LAN hints. The mesh sends to all of them and keeps whichever works.
pub fn peer_endpoints(peers: &[PeerInfo]) -> Vec<SocketAddr> {
    let mut endpoints = Vec::new();
    for peer in peers {
        for value in peer.endpoint.iter().chain(peer.local_endpoints.iter()) {
            let Ok(endpoint) = value.parse::<SocketAddr>() else {
                continue;
            };
            if !endpoints.contains(&endpoint) {
                endpoints.push(endpoint);
            }
        }
    }
    endpoints.sort();
    endpoints
}

pub async fn resolve_udp(value: &str) -> Result<SocketAddr> {
    resolve_udp_prefer(value, true).await
}

/// Resolves `value` preferring the same address family as the bound socket.
///
/// Windows often returns an IPv6 candidate first for a dual-stack name; sending
/// that through an IPv4 UDP socket fails with WSAEAFNOSUPPORT (os error 10047).
async fn resolve_udp_for_socket(value: &str, socket: &UdpSocket) -> Result<SocketAddr> {
    let prefer_ipv4 = socket
        .local_addr()
        .map(|address| address.is_ipv4())
        .unwrap_or(true);
    resolve_udp_prefer(value, prefer_ipv4).await
}

async fn resolve_udp_prefer(value: &str, prefer_ipv4: bool) -> Result<SocketAddr> {
    let addresses = tokio::net::lookup_host(value)
        .await
        .map_err(|error| Error::Http(format!("resolving {value:?}: {error}")))?;

    for address in addresses {
        if address.is_ipv4() == prefer_ipv4 {
            return Ok(address);
        }
    }

    Err(Error::config(format!(
        "no {} address found for {value:?}",
        if prefer_ipv4 { "IPv4" } else { "IPv6" }
    )))
}

/// Defaults the UDP endpoint to the HTTP host on port 8443.
pub fn default_server_udp(server_url: &str) -> Result<String> {
    let url = reqwest::Url::parse(server_url)
        .map_err(|error| Error::config(format!("invalid server URL: {error}")))?;
    let host = url
        .host_str()
        .ok_or_else(|| Error::config("server URL has no host"))?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.contains(':') {
        Ok(format!("[{host}]:{DEFAULT_SERVER_UDP_PORT}"))
    } else {
        Ok(format!("{host}:{DEFAULT_SERVER_UDP_PORT}"))
    }
}

pub fn generate_token() -> [u8; 16] {
    let mut token = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut token);
    token
}

/// Parses a 16-byte token from hex.
pub fn parse_token(value: &str) -> Result<[u8; 16]> {
    parse_hex::<16>(value).map_err(|error| Error::config(format!("invalid room token: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_udp_defaults() {
        assert_eq!(
            default_server_udp("http://127.0.0.1:4646").unwrap(),
            "127.0.0.1:8443"
        );
        assert_eq!(
            default_server_udp("https://relay.example.com").unwrap(),
            "relay.example.com:8443"
        );
        assert_eq!(
            default_server_udp("https://[::1]:4646").unwrap(),
            "[::1]:8443"
        );
    }

    #[test]
    fn picks_peer_with_endpoint() {
        let response = RegisterResponse {
            protocol: PROTOCOL_VERSION,
            room_id: "00".repeat(16),
            self_endpoint: Some("127.0.0.1:42424".into()),
            peers: vec![
                PeerInfo {
                    peer_id: "aa".repeat(32),
                    endpoint: None,
                    local_endpoints: Vec::new(),
                    name: None,
                },
                PeerInfo {
                    peer_id: "bb".repeat(32),
                    endpoint: Some("10.0.0.2:42424".into()),
                    local_endpoints: vec!["192.168.0.2:42424".into()],
                    name: None,
                },
            ],
            ttl_secs: 60,
        };
        let (peer, endpoint) = pick_peer(&response, None).unwrap().unwrap();
        assert_eq!(peer.peer_id, "bb".repeat(32));
        assert_eq!(endpoint.port(), 42424);
    }

    #[test]
    fn peer_endpoints_include_lan_hints_without_duplicates() {
        let peers = vec![PeerInfo {
            peer_id: "cc".repeat(32),
            endpoint: Some("203.0.113.7:42424".into()),
            local_endpoints: vec!["192.168.0.2:42424".into(), "203.0.113.7:42424".into()],
            name: None,
        }];
        let endpoints = peer_endpoints(&peers);
        assert_eq!(endpoints.len(), 2);
        assert!(endpoints.contains(&"192.168.0.2:42424".parse().unwrap()));
        assert!(endpoints.contains(&"203.0.113.7:42424".parse().unwrap()));
    }

    #[test]
    fn explicit_bind_keeps_its_own_endpoint() {
        let bind: SocketAddr = "127.0.0.1:42424".parse().unwrap();
        assert_eq!(local_endpoint_hints(bind), vec![bind]);
    }
}
