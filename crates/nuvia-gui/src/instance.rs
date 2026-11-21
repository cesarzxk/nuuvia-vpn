//! Single-instance guard and activation channel for the GUI.
//!
//! The first instance holds an exclusive lock on `gui.lock` and listens on a
//! loopback port recorded in `gui-activate.json`. A second launch sends a
//! `show` request to that port and exits, restoring the existing window from
//! the tray or the taskbar.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::backend;

#[derive(Debug, Serialize, Deserialize)]
struct Activation {
    port: u16,
    token: String,
}

pub struct SingleInstance {
    /// Held for the process lifetime; closing it releases the OS lock.
    file: File,
    activation_path: PathBuf,
    started: AtomicBool,
    released: AtomicBool,
}

impl SingleInstance {
    /// Returns `None` when another GUI already owns the per-user lock.
    pub fn acquire() -> Option<Self> {
        let lock_path = backend::user_identity_path().with_file_name("gui.lock");
        Self::acquire_at(&lock_path)
    }

    fn acquire_at(lock_path: &Path) -> Option<Self> {
        if let Some(parent) = lock_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .ok()?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                let activation_path = lock_path.with_file_name("gui-activate.json");
                // A previous process may have exited without cleaning up (for
                // example a forced close); never reuse its address or token.
                let _ = std::fs::remove_file(&activation_path);
                Some(Self {
                    file,
                    activation_path,
                    started: AtomicBool::new(false),
                    released: AtomicBool::new(false),
                })
            }
            Err(_) => None,
        }
    }

    /// Tells the running instance to restore its window.
    pub fn activate_existing() {
        let path = backend::user_identity_path().with_file_name("gui-activate.json");
        // The first instance may still be building its window when the second
        // one launches; retry briefly instead of giving up immediately.
        if !Self::activate_at(&path, Duration::from_millis(1500)) {
            tracing::warn!("the running instance did not answer the activation request");
        }
    }

    fn activate_at(path: &Path, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if Self::send_activation(path) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn send_activation(path: &Path) -> bool {
        let Ok(text) = std::fs::read_to_string(path) else {
            return false;
        };
        let Ok(activation) = serde_json::from_str::<Activation>(&text) else {
            return false;
        };
        let address = SocketAddr::from(([127, 0, 0, 1], activation.port));
        let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(500))
        else {
            return false;
        };
        let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
        stream
            .write_all(format!("{}\n", activation.token).as_bytes())
            .is_ok()
    }

    /// Starts the loopback activation listener and records its address.
    pub fn start_activation(&self, ctx: egui::Context) {
        self.start_activation_with(move || crate::tray::show(&ctx));
    }

    fn start_activation_with(&self, activate: impl Fn() + Send + 'static) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let listener = match TcpListener::bind(("127.0.0.1", 0)) {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(%error, "could not bind the single-instance activation socket");
                return;
            }
        };
        let Ok(address) = listener.local_addr() else {
            return;
        };
        let activation = Activation {
            port: address.port(),
            token: random_token(),
        };
        let Ok(text) = serde_json::to_string(&activation) else {
            return;
        };
        if let Some(parent) = self.activation_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let temporary = self.activation_path.with_extension("json.tmp");
        if std::fs::write(&temporary, text).is_err() {
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600));
        }
        if std::fs::rename(&temporary, &self.activation_path).is_err() {
            return;
        }
        tracing::info!(port = address.port(), "single-instance activation ready");

        let token = activation.token;
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    continue;
                };
                let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                let mut line = String::new();
                let mut reader = BufReader::new(stream);
                if reader.read_line(&mut line).is_ok() && line.trim() == token {
                    activate();
                }
            }
        });
    }

    /// Releases the lock before handing over to the software-OpenGL copy.
    ///
    /// The handoff only exists on Windows, where a renderer failure restarts
    /// the process from the `soft\` directory.
    #[cfg(windows)]
    pub fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        let _ = fs2::FileExt::unlock(&self.file);
        let _ = std::fs::remove_file(&self.activation_path);
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        if !self.released.load(Ordering::SeqCst) {
            let _ = std::fs::remove_file(&self.activation_path);
        }
        // `fs2` releases the lock on close too, but unlocking explicitly keeps
        // the hand-off to the software-OpenGL copy deterministic.
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

fn random_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "nuvia-instance-{name}-{}-{}",
            std::process::id(),
            random_token()
        ));
        std::fs::create_dir_all(&directory).expect("create temp dir");
        directory
    }

    #[test]
    fn lock_is_exclusive_until_dropped() {
        let directory = temp_dir("lock");
        let lock = directory.join("gui.lock");

        let first = SingleInstance::acquire_at(&lock).expect("first instance");
        assert!(SingleInstance::acquire_at(&lock).is_none());
        drop(first);
        assert!(SingleInstance::acquire_at(&lock).is_some());

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn activation_reaches_the_owning_instance() {
        let directory = temp_dir("activation");
        let lock = directory.join("gui.lock");
        let instance = SingleInstance::acquire_at(&lock).expect("first instance");

        let shown = std::sync::Arc::new(AtomicBool::new(false));
        let flag = shown.clone();
        instance.start_activation_with(move || flag.store(true, Ordering::SeqCst));

        assert!(SingleInstance::activate_at(
            &directory.join("gui-activate.json"),
            Duration::from_secs(2)
        ));

        let deadline = Instant::now() + Duration::from_secs(2);
        while !shown.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(shown.load(Ordering::SeqCst));

        drop(instance);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_wrong_token_is_ignored() {
        let directory = temp_dir("token");
        let lock = directory.join("gui.lock");
        let instance = SingleInstance::acquire_at(&lock).expect("first instance");

        let shown = std::sync::Arc::new(AtomicBool::new(false));
        let flag = shown.clone();
        instance.start_activation_with(move || flag.store(true, Ordering::SeqCst));

        let text =
            std::fs::read_to_string(directory.join("gui-activate.json")).expect("activation file");
        let activation: Activation = serde_json::from_str(&text).expect("parse activation");
        let mut stream =
            TcpStream::connect(("127.0.0.1", activation.port)).expect("connect activation");
        stream.write_all(b"not-the-token\n").expect("write token");
        drop(stream);
        std::thread::sleep(Duration::from_millis(200));
        assert!(!shown.load(Ordering::SeqCst));

        drop(instance);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
