//! Small port-based firewall for the virtual LAN.
//!
//! The GUI uses it per room, before frames are encrypted and after they are
//! decrypted. It is deliberately not a replacement for the operating system
//! firewall: it only controls IPv4 TCP/UDP/ICMP traffic inside the room. ARP is
//! always allowed because it is needed to resolve the virtual Ethernet segment.

use std::collections::HashMap;
use std::fmt;
use std::net::Ipv4Addr;
use std::str::FromStr;
use std::time::{Duration, Instant};

/// Ports commonly used by LAN games. Minecraft Java discovery/connection,
/// Bedrock discovery, mDNS/SSDP, Source/Steam, Xbox and PSN.
pub const GAMES_RULES: &str = "udp:4445,tcp:25565,udp:25565,udp:19132-19133,udp:5353,udp:1900,udp:27015-27036,tcp:27015-27036,udp:3074,tcp:3074,udp:3478-3480,icmp";

const FLOW_TTL: Duration = Duration::from_secs(120);
const MAX_FLOWS: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirewallMode {
    All,
    Games,
    Block,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirewallProfile {
    pub mode: FirewallMode,
    /// Custom allowlist. Ignored for the presets.
    pub rules: String,
}

impl Default for FirewallProfile {
    fn default() -> Self {
        Self::all()
    }
}

impl FirewallProfile {
    pub fn all() -> Self {
        Self {
            mode: FirewallMode::All,
            rules: String::new(),
        }
    }

    pub fn games() -> Self {
        Self {
            mode: FirewallMode::Games,
            rules: String::new(),
        }
    }

    pub fn block() -> Self {
        Self {
            mode: FirewallMode::Block,
            rules: String::new(),
        }
    }

    pub fn custom(rules: impl Into<String>) -> Self {
        Self {
            mode: FirewallMode::Custom,
            rules: rules.into(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.mode == FirewallMode::Custom {
            parse_rules(&self.rules).map(|_| ())
        } else {
            Ok(())
        }
    }
}

impl fmt::Display for FirewallProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.mode {
            FirewallMode::All => f.write_str("all"),
            FirewallMode::Games => f.write_str("games"),
            FirewallMode::Block => f.write_str("block"),
            FirewallMode::Custom if self.rules.trim().is_empty() => f.write_str("custom"),
            FirewallMode::Custom => write!(f, "custom:{}", self.rules.trim()),
        }
    }
}

impl FromStr for FirewallProfile {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        let lowered = value.to_ascii_lowercase();
        match lowered.as_str() {
            "all" | "allow" | "open" => return Ok(Self::all()),
            "games" | "game" => return Ok(Self::games()),
            "block" | "blocked" | "deny" => return Ok(Self::block()),
            "custom" => return Ok(Self::custom("")),
            _ => {}
        }
        if let Some(rules) = lowered.strip_prefix("custom:") {
            let profile = Self::custom(rules.trim());
            profile.validate()?;
            return Ok(profile);
        }
        Err(format!("unknown firewall profile: {value}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PortRule {
    Tcp(u16, u16),
    Udp(u16, u16),
    Icmp,
}

impl PortRule {
    fn matches(self, protocol: u8, port: u16) -> bool {
        match self {
            Self::Tcp(start, end) => protocol == 6 && (start..=end).contains(&port),
            Self::Udp(start, end) => protocol == 17 && (start..=end).contains(&port),
            Self::Icmp => false,
        }
    }
}

fn parse_rules(text: &str) -> Result<Vec<PortRule>, String> {
    let mut rules = Vec::new();
    for token in text.split(|c: char| c == ',' || c == ';' || c.is_whitespace()) {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let lowered = token.to_ascii_lowercase();
        if lowered == "icmp" {
            rules.push(PortRule::Icmp);
            continue;
        }
        let (protocol, range) = if let Some(rest) = lowered.strip_prefix("tcp:") {
            (Some(6u8), rest)
        } else if let Some(rest) = lowered.strip_prefix("udp:") {
            (Some(17u8), rest)
        } else {
            (None, lowered.as_str())
        };
        let (start, end) = parse_range(range)?;
        match protocol {
            Some(6) => rules.push(PortRule::Tcp(start, end)),
            Some(17) => rules.push(PortRule::Udp(start, end)),
            _ => {
                // A bare port/range means both TCP and UDP.
                rules.push(PortRule::Tcp(start, end));
                rules.push(PortRule::Udp(start, end));
            }
        }
    }
    Ok(rules)
}

fn parse_range(value: &str) -> Result<(u16, u16), String> {
    let (start, end) = match value.split_once('-') {
        Some((start, end)) => (start.trim(), end.trim()),
        None => (value, value),
    };
    let start: u16 = start
        .parse()
        .map_err(|_| format!("invalid port: {start}"))?;
    let end: u16 = end.parse().map_err(|_| format!("invalid port: {end}"))?;
    if start == 0 || end == 0 {
        return Err("ports must be between 1 and 65535".into());
    }
    if start > end {
        return Err(format!("invalid port range: {start}-{end}"));
    }
    Ok((start, end))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FlowKey {
    protocol: u8,
    local_ip: Ipv4Addr,
    local_port: u16,
    remote_ip: Ipv4Addr,
    remote_port: u16,
}

struct Ipv4Packet {
    protocol: u8,
    src: Ipv4Addr,
    dst: Ipv4Addr,
    ports: Option<(u16, u16)>,
    icmp_type: Option<u8>,
    fragmented: bool,
}

fn ether_type(frame: &[u8]) -> Option<u16> {
    if frame.len() < 14 {
        return None;
    }
    Some(u16::from_be_bytes([frame[12], frame[13]]))
}

fn parse_ipv4(frame: &[u8]) -> Option<Ipv4Packet> {
    if frame.len() < 34 || ether_type(frame)? != 0x0800 {
        return None;
    }
    let header_len = ((frame[14] & 0x0f) as usize) * 4;
    if header_len < 20 || frame.len() < 14 + header_len {
        return None;
    }
    let total_len = u16::from_be_bytes([frame[16], frame[17]]) as usize;
    if total_len < header_len || frame.len() < 14 + total_len {
        return None;
    }
    let protocol = frame[23];
    let src = Ipv4Addr::new(frame[26], frame[27], frame[28], frame[29]);
    let dst = Ipv4Addr::new(frame[30], frame[31], frame[32], frame[33]);
    let fragment = u16::from_be_bytes([frame[20], frame[21]]);
    let fragmented = (fragment & 0x3fff) != 0;
    let payload = 14 + header_len;
    let (ports, icmp_type) = match protocol {
        6 | 17 => {
            if frame.len() < payload + 4 {
                return None;
            }
            (
                Some((
                    u16::from_be_bytes([frame[payload], frame[payload + 1]]),
                    u16::from_be_bytes([frame[payload + 2], frame[payload + 3]]),
                )),
                None,
            )
        }
        1 => {
            if frame.len() < payload + 1 {
                return None;
            }
            (None, Some(frame[payload]))
        }
        _ => (None, None),
    };
    Some(Ipv4Packet {
        protocol,
        src,
        dst,
        ports,
        icmp_type,
        fragmented,
    })
}

/// Stateful port filter for one room. TCP/UDP replies are admitted when they
/// match a recently allowed flow; a peer can still initiate a connection to a
/// local port that the profile allows.
pub struct Firewall {
    mode: FirewallMode,
    rules: Vec<PortRule>,
    flows: HashMap<FlowKey, Instant>,
}

impl Firewall {
    pub fn new(profile: FirewallProfile) -> Result<Self, String> {
        let rules = match profile.mode {
            FirewallMode::Games => parse_rules(GAMES_RULES)?,
            FirewallMode::Custom => parse_rules(&profile.rules)?,
            FirewallMode::All | FirewallMode::Block => Vec::new(),
        };
        Ok(Self {
            mode: profile.mode,
            rules,
            flows: HashMap::new(),
        })
    }

    pub fn allow_outbound(&mut self, frame: &[u8]) -> bool {
        self.allow(frame, true)
    }

    pub fn allow_inbound(&mut self, frame: &[u8]) -> bool {
        self.allow(frame, false)
    }

    fn allow(&mut self, frame: &[u8], outbound: bool) -> bool {
        match self.mode {
            FirewallMode::All => return true,
            FirewallMode::Block => return ether_type(frame) == Some(0x0806),
            FirewallMode::Games | FirewallMode::Custom => {}
        }

        let Some(ether) = ether_type(frame) else {
            return false;
        };
        // ARP is required for the virtual Ethernet segment.
        if ether == 0x0806 {
            return true;
        }
        if ether != 0x0800 {
            return false;
        }
        let Some(packet) = parse_ipv4(frame) else {
            return false;
        };
        if packet.fragmented {
            return false;
        }
        if packet.protocol == 1 {
            // Only echo request/reply (ping); never redirects or error messages.
            let echo = matches!(packet.icmp_type, Some(0 | 8));
            return echo
                && (self.mode == FirewallMode::Games || self.rules.contains(&PortRule::Icmp));
        }
        let Some((sport, dport)) = packet.ports else {
            return false;
        };

        let (local_ip, local_port, remote_ip, remote_port) = if outbound {
            (packet.src, sport, packet.dst, dport)
        } else {
            (packet.dst, dport, packet.src, sport)
        };
        let key = FlowKey {
            protocol: packet.protocol,
            local_ip,
            local_port,
            remote_ip,
            remote_port,
        };
        if let Some(seen) = self.flows.get_mut(&key) {
            *seen = Instant::now();
            return true;
        }
        if self
            .rules
            .iter()
            .any(|rule| rule.matches(packet.protocol, dport))
        {
            self.remember(key);
            return true;
        }
        false
    }

    fn remember(&mut self, key: FlowKey) {
        if self.flows.len() >= MAX_FLOWS {
            self.flows.retain(|_, seen| seen.elapsed() < FLOW_TTL);
            if self.flows.len() >= MAX_FLOWS {
                self.flows.clear();
            }
        }
        self.flows.insert(key, Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ipv4(protocol: u8, src: [u8; 4], dst: [u8; 4], sport: u16, dport: u16) -> Vec<u8> {
        let mut frame = vec![0u8; 14 + 20 + 8];
        frame[12] = 0x08;
        frame[13] = 0x00;
        frame[14] = 0x45;
        frame[16..18].copy_from_slice(&28u16.to_be_bytes());
        frame[22] = 64;
        frame[23] = protocol;
        frame[26..30].copy_from_slice(&src);
        frame[30..34].copy_from_slice(&dst);
        frame[34..36].copy_from_slice(&sport.to_be_bytes());
        frame[36..38].copy_from_slice(&dport.to_be_bytes());
        frame
    }

    fn arp() -> Vec<u8> {
        let mut frame = vec![0u8; 42];
        frame[12] = 0x08;
        frame[13] = 0x06;
        frame
    }

    #[test]
    fn profile_roundtrip_and_validation() {
        for profile in [
            FirewallProfile::all(),
            FirewallProfile::games(),
            FirewallProfile::block(),
            FirewallProfile::custom("tcp:25565,udp:4445,icmp"),
        ] {
            let text = profile.to_string();
            let parsed: FirewallProfile = text.parse().unwrap();
            assert_eq!(parsed, profile);
        }
        assert!(FirewallProfile::custom("tcp:0").validate().is_err());
        assert!(FirewallProfile::custom("udp:9000-8000").validate().is_err());
        assert!(FirewallProfile::custom("nope:22").validate().is_err());
    }

    #[test]
    fn allow_all_and_block() {
        let mut all = Firewall::new(FirewallProfile::all()).unwrap();
        let frame = ipv4(6, [10, 240, 1, 2], [10, 240, 1, 3], 50000, 445);
        assert!(all.allow_inbound(&frame));
        assert!(all.allow_outbound(&frame));

        let mut block = Firewall::new(FirewallProfile::block()).unwrap();
        assert!(!block.allow_inbound(&frame));
        assert!(!block.allow_outbound(&frame));
        assert!(block.allow_inbound(&arp()));
    }

    #[test]
    fn games_allows_minecraft_and_tracks_replies() {
        let mut firewall = Firewall::new(FirewallProfile::games()).unwrap();
        let local = [10, 240, 1, 2];
        let peer = [10, 240, 1, 3];

        // Outbound Minecraft connection and its inbound reply.
        let out = ipv4(6, local, peer, 50123, 25565);
        assert!(firewall.allow_outbound(&out));
        let reply = ipv4(6, peer, local, 25565, 50123);
        assert!(firewall.allow_inbound(&reply));

        // Peer connects to the local Minecraft server; our reply is allowed.
        let incoming = ipv4(6, peer, local, 50200, 25565);
        assert!(firewall.allow_inbound(&incoming));
        let outgoing = ipv4(6, local, peer, 25565, 50200);
        assert!(firewall.allow_outbound(&outgoing));

        // SMB is blocked in both directions.
        let smb_in = ipv4(6, peer, local, 50300, 445);
        assert!(!firewall.allow_inbound(&smb_in));
        let smb_out = ipv4(6, local, peer, 50301, 445);
        assert!(!firewall.allow_outbound(&smb_out));

        // LAN discovery is allowed.
        let discovery = ipv4(17, peer, [224, 0, 2, 60], 4445, 4445);
        assert!(firewall.allow_inbound(&discovery));
    }

    #[test]
    fn custom_rules_and_icmp() {
        let mut tcp = Firewall::new(FirewallProfile::custom("tcp:25565")).unwrap();
        let local = [10, 240, 1, 2];
        let peer = [10, 240, 1, 3];
        assert!(tcp.allow_inbound(&ipv4(6, peer, local, 50000, 25565)));
        assert!(!tcp.allow_inbound(&ipv4(17, peer, local, 50000, 25565)));
        assert!(!tcp.allow_inbound(&ipv4(6, peer, local, 50000, 445)));

        let mut icmp = Firewall::new(FirewallProfile::custom("icmp")).unwrap();
        let mut ping = ipv4(1, peer, local, 0, 0);
        ping[34] = 8;
        assert!(icmp.allow_inbound(&ping));
    }
}
