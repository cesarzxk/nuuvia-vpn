//! System tray integration.
//!
//! Closing the window hides it instead of dropping the tunnels. The tray icon
//! restores the window or exits the app. On platforms where the tray cannot be
//! created, closing the window exits normally.
//!
//! On Linux the GUI runs as root to create the TAP, but the session D-Bus
//! belongs to the desktop user. In that case a small copy of this binary is
//! started as the session user with `--tray-helper`; it owns the tray icon and
//! talks to the root process over a private Unix socket.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use eframe::egui::{self, ViewportCommand};

use crate::i18n::Lang;

pub struct Tray {
    active: Arc<AtomicBool>,
    lang: Lang,
    #[cfg(target_os = "linux")]
    _linux: Option<LinuxTrayHandle>,
    #[cfg(windows)]
    _windows: Option<WindowsTray>,
}

impl Tray {
    pub fn spawn(ctx: egui::Context, quitting: Arc<AtomicBool>, lang: Lang) -> Self {
        #[cfg(target_os = "linux")]
        {
            return spawn_linux(ctx, quitting, lang);
        }

        #[cfg(windows)]
        {
            return match WindowsTray::new(ctx, quitting, lang) {
                Ok(tray) => {
                    tracing::info!("system tray ready");
                    Self {
                        active: Arc::new(AtomicBool::new(true)),
                        lang,
                        _windows: Some(tray),
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "system tray unavailable; closing exits the app");
                    Self {
                        active: Arc::new(AtomicBool::new(false)),
                        lang,
                        _windows: None,
                    }
                }
            };
        }

        #[allow(unreachable_code)]
        {
            let _ = (ctx, quitting);
            Self {
                active: Arc::new(AtomicBool::new(false)),
                lang,
                #[cfg(target_os = "linux")]
                _linux: None,
                #[cfg(windows)]
                _windows: None,
            }
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    /// Rebuilds the tray menu after the interface language changes.
    pub fn set_language(&mut self, ctx: egui::Context, quitting: Arc<AtomicBool>, lang: Lang) {
        if self.lang == lang {
            return;
        }
        *self = Self::spawn(ctx, quitting, lang);
    }
}

pub(crate) fn show(ctx: &egui::Context) {
    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(ViewportCommand::Focus);
    ctx.request_repaint();
}

fn quit(ctx: &egui::Context, quitting: &AtomicBool) -> ! {
    quitting.store(true, Ordering::SeqCst);
    ctx.request_repaint();
    // The window may be hidden, in which case the event loop does not process
    // a Close command. Exiting here also guarantees that the tray helper and
    // the tunnels die with the GUI.
    std::process::exit(0);
}

#[cfg(target_os = "linux")]
enum LinuxTrayHandle {
    InProcess {
        _handle: ksni::blocking::Handle<LinuxTray>,
    },
    Helper {
        child: std::process::Child,
    },
}

#[cfg(target_os = "linux")]
impl Drop for LinuxTrayHandle {
    fn drop(&mut self) {
        if let Self::Helper { child } = self {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(target_os = "linux")]
fn spawn_linux(ctx: egui::Context, quitting: Arc<AtomicBool>, lang: Lang) -> Tray {
    use ksni::blocking::TrayMethods;

    let active = Arc::new(AtomicBool::new(false));

    if unsafe { libc::geteuid() } == 0 {
        if let Some(child) = spawn_tray_helper(&ctx, &quitting, &active, lang) {
            return Tray {
                active,
                lang,
                _linux: Some(LinuxTrayHandle::Helper { child }),
            };
        }
    } else {
        ensure_session_bus_for_current_uid();
        let tray = LinuxTray {
            ctx,
            quitting,
            icon_theme: ensure_icon_files(),
            lang,
        };
        match tray.spawn() {
            Ok(handle) => {
                tracing::info!("system tray ready");
                active.store(true, Ordering::Relaxed);
                return Tray {
                    active,
                    lang,
                    _linux: Some(LinuxTrayHandle::InProcess { _handle: handle }),
                };
            }
            Err(error) => {
                tracing::warn!(%error, "system tray unavailable; closing exits the app");
            }
        }
    }

    Tray {
        active,
        lang,
        _linux: None,
    }
}

#[cfg(target_os = "linux")]
fn spawn_tray_helper(
    ctx: &egui::Context,
    quitting: &Arc<AtomicBool>,
    active: &Arc<AtomicBool>,
    lang: Lang,
) -> Option<std::process::Child> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;

    let uid = session_uid()?;
    let gid = session_gid().unwrap_or(uid);
    let socket =
        std::env::temp_dir().join(format!("nuvia-tray-{}-{}.sock", uid, std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let listener = std::os::unix::net::UnixListener::bind(&socket).ok()?;
    let _ = std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600));
    chown(&socket, uid, gid);

    let exe = std::env::current_exe().ok()?;
    let mut command = std::process::Command::new(exe);
    command
        .arg("--tray-helper")
        .arg(&socket)
        .arg(lang.code())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .env("XDG_RUNTIME_DIR", format!("/run/user/{uid}"))
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path=/run/user/{uid}/bus"),
        )
        .envs(
            session_home(uid)
                .map(|home| {
                    vec![
                        ("HOME", home.clone()),
                        ("XDG_DATA_HOME", format!("{home}/.local/share")),
                    ]
                })
                .unwrap_or_default(),
        )
        .uid(uid)
        .gid(gid);
    unsafe {
        command.pre_exec(|| {
            libc::setgroups(0, std::ptr::null());
            Ok(())
        });
    }

    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(%error, "could not start tray helper");
            return None;
        }
    };

