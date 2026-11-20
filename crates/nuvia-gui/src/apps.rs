//! Finds applications with listening TCP/UDP ports so the firewall editor can
//! import their ports as allowlist rules.

use std::collections::BTreeMap;

/// A local port an application is listening on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListeningPort {
    pub port: u16,
    pub tcp: bool,
    pub udp: bool,
}

/// One process and the ports it currently listens on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListeningApp {
    pub pid: u32,
    pub process: String,
    pub ports: Vec<ListeningPort>,
}

impl ListeningApp {
    /// Compact label such as `TCP 25565  ·  UDP 19132-19133`.
    pub fn ports_label(&self) -> String {
        let mut tcp = Vec::new();
        let mut udp = Vec::new();
        for entry in &self.ports {
            if entry.tcp {
                tcp.push(entry.port);
            }
            if entry.udp {
                udp.push(entry.port);
            }
        }
        let mut parts = Vec::new();
        if !tcp.is_empty() {
            parts.push(format!("TCP {}", port_ranges(&tcp)));
        }
        if !udp.is_empty() {
            parts.push(format!("UDP {}", port_ranges(&udp)));
        }
        parts.join("  ·  ")
    }
}

/// Returns every process listening on IPv4 TCP/UDP ports.
pub fn scan() -> Vec<ListeningApp> {
    platform::scan()
}

fn port_ranges(ports: &[u16]) -> String {
    let mut sorted = ports.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut parts = Vec::new();
    let mut index = 0;
    while index < sorted.len() {
        let start = sorted[index];
        let mut end = start;
        while index + 1 < sorted.len() && sorted[index + 1] == end.saturating_add(1) {
            index += 1;
            end = sorted[index];
        }
        if start == end {
            parts.push(start.to_string());
        } else {
            parts.push(format!("{start}-{end}"));
        }
        index += 1;
    }
    parts.join(", ")
}

fn into_apps(map: BTreeMap<(u32, String), BTreeMap<u16, (bool, bool)>>) -> Vec<ListeningApp> {
    let mut apps: Vec<ListeningApp> = map
        .into_iter()
        .map(|((pid, process), ports)| ListeningApp {
            pid,
            process,
            ports: ports
                .into_iter()
                .map(|(port, (tcp, udp))| ListeningPort { port, tcp, udp })
                .collect(),
        })
        .collect();
    apps.sort_by(|left, right| {
        left.process
            .to_lowercase()
            .cmp(&right.process.to_lowercase())
            .then(left.pid.cmp(&right.pid))
    });
    apps
}

#[cfg(target_os = "linux")]
mod platform {
    use std::collections::{BTreeMap, HashMap, HashSet};

    use super::{into_apps, ListeningApp};

    pub fn scan() -> Vec<ListeningApp> {
        let sockets = socket_ports();
        if sockets.is_empty() {
            return Vec::new();
        }
        let owners = socket_owners(&sockets);
        let mut apps: BTreeMap<(u32, String), BTreeMap<u16, (bool, bool)>> = BTreeMap::new();
        for (inode, port, tcp) in sockets {
            if port == 0 {
                continue;
            }
            let Some((pid, process)) = owners.get(&inode) else {
                continue;
            };
            let protocols = apps
                .entry((*pid, process.clone()))
                .or_default()
                .entry(port)
                .or_insert((false, false));
            if tcp {
                protocols.0 = true;
            } else {
                protocols.1 = true;
            }
        }
        into_apps(apps)
    }

    /// Socket inode, local port and protocol for listening TCP sockets and
    /// bound UDP sockets.
    fn socket_ports() -> Vec<(u64, u16, bool)> {
        let mut sockets = Vec::new();
        read_table("/proc/net/tcp", true, &mut sockets);
        read_table("/proc/net/tcp6", true, &mut sockets);
        read_table("/proc/net/udp", false, &mut sockets);
        read_table("/proc/net/udp6", false, &mut sockets);
        sockets
    }

    fn read_table(path: &str, tcp: bool, sockets: &mut Vec<(u64, u16, bool)>) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        for line in text.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 10 {
                continue;
            }
            // TCP only counts sockets in LISTEN; UDP has no such state.
            if tcp && fields[3] != "0A" {
                continue;
            }
            let Some((_, port)) = fields[1].split_once(':') else {
                continue;
            };
            let Ok(port) = u16::from_str_radix(port, 16) else {
                continue;
            };
            let Ok(inode) = fields[9].parse::<u64>() else {
                continue;
            };
            if inode != 0 {
                sockets.push((inode, port, tcp));
            }
        }
    }

    fn socket_owners(sockets: &[(u64, u16, bool)]) -> HashMap<u64, (u32, String)> {
        let wanted: HashSet<u64> = sockets.iter().map(|(inode, _, _)| *inode).collect();
        let mut owners = HashMap::new();
        let Ok(processes) = std::fs::read_dir("/proc") else {
            return owners;
        };
        for process in processes.flatten() {
            let Some(pid) = process
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(descriptors) = std::fs::read_dir(process.path().join("fd")) else {
                continue;
            };
            for descriptor in descriptors.flatten() {
                let Ok(target) = std::fs::read_link(descriptor.path()) else {
                    continue;
                };
                let Some(inode) = socket_inode(&target) else {
                    continue;
                };
                if !wanted.contains(&inode) {
                    continue;
                }
                owners
                    .entry(inode)
                    .or_insert_with(|| (pid, process_name(pid)));
            }
        }
        owners
    }

    fn socket_inode(target: &std::path::Path) -> Option<u64> {
        target
            .to_str()?
            .strip_prefix("socket:[")?
            .strip_suffix(']')?
            .parse()
            .ok()
    }

    fn process_name(pid: u32) -> String {
        std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .ok()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("PID {pid}"))
    }
}

