//! Local IPC between the unprivileged GUI and the privileged TAP helper.
//!
//! The helper owns the TAP device and the discovery routes. The GUI exchanges
//! raw Ethernet frames with it over a loopback TCP connection protected by a
//! random token written to the helper state file.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::{Error, Result};

/// Largest Ethernet frame accepted over the helper channel.
pub const MAX_FRAME: usize = 65_535;

/// Fixed loopback port used by socket activation and the Windows service.
pub const DEFAULT_HELPER_PORT: u16 = 4647;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperState {
    pub port: u16,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartRequest {
    pub tap_name: String,
    pub ipv4: String,
    pub prefix: u8,
    pub mtu: u16,
}

impl StartRequest {
    pub fn new(tap_name: impl Into<String>, ipv4: Ipv4Addr, prefix: u8, mtu: u16) -> Self {
        Self {
            tap_name: tap_name.into(),
            ipv4: ipv4.to_string(),
            prefix,
            mtu,
        }
    }
}

pub fn state_path() -> PathBuf {
    if let Ok(path) = std::env::var("NUVIA_HELPER_STATE") {
        return PathBuf::from(path);
    }

    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/etc/nuvia/helper.json")
    }

    #[cfg(windows)]
    {
        std::env::var("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(r"C:\ProgramData"))
            .join("Nuuvia")
            .join("helper.json")
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    {
        std::env::temp_dir().join("nuvia-helper.json")
    }
}

pub fn read_state() -> Option<HelperState> {
    let text = std::fs::read_to_string(state_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// The helper writes the state file while it is running and removes it on a
/// clean shutdown.
pub fn available() -> bool {
    read_state().is_some()
}

static HELPER_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn active() -> bool {
    HELPER_ACTIVE.load(Ordering::Relaxed)
}

pub(crate) fn set_active(value: bool) {
    HELPER_ACTIVE.store(value, Ordering::Relaxed);
}

/// Connects to the helper, authenticates, asks it to create the TAP and
/// returns the frame stream.
pub async fn connect(request: &StartRequest) -> Result<TcpStream> {
    let state = read_state().ok_or_else(|| Error::config("nuvia helper is not installed"))?;
    let address = SocketAddr::from(([127, 0, 0, 1], state.port));
    let mut stream =
        match tokio::time::timeout(Duration::from_millis(800), TcpStream::connect(address)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(_error)) => {
                #[cfg(windows)]
                {
                    start_windows_service();
                    connect_windows_retry(address).await?
                }
                #[cfg(not(windows))]
                {
                    return Err(Error::Io(_error));
                }
            }
            Err(_) => {
                #[cfg(windows)]
                {
                    start_windows_service();
                    connect_windows_retry(address).await?
                }
                #[cfg(not(windows))]
                {
                    return Err(Error::Timeout("connecting to the nuvia helper".into()));
                }
            }
        };

    stream
        .write_all(format!("{}\n", state.token).as_bytes())
        .await?;
    let request = serde_json::to_string(request)
        .map_err(|error| Error::config(format!("serializing helper request: {error}")))?;
    let stream = tokio::time::timeout(Duration::from_secs(15), async move {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        if line.trim() != "ok" {
            return Err(Error::config(format!("nuvia helper: {}", line.trim())));
        }

        reader
            .get_mut()
            .write_all(format!("{request}\n").as_bytes())
            .await?;
        line.clear();
        reader.read_line(&mut line).await?;
        if let Some(message) = line.trim().strip_prefix("error ") {
            return Err(Error::config(format!("nuvia helper: {message}")));
        }
        if line.trim() != "started" {
            return Err(Error::config(
                "nuvia helper returned an invalid start reply",
            ));
        }
        Ok(reader.into_inner())
    })
    .await
    .map_err(|_| Error::Timeout("starting the nuvia helper".into()))??;

    set_active(true);
    Ok(stream)
}

#[cfg(windows)]
async fn connect_windows_retry(address: SocketAddr) -> Result<TcpStream> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let error =
            match tokio::time::timeout(Duration::from_millis(500), TcpStream::connect(address))
                .await
            {
                Ok(Ok(stream)) => return Ok(stream),
                Ok(Err(error)) => Error::Io(error),
                Err(_) => Error::Timeout("connecting to the nuvia helper".into()),
            };
        if tokio::time::Instant::now() >= deadline {
            return Err(error);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[cfg(windows)]
fn start_windows_service() {
    let _ = std::process::Command::new("sc.exe")
        .args(["start", "nuvia-helper"])
        .output();
}

pub async fn read_frame<R>(reader: &mut R) -> Result<Vec<u8>>
where
    R: AsyncReadExt + Unpin,
{
    let mut length = [0u8; 4];
    reader.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(Error::protocol(format!(
            "helper frame length {length} is out of range"
        )));
    }
    let mut frame = vec![0u8; length];
    reader.read_exact(&mut frame).await?;
    Ok(frame)
}

pub async fn write_frame<W>(writer: &mut W, frame: &[u8]) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    if frame.is_empty() || frame.len() > MAX_FRAME {
        return Err(Error::protocol(format!(
            "helper frame length {} is out of range",
            frame.len()
        )));
    }
    writer
        .write_all(&(frame.len() as u32).to_be_bytes())
        .await?;
    writer.write_all(frame).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frame_roundtrip() {
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        write_frame(&mut writer, b"frame").await.unwrap();
        assert_eq!(read_frame(&mut reader).await.unwrap(), b"frame");
    }

    #[tokio::test]
    async fn empty_frames_are_rejected() {
        let mut writer = Vec::new();
        assert!(write_frame(&mut writer, b"").await.is_err());
    }
}
