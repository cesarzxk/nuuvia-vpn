#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod apps;
mod backend;
mod config;
mod i18n;
mod instance;
mod theme;
mod tray;

use std::sync::Arc;

use eframe::egui;

fn main() -> eframe::Result<()> {
    #[cfg(target_os = "linux")]
    if let Some((socket, lang)) = tray::helper_args() {
        std::process::exit(tray::run_helper(&socket, lang));
    }

    init_logging();

    let Some(instance) = instance::SingleInstance::acquire() else {
        tracing::info!("another instance is running; activating its window");
        instance::SingleInstance::activate_existing();
        return Ok(());
    };
    let instance = Arc::new(instance);

    #[cfg(windows)]
    if is_software_copy() {
        // This copy lives next to Mesa's `opengl32.dll`. Force the CPU
        // renderer so a half-working driver cannot make Mesa pick a broken
        // hardware path.
        std::env::set_var("GALLIUM_DRIVER", "llvmpipe");
    }

    #[cfg(windows)]
    let result = run_windows(instance.clone());
    #[cfg(not(windows))]
    let result = run(eframe::Renderer::Glow, instance.clone());

    if let Err(error) = &result {
        tracing::error!(%error, "eframe exited with an error");
    }
    result
}

/// Picks a renderer that also works on older Windows machines.
///
/// `wgpu` needs a DX12/Vulkan-capable GPU and a recent driver; on older
/// hardware it fails before the window is created and the app looks like it
/// does nothing. OpenGL (glow) only needs OpenGL 2.0 and has much broader
/// support, so it is the default. The environment variable `NUVIA_RENDERER`
/// can force `wgpu`/`dx12`/`vulkan` or `glow`/`opengl`. If the preferred
/// renderer fails, the other one is tried; if both fail, the copy in `soft\`
/// is started with Mesa's software OpenGL (llvmpipe).
#[cfg(windows)]
fn run_windows(instance: Arc<instance::SingleInstance>) -> eframe::Result<()> {
    let preferred = std::env::var("NUVIA_RENDERER")
        .unwrap_or_default()
        .to_ascii_lowercase();

    let result = match preferred.as_str() {
        "wgpu" | "dx12" | "vulkan" => match run(eframe::Renderer::Wgpu, instance.clone()) {
            Ok(()) => Ok(()),
            Err(error) => {
                tracing::warn!(%error, "wgpu renderer failed; retrying with OpenGL");
                run(eframe::Renderer::Glow, instance.clone())
            }
        },
        _ => match run(eframe::Renderer::Glow, instance.clone()) {
            Ok(()) => Ok(()),
            Err(error) => {
                tracing::warn!(%error, "OpenGL renderer failed; retrying with wgpu");
                run(eframe::Renderer::Wgpu, instance.clone())
            }
        },
    };

    if let Err(error) = &result {
        tracing::error!(%error, "both hardware renderers failed");
        instance.release();
        if relaunch_with_software_gl() {
            std::process::exit(0);
        }
    }
    result
}

/// True when this executable is the compatibility copy in `soft\`, identified
/// by Mesa's `opengl32.dll` sitting next to it.
#[cfg(windows)]
fn is_software_copy() -> bool {
    if std::env::var_os("NUVIA_SOFTWARE_GL").is_some() {
        return true;
    }
    std::env::current_exe()
        .ok()
        .and_then(|executable| {
            executable
                .parent()
                .map(|directory| directory.join("opengl32.dll").is_file())
        })
        .unwrap_or(false)
}

/// Last-resort fallback for machines with neither OpenGL 2.0 nor DX12/Vulkan.
///
/// The installer keeps a copy of this executable in `soft\`, next to Mesa's
/// software OpenGL DLLs. Running that copy makes Windows load llvmpipe before
/// the GUI starts. The `NUVIA_SOFTWARE_GL` environment variable prevents an
/// endless relaunch loop if even the software renderer fails.
#[cfg(windows)]
fn relaunch_with_software_gl() -> bool {
    if std::env::var_os("NUVIA_SOFTWARE_GL").is_some() {
        return false;
    }

    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    let Some(name) = executable.file_name() else {
        return false;
    };
    let Some(directory) = executable.parent() else {
        return false;
    };
    let candidate = directory.join("soft").join(name);
    if !candidate.is_file() {
        return false;
    }

    match std::process::Command::new(&candidate)
        .env("NUVIA_SOFTWARE_GL", "1")
        .env("GALLIUM_DRIVER", "llvmpipe")
        .env_remove("NUVIA_RENDERER")
        .env_remove("WGPU_BACKEND")
        .env_remove("MESA_GL_VERSION_OVERRIDE")
        .spawn()
    {
        Ok(_) => {
            tracing::info!(
                path = %candidate.display(),
                "relaunching with software OpenGL"
            );
            true
        }
        Err(error) => {
            tracing::warn!(
                %error,
                path = %candidate.display(),
                "could not start software OpenGL fallback"
            );
            false
        }
    }
}

fn run(renderer: eframe::Renderer, instance: Arc<instance::SingleInstance>) -> eframe::Result<()> {
    tracing::info!(?renderer, "starting GUI renderer");

    let options = eframe::NativeOptions {
        persist_window: false,
        renderer,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([380.0, 540.0])
            .with_min_inner_size([360.0, 480.0])
            .with_title("Nuuvia VPN")
            .with_app_id("nuvia")
            .with_icon(icon()),
        ..Default::default()
    };

    eframe::run_native(
        "Nuuvia",
        options,
        Box::new(|cc| {
            theme::install(&cc.egui_ctx);
            Ok(Box::new(app::NuviaApp::new(cc, instance)))
        }),
    )
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