#[cfg(windows)]
mod platform {
    use std::collections::{BTreeMap, HashMap};
    use std::ffi::c_void;

    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCPROW_OWNER_PID, MIB_UDPROW_OWNER_PID,
        TCP_TABLE_OWNER_PID_LISTENER, UDP_TABLE_OWNER_PID,
    };
    use windows_sys::Win32::Networking::WinSock::AF_INET;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    use super::{into_apps, ListeningApp};

    pub fn scan() -> Vec<ListeningApp> {
        let mut apps: BTreeMap<(u32, String), BTreeMap<u16, (bool, bool)>> = BTreeMap::new();
        let mut names = HashMap::new();
        for row in tcp_listeners() {
            let port = network_port(row.dwLocalPort);
            if port == 0 {
                continue;
            }
            let pid = row.dwOwningPid;
            let name = process_name(pid, &mut names);
            apps.entry((pid, name))
                .or_default()
                .entry(port)
                .or_insert((false, false))
                .0 = true;
        }
        for row in udp_sockets() {
            let port = network_port(row.dwLocalPort);
            if port == 0 {
                continue;
            }
            let pid = row.dwOwningPid;
            let name = process_name(pid, &mut names);
            apps.entry((pid, name))
                .or_default()
                .entry(port)
                .or_insert((false, false))
                .1 = true;
        }
        into_apps(apps)
    }

    fn tcp_listeners() -> Vec<MIB_TCPROW_OWNER_PID> {
        let mut size = 0u32;
        let first = unsafe {
            GetExtendedTcpTable(
                std::ptr::null_mut(),
                &mut size,
                0,
                AF_INET as u32,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if size == 0 || (first != 0 && first != ERROR_INSUFFICIENT_BUFFER) {
            return Vec::new();
        }
        let mut buffer = table_buffer(size);
        let result = unsafe {
            GetExtendedTcpTable(
                buffer.as_mut_ptr() as *mut c_void,
                &mut size,
                0,
                AF_INET as u32,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if result != 0 {
            return Vec::new();
        }
        unsafe { table_rows::<MIB_TCPROW_OWNER_PID>(&buffer) }
    }

    fn udp_sockets() -> Vec<MIB_UDPROW_OWNER_PID> {
        let mut size = 0u32;
        let first = unsafe {
            GetExtendedUdpTable(
                std::ptr::null_mut(),
                &mut size,
                0,
                AF_INET as u32,
                UDP_TABLE_OWNER_PID,
                0,
            )
        };
        if size == 0 || (first != 0 && first != ERROR_INSUFFICIENT_BUFFER) {
            return Vec::new();
        }
        let mut buffer = table_buffer(size);
        let result = unsafe {
            GetExtendedUdpTable(
                buffer.as_mut_ptr() as *mut c_void,
                &mut size,
                0,
                AF_INET as u32,
                UDP_TABLE_OWNER_PID,
                0,
            )
        };
        if result != 0 {
            return Vec::new();
        }
        unsafe { table_rows::<MIB_UDPROW_OWNER_PID>(&buffer) }
    }

    fn table_buffer(size: u32) -> Vec<u32> {
        vec![0u32; (size as usize).div_ceil(4)]
    }

    /// The owner tables start with a `u32` row count followed by the rows.
    unsafe fn table_rows<T: Copy>(buffer: &[u32]) -> Vec<T> {
        let count = buffer.first().copied().unwrap_or(0) as usize;
        let rows = buffer.as_ptr().add(1) as *const T;
        std::slice::from_raw_parts(rows, count).to_vec()
    }

    fn network_port(raw: u32) -> u16 {
        u16::from_be(raw as u16)
    }

    fn process_name(pid: u32, cache: &mut HashMap<u32, String>) -> String {
        if let Some(name) = cache.get(&pid) {
            return name.clone();
        }
        let name = if pid == 0 || pid == 4 {
            "System".to_string()
        } else {
            process_path(pid)
                .map(|path| display_name(&path))
                .unwrap_or_else(|| format!("PID {pid}"))
        };
        cache.insert(pid, name.clone());
        name
    }

    fn process_path(pid: u32) -> Option<String> {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return None;
            }
            let mut buffer = [0u16; 1024];
            let mut length = buffer.len() as u32;
            let result = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length);
            CloseHandle(handle);
            if result == 0 || length == 0 {
                return None;
            }
            Some(String::from_utf16_lossy(&buffer[..length as usize]))
        }
    }

    fn display_name(path: &str) -> String {
        let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
        if file.len() > 4 && file[file.len() - 4..].eq_ignore_ascii_case(".exe") {
            file[..file.len() - 4].to_string()
        } else {
            file.to_string()
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    use super::ListeningApp;

    pub fn scan() -> Vec<ListeningApp> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_are_grouped_into_ranges() {
        assert_eq!(
            port_ranges(&[25565, 25567, 25566, 19132, 19132]),
            "19132, 25565-25567"
        );
        assert_eq!(port_ranges(&[65535]), "65535");
        assert_eq!(port_ranges(&[]), "");
    }

    #[test]
    fn labels_group_ports_by_protocol() {
        let app = ListeningApp {
            pid: 42,
            process: "game".into(),
            ports: vec![
                ListeningPort {
                    port: 25565,
                    tcp: true,
                    udp: false,
                },
                ListeningPort {
                    port: 19132,
                    tcp: false,
                    udp: true,
                },
                ListeningPort {
                    port: 19133,
                    tcp: false,
                    udp: true,
                },
            ],
        };
        assert_eq!(app.ports_label(), "TCP 25565  ·  UDP 19132-19133");
    }
}
