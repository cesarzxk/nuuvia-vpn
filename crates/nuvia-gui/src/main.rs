#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod backend;
mod config;
mod theme;
mod tray;

use eframe::egui;

fn main() -> eframe::Result<()> {
    #[cfg(target_os = "linux")]
    if let Some(socket) = tray::helper_socket_arg() {
        std::process::exit(tray::run_helper(&socket));
    }

    init_logging();

    let renderer = {
        #[cfg(windows)]
        {
            eframe::Renderer::Wgpu
        }
        #[cfg(not(windows))]
        {
            eframe::Renderer::Glow
        }
    };

    let options = eframe::NativeOptions {
        persist_window: false,
        renderer,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([380.0, 540.0])
            .with_min_inner_size([360.0, 480.0])
            .with_title("NUVIA VPN")
            .with_app_id("nuvia")
            .with_icon(icon()),
        ..Default::default()
    };

    let result = eframe::run_native(
        "NUVIA",
        options,
        Box::new(|cc| {
            theme::install(&cc.egui_ctx);
            Ok(Box::new(app::NuviaApp::new(cc)))
        }),
    );
    if let Err(error) = &result {
        tracing::error!(%error, "eframe exited with an error");
    }
    result
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    #[cfg(windows)]
    {
        let path = backend::user_identity_path().with_file_name("nuvia.log");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let writer = move || file.try_clone().expect("clone log file");
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .with_ansi(false)
                .with_writer(writer)
                .init();
            tracing::info!(
                version = env!("CARGO_PKG_VERSION"),
                path = %path.display(),
                "starting nuvia-gui"
            );
            std::panic::set_hook(Box::new(|info| {
                tracing::error!("panic: {info}");
            }));
            return;
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

pub(crate) fn icon() -> egui::IconData {
    const SIZE: usize = 256;
    egui::IconData {
        rgba: include_bytes!("../assets/nuvia-icon.rgba").to_vec(),
        width: SIZE as u32,
        height: SIZE as u32,
    }
}
