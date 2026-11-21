#![cfg_attr(windows, windows_subsystem = "windows")]

use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

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
    #[cfg(windows)]
    {
        let log_path = helper::state_path().with_file_name("helper.log");
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let path = log_path.clone();
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .with_target(false)
            .with_ansi(false)
            .with_writer(move || {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .unwrap_or_else(|_| std::fs::File::create("NUL").expect("opening NUL"))
            })
            .init();
    }
    #[cfg(not(windows))]
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
    let existing = helper::read_state();
    let token = existing
        .as_ref()
        .map(|state| state.token.clone())
        .filter(|token| !token.is_empty())
        .unwrap_or_else(random_token);
    let port_hint = existing
        .as_ref()
        .map(|state| state.port)
        .unwrap_or(helper::DEFAULT_HELPER_PORT);
    let listener = match inherited_listener()? {
        Some(listener) => listener,
        None => TcpListener::bind(("127.0.0.1", port_hint))
            .await
            .with_context(|| format!("binding the helper loopback port {port_hint}"))?,
    };
    let port = listener.local_addr()?.port();
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
            result = listener.accept() => Some(result),
            _ = wait_for_shutdown(&shutdown) => None,
            _ = tokio::time::sleep(Duration::from_secs(30)) => {
                if semaphore.available_permits() == 0 {
                    continue;
                }
                tracing::info!("nuvia helper idle; exiting for socket activation");
                break;
            }
        };
        let Some(accepted) = accepted else { break };
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

    Ok(())
}

#[cfg(unix)]
fn inherited_listener() -> Result<Option<TcpListener>> {
    let fds = std::env::var("LISTEN_FDS")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0);
    if fds <= 0 {
        return Ok(None);
    }
    if let Some(pid) = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
    {
        ensure!(pid == std::process::id(), "LISTEN_PID does not match");
    }
    ensure!(fds == 1, "expected exactly one inherited socket");
    use std::os::fd::FromRawFd;
    let listener = unsafe { std::net::TcpListener::from_raw_fd(3) };
    listener.set_nonblocking(true)?;
    Ok(Some(TcpListener::from_std(listener)?))
}

#[cfg(not(unix))]
fn inherited_listener() -> Result<Option<TcpListener>> {
    Ok(None)
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
fn hidden_command(program: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = std::process::Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(windows)]
fn install_service() -> Result<()> {
    let sid = current_user_sid().context("determining the installing user SID")?;
    let config = helper::state_path().with_file_name("helper-user.txt");
    if let Some(parent) = config.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&config, &sid).with_context(|| format!("writing {}", config.display()))?;

    let mut state = helper::read_state().unwrap_or(HelperState {
        port: helper::DEFAULT_HELPER_PORT,
        token: random_token(),
    });
    state.port = helper::DEFAULT_HELPER_PORT;
    write_state(&helper::state_path(), &state)?;

    let executable = std::env::current_exe().context("locating nuvia-helper.exe")?;
    let binary = format!("\"{}\" --service", executable.display());
    let _ = hidden_command("sc.exe")
        .args(["stop", "nuvia-helper"])
        .status();
    let _ = hidden_command("sc.exe")
        .args(["delete", "nuvia-helper"])
        .status();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let status = hidden_command("sc.exe")
        .args([
            "create",
            "nuvia-helper",
            "binPath=",
            &binary,
            "start=",
            "demand",
        ])
        .status()
        .context("running sc create")?;
    ensure!(status.success(), "sc create failed");

    let sddl = format!(
        "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;CCLCSWRP;;;{sid})"
    );
    let status = hidden_command("sc.exe")
        .args(["sdset", "nuvia-helper", &sddl])
        .status()
        .context("running sc sdset")?;
    ensure!(status.success(), "sc sdset failed");
    Ok(())
}

#[cfg(windows)]
fn uninstall_service() -> Result<()> {
    let _ = hidden_command("sc.exe")
        .args(["stop", "nuvia-helper"])
        .status();
    let status = hidden_command("sc.exe")
        .args(["delete", "nuvia-helper"])
        .status()
        .context("running sc delete")?;
    ensure!(status.success(), "sc delete failed");
    let state = helper::state_path();
    let _ = std::fs::remove_file(&state);
    let _ = std::fs::remove_file(state.with_file_name("helper-user.txt"));
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

        let owner = allowed_user().and_then(|name| user_ids(&name));
        let mode = if owner.is_some() { 0o600 } else { 0o644 };
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode))?;
        if let Some((uid, gid)) = owner {
            if let Err(error) = std::os::unix::fs::chown(&temporary, Some(uid), Some(gid)) {
                tracing::warn!(%error, "could not restrict the helper token owner; leaving it readable");
                std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o644))?;
            }
        }
    }

    std::fs::rename(&temporary, path)?;

    #[cfg(windows)]
    {
        let sid = allowed_sid();
        if let Some(sid) = sid {
            if let Some(parent) = path.parent() {
                let _ = hidden_command("icacls")
                    .arg(parent)
                    .args(["/grant:r", &format!("*{sid}:(RX)")])
                    .status();
            }
            let _ = hidden_command("icacls")
                .arg(path)
                .args([
                    "/inheritance:r",
                    "/grant:r",
                    "*S-1-5-18:(F)",
                    "*S-1-5-32-544:(F)",
                    &format!("*{sid}:(R)"),
                ])
                .status();
        } else {
            if let Some(parent) = path.parent() {
                let _ = hidden_command("icacls")
                    .arg(parent)
                    .args(["/grant:r", "*S-1-5-32-545:RX"])
                    .status();
            }
            let _ = hidden_command("icacls")
                .arg(path)
                .args(["/grant:r", "*S-1-5-32-545:R"])
                .status();
        }
    }
    Ok(())
}

#[cfg(unix)]
fn allowed_user() -> Option<String> {
    if let Ok(value) = std::env::var("NUVIA_HELPER_USER") {
        let value = value.trim();
        if !value.is_empty() && value != "root" {
            return Some(value.to_string());
        }
    }
    let value = std::fs::read_to_string("/etc/nuvia/helper-user").ok()?;
    let value = value.trim();
    if value.is_empty() || value == "root" {
        None
    } else {
        Some(value.to_string())
    }
}

#[cfg(unix)]
fn user_ids(name: &str) -> Option<(u32, u32)> {
    let uid = std::process::Command::new("id")
        .args(["-u", name])
        .output()
        .ok()?;
    let gid = std::process::Command::new("id")
        .args(["-g", name])
        .output()
        .ok()?;
    if !uid.status.success() || !gid.status.success() {
        return None;
    }
    Some((
        String::from_utf8_lossy(&uid.stdout).trim().parse().ok()?,
        String::from_utf8_lossy(&gid.stdout).trim().parse().ok()?,
    ))
}

#[cfg(windows)]
fn allowed_sid() -> Option<String> {
    if let Ok(value) = std::env::var("NUVIA_HELPER_SID") {
        let value = value.trim();
        if value.starts_with("S-1-") {
            return Some(value.to_string());
        }
    }
    let value =
        std::fs::read_to_string(helper::state_path().with_file_name("helper-user.txt")).ok()?;
    let value = value.trim();
    value.starts_with("S-1-").then(|| value.to_string())
}

#[cfg(windows)]
fn current_user_sid() -> Option<String> {
    let output = hidden_command("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let sid = text
        .rsplit(',')
        .next()?
        .trim()
        .trim_matches('"')
        .to_string();
    sid.starts_with("S-1-").then_some(sid)
}