    let ctx = ctx.clone();
    let quitting = quitting.clone();
    let active = active.clone();
    let socket_path = socket.clone();
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let Ok((stream, _)) = listener.accept() else {
            active.store(false, Ordering::Relaxed);
            return;
        };
        let _ = std::fs::remove_file(&socket_path);
        active.store(true, Ordering::Relaxed);
        tracing::info!("system tray ready via user helper");

        for line in BufReader::new(stream).lines() {
            match line.as_deref() {
                Ok("show") => show(&ctx),
                Ok("quit") => {
                    quit(&ctx, &quitting);
                }
                _ => {}
            }
        }
        active.store(false, Ordering::Relaxed);
    });

    Some(child)
}

#[cfg(target_os = "linux")]
fn session_uid() -> Option<u32> {
    std::env::var("SUDO_UID")
        .ok()
        .or_else(|| std::env::var("PKEXEC_UID").ok())
        .and_then(|value| value.parse().ok())
}

#[cfg(target_os = "linux")]
fn session_gid() -> Option<u32> {
    std::env::var("SUDO_GID")
        .ok()
        .and_then(|value| value.parse().ok())
}

#[cfg(target_os = "linux")]
fn session_home(uid: u32) -> Option<String> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        if fields.nth(2)?.parse::<u32>().ok()? == uid {
            fields.nth(2).map(str::to_string)
        } else {
            None
        }
    })
}

#[cfg(target_os = "linux")]
fn chown(path: &std::path::Path, uid: u32, gid: u32) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    if let Ok(path) = CString::new(path.as_os_str().as_bytes()) {
        unsafe {
            libc::chown(path.as_ptr(), uid as libc::uid_t, gid as libc::gid_t);
        }
    }
}

#[cfg(target_os = "linux")]
fn ensure_session_bus_for_current_uid() {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() {
        return;
    }
    let uid = unsafe { libc::geteuid() };
    let bus = format!("/run/user/{uid}/bus");
    if std::path::Path::new(&bus).exists() {
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={bus}"));
        if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
            std::env::set_var("XDG_RUNTIME_DIR", format!("/run/user/{uid}"));
        }
    }
}

#[cfg(target_os = "linux")]
struct LinuxTray {
    ctx: egui::Context,
    quitting: Arc<AtomicBool>,
    icon_theme: Option<String>,
    lang: Lang,
}

#[cfg(target_os = "linux")]
impl ksni::Tray for LinuxTray {
    fn id(&self) -> String {
        "nuvia".into()
    }

    fn title(&self) -> String {
        self.lang.strings().tray_tooltip.into()
    }

    fn icon_name(&self) -> String {
        "nuvia".into()
    }

