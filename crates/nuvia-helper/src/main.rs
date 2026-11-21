use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use clap::Parser;
use nuvia_core::helper::{self, HelperState, StartRequest};
#[cfg(windows)]
use nuvia_core::tunnel::configure_windows_interface_with;
use nuvia_core::tunnel::{open_tap_device, RouteGuard, TapConfig};
use rand::RngCore;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, Semaphore};

#[cfg(windows)]
mod service;

#[derive(Parser)]
#[command(
    name = "nuvia-helper",
    version,
    about = "Privileged TAP helper for Nuuvia"
)]
struct Cli {
    /// Run in the foreground (the default on Linux).
    #[arg(long)]
    foreground: bool,
    /// Run as a Windows service (installed by the installer).
    #[arg(long)]
    service: bool,
    /// Install/update the Windows service (requires administrator).
    #[arg(long)]
    install_service: bool,
    /// Remove the Windows service (requires administrator).
    #[arg(long)]
    uninstall_service: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.foreground {
        tracing::debug!("starting helper in foreground mode");
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    #[cfg(windows)]
    {
        if cli.install_service {
            return install_service();
        }
        if cli.uninstall_service {
            return uninstall_service();
        }
        if cli.service {
            return service::run();
        }
    }

    #[cfg(not(windows))]
    if cli.service || cli.install_service || cli.uninstall_service {
        anyhow::bail!("service options are only supported on Windows");
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("creating helper runtime")?;
    runtime.block_on(run_server(None))
}

async fn run_server(shutdown: Option<Arc<Notify>>) -> Result<()> {
    let state_path = helper::state_path();
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("binding the helper loopback socket")?;
    let port = listener.local_addr()?.port();
    let token = random_token();
    write_state(
        &state_path,
        &HelperState {
            port,
            token: token.clone(),
        },
    )?;

    tracing::info!(
        port,
        state = %state_path.display(),
        "nuvia helper listening"
    );

    let semaphore = Arc::new(Semaphore::new(1));
    loop {
        let accepted = tokio::select! {
            result = listener.accept() => result,
            _ = wait_for_shutdown(&shutdown) => break,
        };
        let (stream, _) = match accepted {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error, "helper accept failed");
                continue;
            }
        };

        let Ok(permit) = semaphore.clone().try_acquire_owned() else {
            let mut stream = stream;
            let _ = stream.write_all(b"busy\n").await;
            continue;
        };

        let token = token.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_client(stream, &token, permit).await {
                tracing::warn!(%error, "helper session failed");
            }
        });
    }

    let _ = std::fs::remove_file(&state_path);
    Ok(())
}

async fn serve_client(
    stream: TcpStream,
    token: &str,
    _permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if line.trim() != token {
        reader.get_mut().write_all(b"denied\n").await?;
        return Ok(());
    }
    reader.get_mut().write_all(b"ok\n").await?;

    line.clear();
    reader.read_line(&mut line).await?;
    let request: StartRequest =
        serde_json::from_str(line.trim()).context("parsing helper start request")?;
    let ipv4: Ipv4Addr = request.ipv4.parse().context("parsing helper IPv4")?;
    if let Err(error) = validate_request(&request, ipv4) {
        let message = format!("error {error}\n");
        reader.get_mut().write_all(message.as_bytes()).await?;
        return Ok(());
    }
    let config = TapConfig {
        name: request.tap_name.clone(),
        ipv4,
        prefix: request.prefix,
        mtu: request.mtu,
    };

    let device = match open_tap_device(&config).await {
        Ok(device) => device,
        Err(error) => {
            let message = format!("error {error}\n");
            reader.get_mut().write_all(message.as_bytes()).await?;
            return Ok(());
        }
    };
    #[cfg(windows)]
    {
        let gui = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|parent| parent.join("nuvia-gui.exe")));
        configure_windows_interface_with(&request.tap_name, gui.as_deref());
    }
    let _routes = RouteGuard::install(&request.tap_name);

    let stream = reader.into_inner();
    let mut stream = stream;
    stream.write_all(b"started\n").await?;
    let (mut read_half, mut write_half) = stream.into_split();

    let device = Arc::new(device);
    let client_device = device.clone();
    let (closed_tx, mut closed_rx) = tokio::sync::mpsc::channel(1);
    let client_task = tokio::spawn(async move {
        while let Ok(frame) = helper::read_frame(&mut read_half).await {
            if client_device.send(&frame).await.is_err() {
                break;
            }
        }
        let _ = closed_tx.send(()).await;
    });

    let mut buffer = vec![0u8; 65_535];
    loop {
        tokio::select! {
            result = device.recv(&mut buffer) => {
                match result {
                    Ok(0) => continue,
                    Ok(length) => {
                        if helper::write_frame(&mut write_half, &buffer[..length])
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::debug!(%error, "helper TAP read ended");
                        break;
                    }
                }
            }
            _ = closed_rx.recv() => break,
        }
    }
    client_task.abort();
    Ok(())
}

async fn wait_for_shutdown(shutdown: &Option<Arc<Notify>>) {
    match shutdown {
        Some(notify) => notify.notified().await,
        None => {
            #[cfg(unix)]
            {
                let mut terminate =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("installing SIGTERM handler");
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
}

fn validate_request(request: &StartRequest, ipv4: Ipv4Addr) -> Result<()> {
    ensure!(
        request.tap_name == nuvia_core::tunnel::shared_tap_name(),
        "unexpected TAP name"
    );
    ensure!(request.prefix == 16, "unexpected TAP prefix");
    ensure!((576..=9000).contains(&request.mtu), "invalid TAP MTU");
    ensure!(
        ipv4.octets()[..2] == [10, 240],
        "TAP address outside 10.240.0.0/16"
    );
    Ok(())
}

#[cfg(windows)]
fn install_service() -> Result<()> {
    let executable = std::env::current_exe().context("locating nuvia-helper.exe")?;
    let binary = format!("\"{}\" --service", executable.display());
    let _ = std::process::Command::new("sc.exe")
        .args(["stop", "nuvia-helper"])
        .status();
    let _ = std::process::Command::new("sc.exe")
        .args(["delete", "nuvia-helper"])
        .status();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let status = std::process::Command::new("sc.exe")
        .args([
            "create",
            "nuvia-helper",
            "binPath=",
            &binary,
            "start=",
            "auto",
        ])
        .status()
        .context("running sc create")?;
    ensure!(status.success(), "sc create failed");
    let status = std::process::Command::new("sc.exe")
        .args(["start", "nuvia-helper"])
        .status()
        .context("running sc start")?;
    ensure!(status.success(), "sc start failed");
    Ok(())
}

#[cfg(windows)]
fn uninstall_service() -> Result<()> {
    let _ = std::process::Command::new("sc.exe")
        .args(["stop", "nuvia-helper"])
        .status();
    let status = std::process::Command::new("sc.exe")
        .args(["delete", "nuvia-helper"])
        .status()
        .context("running sc delete")?;
    ensure!(status.success(), "sc delete failed");
    Ok(())
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn write_state(path: &Path, state: &HelperState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let text = serde_json::to_string(state)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o644))?;
    }
    std::fs::rename(&temporary, path)?;
    #[cfg(windows)]
    {
        if let Some(parent) = path.parent() {
            let _ = std::process::Command::new("icacls")
                .arg(parent)
                .args(["/grant", "*S-1-5-32-545:RX"])
                .status();
        }
        let _ = std::process::Command::new("icacls")
            .arg(path)
            .args(["/grant", "*S-1-5-32-545:R"])
            .status();
    }
    Ok(())
}