    fn icon_theme_path(&self) -> String {
        self.icon_theme.clone().unwrap_or_default()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        tray_icon_pixmap()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        show(&self.ctx);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem};
        let t = self.lang.strings();
        vec![
            StandardItem {
                label: t.tray_open.into(),
                icon_name: "window-new".into(),
                activate: Box::new(|tray: &mut Self| show(&tray.ctx)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: t.tray_quit.into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|tray: &mut Self| quit(&tray.ctx, &tray.quitting)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

#[cfg(target_os = "linux")]
fn ensure_icon_files() -> Option<String> {
    let home = std::path::PathBuf::from(std::env::var_os("HOME")?);
    let roots = [home.join(".local/share/icons"), home.join(".icons")];
    for root in &roots {
        for (size, data) in [
            (
                64usize,
                include_bytes!("../assets/nuvia-tray-64.png").as_slice(),
            ),
            (
                256usize,
                include_bytes!("../assets/nuvia-tray-256.png").as_slice(),
            ),
        ] {
            let dir = root
                .join("hicolor")
                .join(format!("{size}x{size}"))
                .join("apps");
            if std::fs::create_dir_all(&dir).is_ok() {
                let _ = std::fs::write(dir.join("nuvia.png"), data);
            }
        }

        // Cinnamon/xapp caches icon lookups. Refresh the cache after adding the
        // file, otherwise a newly installed nuvia can keep a blank tray icon
        // until the panel is restarted.
        let _ = std::process::Command::new("gtk-update-icon-cache")
            .args(["-f", "-t"])
            .arg(root.join("hicolor"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    Some(roots[0].to_string_lossy().to_string())
}

#[cfg(target_os = "linux")]
fn tray_icon_pixmap() -> Vec<ksni::Icon> {
    let icon = crate::icon();
    let source = icon.rgba;
    let source_width = icon.width as usize;
    let source_height = icon.height as usize;

    let mut sizes = vec![(source_width, source_height)];
    for size in [64usize, 48, 32] {
        if size < source_width {
            sizes.push((size, size));
        }
    }

    sizes
        .into_iter()
        .map(|(width, height)| {
            let mut data = Vec::with_capacity(width * height * 4);
            for y in 0..height {
                let sy = y * source_height / height;
                for x in 0..width {
                    let sx = x * source_width / width;
                    let index = (sy * source_width + sx) * 4;
                    // ksni expects ARGB32 in network byte order.
                    data.extend_from_slice(&[
                        source[index + 3],
                        source[index],
                        source[index + 1],
                        source[index + 2],
                    ]);
                }
            }
            ksni::Icon {
                width: width as i32,
                height: height as i32,
                data,
            }
        })
        .collect()
}

#[cfg(target_os = "linux")]
pub fn helper_args() -> Option<(String, Lang)> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--tray-helper") {
        let socket = args.next()?;
        let lang = args
            .next()
            .and_then(|value| Lang::from_code(&value))
            .or_else(Lang::from_env)
            .unwrap_or_else(Lang::from_locale);
        Some((socket, lang))
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
pub fn run_helper(socket: &str, lang: Lang) -> i32 {
    use ksni::blocking::TrayMethods;
    use std::os::unix::net::UnixStream;
    use std::sync::Mutex;

    let Ok(stream) = UnixStream::connect(socket) else {
        return 1;
    };
    let Ok(writer) = stream.try_clone() else {
        return 1;
    };
    let writer = Arc::new(Mutex::new(writer));
    let mut watch = stream;

    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 1];
        loop {
            match watch.read(&mut buf) {
                Ok(0) | Err(_) => std::process::exit(0),
                Ok(_) => {}
            }
        }
    });

    ensure_session_bus_for_current_uid();
    let tray = HelperTray {
        writer: writer.clone(),
        icon_theme: ensure_icon_files(),
        lang,
    };
    let _handle = match tray.spawn() {
        Ok(handle) => handle,
        Err(_) => return 1,
    };

    // Keep the helper alive. The watcher thread exits when the root process
    // closes the socket.
    loop {
        std::thread::park();
    }
}

#[cfg(target_os = "linux")]
struct HelperTray {
    writer: Arc<std::sync::Mutex<std::os::unix::net::UnixStream>>,
    icon_theme: Option<String>,
    lang: Lang,
}

#[cfg(target_os = "linux")]
fn helper_send(writer: &Arc<std::sync::Mutex<std::os::unix::net::UnixStream>>, line: &str) {
    use std::io::Write;
    if let Ok(mut stream) = writer.lock() {
        let _ = writeln!(stream, "{line}");
        let _ = stream.flush();
    }
}

#[cfg(target_os = "linux")]
impl ksni::Tray for HelperTray {
    fn id(&self) -> String {
        "nuvia".into()
    }

    fn title(&self) -> String {
        self.lang.strings().tray_tooltip.into()
    }

    fn icon_name(&self) -> String {
        "nuvia".into()
    }

    fn icon_theme_path(&self) -> String {
        self.icon_theme.clone().unwrap_or_default()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        tray_icon_pixmap()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        helper_send(&self.writer, "show");
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem};
        let t = self.lang.strings();
        vec![
            StandardItem {
                label: t.tray_open.into(),
                icon_name: "window-new".into(),
                activate: Box::new(|tray: &mut Self| helper_send(&tray.writer, "show")),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: t.tray_quit.into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|tray: &mut Self| {
                    helper_send(&tray.writer, "quit");
                    std::process::exit(0);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

#[cfg(windows)]
struct WindowsTray {
    _icon: tray_icon::TrayIcon,
}

#[cfg(windows)]
impl WindowsTray {
    fn new(ctx: egui::Context, quitting: Arc<AtomicBool>, lang: Lang) -> anyhow::Result<Self> {
        use tray_icon::menu::{Menu, MenuEvent, MenuItem};
        use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};

        let t = lang.strings();
        let menu = Menu::new();
        let open = MenuItem::new(t.tray_open, true, None);
        let quit_item = MenuItem::new(t.tray_quit, true, None);
        menu.append_items(&[&open, &quit_item])?;
        let open_id = open.id().clone();
        let quit_id = quit_item.id().clone();

        let data = crate::icon();
        let icon = Icon::from_rgba(data.rgba, data.width, data.height)?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(t.tray_tooltip)
            .with_icon(icon)
            .with_menu_on_left_click(false)
            .build()?;

        let show_ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event| {
            if let TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                button_state: tray_icon::MouseButtonState::Up,
                ..
            } = event
            {
                show(&show_ctx);
            }
        }));

        MenuEvent::set_event_handler(Some(move |event: tray_icon::menu::MenuEvent| {
            if event.id == open_id {
                show(&ctx);
            } else if event.id == quit_id {
                quit(&ctx, &quitting);
            }
        }));

        Ok(Self { _icon: tray })
    }
}
