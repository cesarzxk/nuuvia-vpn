use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, Context, FontFamily, FontId, Frame, Grid, Layout, Margin, RichText,
    Rounding, ScrollArea, Stroke, TextEdit, Ui, Vec2, ViewportCommand,
};
use nuvia_core::firewall::{FirewallMode, FirewallProfile, FirewallRuleProtocol, FirewallRuleView};
use nuvia_core::tunnel::DiscoveryRoutes;
use nuvia_core::Identity;
use nuvia_protocol::PublicRoom;
use serde::{Deserialize, Serialize};

use crate::apps::{self, ListeningApp};
use crate::backend::{self, Backend, Event, PeerView, Phase, RoomConfig};
use crate::i18n::{render, Lang, Strings};
use crate::theme;
use crate::tray::Tray;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddMode {
    Create,
    Join,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoomState {
    Starting,
    Searching,
    Connecting,
    Connected { relay: bool },
    Stopped,
    Error,
}

impl RoomState {
    fn pill(self, lang: Lang) -> (&'static str, Color32) {
        let t = lang.strings();
        match self {
            RoomState::Starting => (t.state_starting, theme::AMBER),
            RoomState::Searching => (t.state_searching, theme::AMBER),
            RoomState::Connecting => (t.state_connecting, theme::ACCENT),
            RoomState::Connected { relay: true } => (t.state_relay, theme::PURPLE),
            RoomState::Connected { relay: false } => (t.state_direct, theme::ACCENT),
            RoomState::Stopped => (t.state_off, theme::MUTED),
            RoomState::Error => (t.state_error, theme::RED),
        }
    }

    fn busy(self) -> bool {
        matches!(
            self,
            RoomState::Starting | RoomState::Searching | RoomState::Connecting
        )
    }

    fn connected(self) -> bool {
        matches!(self, RoomState::Connected { .. })
    }
}

struct RoomEntry {
    name: String,
    password: String,
    public: bool,
    tap_name: String,
    bind_port: u16,
    state: RoomState,
    own_ip: Option<String>,
    participants: Vec<PeerView>,
    peer_paths: HashMap<[u8; nuvia_protocol::PEER_TAG_LEN], (bool, Instant)>,
    peer_pings: HashMap<[u8; nuvia_protocol::PEER_TAG_LEN], (u32, Instant)>,
    log: VecDeque<String>,
    firewall: Option<FirewallProfile>,
}

struct PendingAdd {
    room: String,
    password: String,
    public: bool,
    tap_name: String,
    bind_port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PortChoice {
    Both,
    Tcp,
    Udp,
}

impl PortChoice {
    fn protocols(self) -> &'static [FirewallRuleProtocol] {
        const BOTH: [FirewallRuleProtocol; 2] =
            [FirewallRuleProtocol::Tcp, FirewallRuleProtocol::Udp];
        const TCP: [FirewallRuleProtocol; 1] = [FirewallRuleProtocol::Tcp];
        const UDP: [FirewallRuleProtocol; 1] = [FirewallRuleProtocol::Udp];
        match self {
            Self::Both => &BOTH,
            Self::Tcp => &TCP,
            Self::Udp => &UDP,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Both => Self::Tcp,
            Self::Tcp => Self::Udp,
            Self::Udp => Self::Both,
        }
    }

    fn covers_tcp(self) -> bool {
        !matches!(self, Self::Udp)
    }

    fn covers_udp(self) -> bool {
        !matches!(self, Self::Tcp)
    }

    fn label(self, t: &Strings) -> &'static str {
        match self {
            Self::Both => t.firewall_both,
            Self::Tcp => "TCP",
            Self::Udp => "UDP",
        }
    }

    fn color(self) -> Color32 {
        match self {
            Self::Both => theme::ACCENT,
            Self::Tcp => theme::ACCENT_SOFT,
            Self::Udp => theme::PURPLE,
        }
    }
}

#[derive(Debug, Clone)]
struct PortRow {
    start: String,
    end: String,
    protocol: PortChoice,
}

impl Default for PortRow {
    fn default() -> Self {
        Self {
            start: String::new(),
            end: String::new(),
            protocol: PortChoice::Both,
        }
    }
}

#[derive(Default)]
struct AppPicker {
    open: bool,
    entries: Vec<ListeningApp>,
}

struct FirewallEdit {
    room: String,
    inherit: bool,
    mode: FirewallMode,
    ports: Vec<PortRow>,
    icmp: bool,
    apps: AppPicker,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredRooms {
    #[serde(default)]
    rooms: Vec<StoredRoom>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    firewall: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredRoom {
    name: String,
    password: String,
    #[serde(default, skip_serializing_if = "is_false")]
    public: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    firewall: Option<String>,
}

enum Action {
    ToggleExpand(usize),
    Disconnect(usize),
    Leave(usize),
    OpenFirewall(usize),
    CopiedIp,
}

pub struct NuviaApp {
    backend: Backend,
    rooms: Vec<RoomEntry>,
    selected: Option<usize>,
    expanded_room: Option<String>,

    new_name: String,
    new_password: String,
    new_public: bool,
    show_password: bool,
    show_add: bool,
    add_mode: AddMode,
    focus_add: bool,
    focus_password: bool,
    public_rooms: Vec<PublicRoom>,
    public_rooms_error: Option<String>,
    public_rooms_loading: bool,
    public_rooms_refresh_at: Option<Instant>,
    show_public_rooms: bool,
    public_rooms_query: String,
    focus_public_search: bool,

    identity_path: PathBuf,
    identity_public: Option<String>,
    identity_name: String,
    rooms_path: PathBuf,
    language: Lang,
    elevated: bool,
    show_settings: bool,
    show_about: bool,
    show_firewall: bool,

    discovery: Option<DiscoveryRoutes>,
    active_room: Option<String>,
    pending_add: Option<PendingAdd>,
    firewall_default: FirewallProfile,
    firewall_global_mode: FirewallMode,
    firewall_global_ports: Vec<PortRow>,
    firewall_global_icmp: bool,
    firewall_global_apps: AppPicker,
    firewall_edit: Option<FirewallEdit>,

    tray: Tray,
    quitting: Arc<AtomicBool>,

    error: Option<String>,
    error_room: Option<String>,
    toast: Option<(String, Instant)>,
}

impl NuviaApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let lang_override = Lang::from_env();
        let mut language = lang_override.unwrap_or_else(Lang::from_locale);
        let identity_path = backend::user_identity_path();
        let identity = Identity::load(&identity_path).ok();
        let identity_public = identity.as_ref().map(|identity| identity.public_key_hex());
        let identity_name = identity
            .as_ref()
            .and_then(|identity| identity.name().map(str::to_string))
            .or_else(|| std::env::var("USER").ok())
            .or_else(|| std::env::var("USERNAME").ok())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| language.strings().user_fallback.into());

        let rooms_path = identity_path.with_file_name("rooms.toml");
        let mut backend = Backend::new().expect("tokio runtime");
        let mut rooms = Vec::new();
        let mut firewall_default = FirewallProfile::default();
        if let Ok(text) = std::fs::read_to_string(&rooms_path) {
            match toml::from_str::<StoredRooms>(&text) {
                Ok(stored) => {
                    if lang_override.is_none() {
                        if let Some(code) = stored.language.as_deref() {
                            language = Lang::from_code(code).unwrap_or(language);
                        }
                    }
                    if let Some(value) = stored.firewall.as_deref() {
                        match value.parse::<FirewallProfile>() {
                            Ok(profile) => firewall_default = profile,
                            Err(error) => {
                                tracing::warn!(%error, value, "invalid global firewall profile")
                            }
                        }
                    }
                    for saved in stored.rooms {
                        let firewall = saved.firewall.as_deref().and_then(|value| {
                            match value.parse::<FirewallProfile>() {
                                Ok(profile) => Some(profile),
                                Err(error) => {
                                    tracing::warn!(%error, value, "invalid room firewall profile");
                                    None
                                }
                            }
                        });
                        let name = saved.name.trim().to_string();
                        if name.is_empty() || rooms.iter().any(|room: &RoomEntry| room.name == name)
                        {
                            continue;
                        }
                        let (tap_name, bind_port) = backend.allocate_slot();
                        rooms.push(RoomEntry {
                            name,
                            password: saved.password,
                            public: saved.public,
                            tap_name,
                            bind_port,
                            state: RoomState::Stopped,
                            own_ip: None,
                            participants: Vec::new(),
                            peer_paths: HashMap::new(),
                            peer_pings: HashMap::new(),
                            log: VecDeque::new(),
                            firewall,
                        });
                    }
                }
                Err(error) => {
                    tracing::warn!(path = %rooms_path.display(), %error, "could not parse rooms file");
                }
            }
        }

        let selected = if rooms.is_empty() { None } else { Some(0) };
        let quitting = Arc::new(AtomicBool::new(false));
        let tray = Tray::spawn(cc.egui_ctx.clone(), quitting.clone(), language);
        let (firewall_global_ports, firewall_global_icmp) =
            port_rows_from_profile(&firewall_default);

        Self {
            backend,
            rooms,
            selected,
            expanded_room: None,
            new_name: String::new(),
            new_password: String::new(),
            new_public: false,
            show_password: false,
            show_add: false,
            add_mode: AddMode::Create,
            focus_add: false,
            focus_password: false,
            public_rooms: Vec::new(),
            public_rooms_error: None,
            public_rooms_loading: false,
            public_rooms_refresh_at: None,
            show_public_rooms: false,
            public_rooms_query: String::new(),
            focus_public_search: false,
            identity_path,
            identity_public,
            identity_name,
            rooms_path,
            language,
            elevated: backend::is_elevated() || nuvia_core::helper::available(),
            show_settings: false,
            show_about: false,
            show_firewall: false,
            discovery: None,
            active_room: None,
            pending_add: None,
            firewall_global_mode: firewall_default.mode,
            firewall_global_ports,
            firewall_global_icmp,
            firewall_global_apps: AppPicker::default(),
            firewall_default,
            firewall_edit: None,
            tray,
            quitting,
            error: None,
            error_room: None,
            toast: None,
        }
    }

    fn save_rooms(&self) {
        let stored = StoredRooms {
            rooms: self
                .rooms
                .iter()
                .map(|room| StoredRoom {
                    name: room.name.clone(),
                    password: room.password.clone(),
                    public: room.public,
                    firewall: room.firewall.as_ref().map(ToString::to_string),
                })
                .collect(),
            language: Some(self.language.code().to_string()),
            firewall: Some(self.firewall_default.to_string()),
        };
        let Ok(text) = toml::to_string_pretty(&stored) else {
            return;
        };
        if let Some(parent) = self.rooms_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match write_private(&self.rooms_path, &text) {
            Ok(()) => {
                tracing::debug!(path = %self.rooms_path.display(), "rooms saved");
            }
            Err(error) => {
                tracing::warn!(path = %self.rooms_path.display(), %error, "could not save rooms");
            }
        }
    }

    fn toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now()));
    }

    fn log_for(&mut self, room: &str, message: impl Into<String>) {
        if let Some(entry) = self.rooms.iter_mut().find(|entry| entry.name == room) {
            entry.log.push_back(message.into());
            while entry.log.len() > 40 {
                entry.log.pop_front();
            }
        }
    }

    fn open_add(&mut self, mode: AddMode) {
        if let Some(pending) = self.pending_add.take() {
            self.backend.leave(&pending.room);
        }
        self.error = None;
        self.error_room = None;
        self.new_name.clear();
        self.new_password.clear();
        self.new_public = false;
        self.show_password = false;
        self.focus_password = false;
        self.public_rooms.clear();
        self.public_rooms_error = None;
        self.public_rooms_loading = false;
        self.public_rooms_refresh_at = (mode == AddMode::Join).then(std::time::Instant::now);
        self.show_public_rooms = false;
        self.public_rooms_query.clear();
        self.focus_public_search = false;
        self.add_mode = mode;
        self.show_add = true;
        self.focus_add = true;
    }

    fn refresh_public_rooms(&mut self) {
        if self.public_rooms_loading {
            return;
        }
        self.public_rooms_loading = true;
        self.public_rooms_error = None;
        self.public_rooms_refresh_at = Some(Instant::now() + Duration::from_secs(10));
        self.backend.refresh_public_rooms();
    }

    /// Starts a room join. The room is only added to the list (and saved)
    /// after the rendezvous server accepts the derived token, so a wrong
    /// password cannot look like a successful add.
    fn join(&mut self) {
        self.error = None;
        self.error_room = None;
        let t = self.language.strings();
        let name = self.new_name.trim().to_string();
        let password = self.new_password.clone();

        if self.pending_add.is_some() {
            return;
        }
        self.show_public_rooms = false;
        if name.is_empty() {
            self.error = Some(t.err_enter_room_name.into());
            return;
        }
        if self.rooms.iter().any(|room| room.name == name) {
            self.error = Some(t.err_room_exists.into());
            return;
        }

        let (tap_name, bind_port) = self.backend.allocate_slot();
        let config = RoomConfig {
            name: name.clone(),
            password: password.clone(),
            tap_name: tap_name.clone(),
            bind_port,
            identity_path: self.identity_path.clone(),
            lang: self.language,
            create: matches!(self.add_mode, AddMode::Create),
            public: self.new_public,
            firewall: self.firewall_default.clone(),
        };

        match self.backend.join(config) {
            Ok(()) => {
                self.pending_add = Some(PendingAdd {
                    room: name,
                    password,
                    public: self.new_public,
                    tap_name,
                    bind_port,
                });
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
            }
        }
    }

    fn leave(&mut self, index: usize) {
        if index >= self.rooms.len() {
            return;
        }
        let room = self.rooms.remove(index);
        let t = self.language.strings();
        self.backend.leave(&room.name);
        if self.expanded_room.as_deref() == Some(room.name.as_str()) {
            self.expanded_room = None;
        }
        let was_active = self.active_room.as_deref() == Some(room.name.as_str());
        if was_active {
            self.active_room = None;
        }
        self.selected = if self.rooms.is_empty() {
            None
        } else {
            Some(index.min(self.rooms.len() - 1))
        };
        self.toast(render(t.toast_room_removed, &[("name", &room.name)]));
        self.save_rooms();
        if was_active {
            self.activate_next_connected();
        }
    }

    fn disconnect(&mut self, index: usize) {
        let Some(entry) = self.rooms.get(index) else {
            return;
        };
        let name = entry.name.clone();
        let t = self.language.strings();
        self.backend.leave(&name);
        let was_active = self.active_room.as_deref() == Some(name.as_str());
        if was_active {
            self.active_room = None;
        }
        if let Some(entry) = self.rooms.get_mut(index) {
            entry.state = RoomState::Stopped;
            entry.own_ip = None;
            entry.participants.clear();
            entry.peer_paths.clear();
            entry.peer_pings.clear();
            entry.log.push_back(t.log_disconnected.into());
            while entry.log.len() > 40 {
                entry.log.pop_front();
            }
        }
        self.toast(render(t.toast_room_disconnected, &[("name", &name)]));
        if was_active {
            self.activate_next_connected();
        }
    }

    fn activate_next_connected(&mut self) {
        if self.active_room.is_some() {
            return;
        }
        if let Some(index) = self.rooms.iter().position(|room| room.state.connected()) {
            self.activate(index);
        }
    }

    fn reconnect(&mut self, index: usize) {
        if index >= self.rooms.len() {
            return;
        }
        self.error = None;
        self.error_room = None;
        let (name, password, public, tap_name, bind_port) = {
            let room = &self.rooms[index];
            (
                room.name.clone(),
                room.password.clone(),
                room.public,
                room.tap_name.clone(),
                room.bind_port,
            )
        };
        let t = self.language.strings();
        self.backend.leave(&name);
        if self.active_room.as_deref() == Some(name.as_str()) {
            self.active_room = None;
        }
        let config = RoomConfig {
            name: name.clone(),
            password,
            tap_name,
            bind_port,
            identity_path: self.identity_path.clone(),
            lang: self.language,
            create: true,
            public,
            firewall: self.effective_firewall(index),
        };
        match self.backend.join(config) {
            Ok(()) => {
                let room = &mut self.rooms[index];
                room.state = RoomState::Starting;
                room.own_ip = None;
                room.participants.clear();
                room.peer_paths.clear();
                room.peer_pings.clear();
                room.log.clear();
                room.log.push_back(t.log_reconnecting.into());
                self.toast(t.toast_reconnecting);
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
            }
        }
    }

    fn effective_firewall(&self, index: usize) -> FirewallProfile {
        self.rooms
            .get(index)
            .and_then(|room| room.firewall.clone())
            .unwrap_or_else(|| self.firewall_default.clone())
    }

    fn apply_global_firewall(&self) {
        for room in &self.rooms {
            let effective = room
                .firewall
                .clone()
                .unwrap_or_else(|| self.firewall_default.clone());
            self.backend.set_firewall(&room.name, effective);
        }
    }

    fn open_firewall(&mut self, index: usize) {
        let Some(room) = self.rooms.get(index) else {
            return;
        };
        let profile = room
            .firewall
            .clone()
            .unwrap_or_else(|| self.firewall_default.clone());
        let (ports, icmp) = port_rows_from_profile(&profile);
        self.firewall_edit = Some(FirewallEdit {
            room: room.name.clone(),
            inherit: room.firewall.is_none(),
            mode: profile.mode,
            ports,
            icmp,
            apps: AppPicker::default(),
        });
    }

    fn activate(&mut self, index: usize) {
        if index >= self.rooms.len() {
            return;
        }
        let t = self.language.strings();
        let room = &self.rooms[index];
        if !room.state.connected() {
            self.toast(t.toast_connect_before_activate);
            return;
        }
        if !self.elevated {
            self.error = Some(t.err_need_admin_routes.into());
            return;
        }

        let tap = room.tap_name.clone();
        let name = room.name.clone();
        self.active_room = Some(name.clone());
        // The TAP is shared by every room; install the discovery routes only
        // once per app session, not every time a room is toggled. When the
        // privileged helper owns the TAP it also owns the routes.
        if self.discovery.is_none() && !nuvia_core::helper::active() {
            self.discovery = Some(DiscoveryRoutes::install(&tap));
            self.log_for(&name, t.log_routes_installed);
            self.toast(t.toast_discovery_active);
        }
    }

    fn poll_backend(&mut self, ctx: &Context) {
        let t = self.language.strings();
        let events = self.backend.poll();
        for event in events {
            match event {
                Event::Phase { room, phase } => {
                    let state = match phase {
                        Phase::Starting => RoomState::Starting,
                        Phase::Searching => RoomState::Searching,
                        Phase::Connecting => RoomState::Connecting,
                    };
                    let text = match phase {
                        Phase::Starting => t.phase_starting,
                        Phase::Searching => t.phase_searching,
                        Phase::Connecting => t.phase_connecting,
                    };
                    if let Some(entry) = self.rooms.iter_mut().find(|entry| entry.name == room) {
                        entry.state = state;
                        entry.log.push_back(text.into());
                        while entry.log.len() > 40 {
                            entry.log.pop_front();
                        }
                    }
                }
                Event::Participants {
                    room,
                    own_ip,
                    peers,
                } => {
                    if self
                        .pending_add
                        .as_ref()
                        .is_some_and(|pending| pending.room == room)
                    {
                        if let Some(pending) = self.pending_add.take() {
                            let mut log = VecDeque::new();
                            log.push_back(t.log_joining.to_string());
                            let mut entry = RoomEntry {
                                name: pending.room,
                                password: pending.password,
                                public: pending.public,
                                tap_name: pending.tap_name,
                                bind_port: pending.bind_port,
                                state: RoomState::Connecting,
                                own_ip: Some(own_ip),
                                participants: Vec::new(),
                                peer_paths: HashMap::new(),
                                peer_pings: HashMap::new(),
                                log,
                                firewall: None,
                            };
                            mark_participants_online(&mut entry, &peers);
                            entry.participants = peers;
                            self.rooms.insert(0, entry);
                            self.selected = Some(0);
                            self.new_name.clear();
                            self.new_password.clear();
                            self.show_add = false;
                            self.error = None;
                            self.error_room = None;
                            self.save_rooms();
                            self.toast(t.toast_room_added);
                        }
                        continue;
                    }
                    if let Some(entry) = self.rooms.iter_mut().find(|entry| entry.name == room) {
                        entry.own_ip = Some(own_ip);
                        entry
                            .peer_pings
                            .retain(|tag, _| peers.iter().any(|peer| peer.tag == *tag));
                        mark_participants_online(entry, &peers);
                        entry.participants = peers;
                        refresh_room_summary(entry);
                    }
                }
                Event::PeerStatus { room, tag, direct } => {
                    if let Some(entry) = self.rooms.iter_mut().find(|entry| entry.name == room) {
                        entry.peer_paths.insert(tag, (direct, Instant::now()));
                        refresh_room_summary(entry);
                    }
                }
                Event::PeerPing { room, tag, ms } => {
                    if let Some(entry) = self.rooms.iter_mut().find(|entry| entry.name == room) {
                        entry.peer_pings.insert(tag, (ms, Instant::now()));
                    }
                }
                Event::Connected { room, relay } => {
                    if self.error_room.as_deref() == Some(room.as_str()) {
                        self.error = None;
                        self.error_room = None;
                    }
                    let mut activate = None;
                    let no_active = self.active_room.is_none();
                    if let Some((index, entry)) = self
                        .rooms
                        .iter_mut()
                        .enumerate()
                        .find(|(_, entry)| entry.name == room)
                    {
                        entry.state = RoomState::Connected { relay };
                        entry.log.push_back(if relay {
                            t.log_connected_relay.into()
                        } else {
                            t.log_connected_direct.into()
                        });
                        refresh_room_summary(entry);
                        if no_active {
                            activate = Some(index);
                        }
                    }
                    if let Some(index) = activate {
                        self.activate(index);
                    }
                }
                Event::Error { room, message } => {
                    if self
                        .pending_add
                        .as_ref()
                        .is_some_and(|pending| pending.room == room)
                    {
                        self.pending_add = None;
                        let detail = if message.contains("room not found")
                            || message.contains("404")
                        {
                            t.err_room_not_found
                        } else if message.contains("invalid room token") || message.contains("403")
                        {
                            t.err_wrong_password
                        } else {
                            message.as_str()
                        };
                        self.error = Some(format!("{room}: {detail}"));
                        self.error_room = Some(room);
                        continue;
                    }
                    let room_name = room.clone();
                    if self.active_room.as_deref() == Some(room.as_str()) {
                        self.active_room = None;
                        self.activate_next_connected();
                    }
                    if let Some(entry) = self.rooms.iter_mut().find(|entry| entry.name == room) {
                        entry.state = RoomState::Error;
                        entry
                            .log
                            .push_back(format!("{}: {message}", t.log_error_prefix));
                        self.error = Some(format!("{}: {message}", entry.name));
                    }
                    self.error_room = Some(room_name);
                }
                Event::Stopped { room } => {
                    if self
                        .pending_add
                        .as_ref()
                        .is_some_and(|pending| pending.room == room)
                    {
                        self.pending_add = None;
                        self.error = Some(format!("{room}: {}", t.log_session_ended));
                        self.error_room = Some(room);
                        continue;
                    }
                    if self.active_room.as_deref() == Some(room.as_str()) {
                        self.active_room = None;
                        self.activate_next_connected();
                    }
                    if let Some(entry) = self.rooms.iter_mut().find(|entry| entry.name == room) {
                        if entry.state != RoomState::Error {
                            entry.state = RoomState::Stopped;
                            entry.log.push_back(t.log_session_ended.into());
                        }
                    }
                }
                Event::PublicRooms { rooms, error } => {
                    self.public_rooms = rooms;
                    self.public_rooms_error = error;
                    self.public_rooms_loading = false;
                    self.public_rooms_refresh_at = Some(Instant::now() + Duration::from_secs(10));
                }
            }
        }

        if self.rooms.iter().any(|room| room.state.busy()) {
            ctx.request_repaint_after(Duration::from_millis(200));
        } else if self.rooms.iter().any(|room| room.state.connected()) {
            // Keep peer dots fresh: a member that stops sending keepalives
            // must turn gray even without new backend events.
            for entry in &mut self.rooms {
                if entry.state.connected() {
                    refresh_room_summary(entry);
                }
            }
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        if self.pending_add.is_some() || self.public_rooms_loading {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if self.show_add && self.add_mode == AddMode::Join {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    fn bar_target(&self) -> Option<usize> {
        if let Some(index) = self.selected {
            return Some(index);
        }
        if let Some(active) = self.active_room.as_deref() {
            if let Some(index) = self.rooms.iter().position(|room| room.name == active) {
                return Some(index);
            }
        }
        (!self.rooms.is_empty()).then_some(0)
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let t = self.language.strings();
        let target = self.bar_target();
        let (ip, state_label, state_color, connected, busy) = match target {
            Some(index) => {
                let room = &self.rooms[index];
                let (label, color) = room.state.pill(self.language);
                (
                    room.own_ip.clone().unwrap_or_else(|| "—".into()),
                    label,
                    color,
                    room.state.connected(),
                    room.state.busy(),
                )
            }
            None => (
                "—".to_string(),
                t.status_no_network,
                theme::MUTED,
                false,
                false,
            ),
        };

        let mut toggle = false;
        ui.horizontal(|ui| {
            let (rect, response) = ui.allocate_exact_size(Vec2::splat(50.0), egui::Sense::click());
            let (fill, stroke_color, color) = if connected {
                (theme::ACCENT_DARK, theme::ACCENT, theme::ACCENT)
            } else if busy {
                (theme::CARD_2, theme::AMBER, theme::AMBER)
            } else {
                (theme::CARD_2, theme::BORDER, theme::MUTED)
            };
            ui.painter().rect_filled(rect, Rounding::same(13.0), fill);
            ui.painter().rect_stroke(
                rect,
                Rounding::same(13.0),
                Stroke::new(1.0_f32, stroke_color),
            );

            let center = rect.center() + egui::vec2(0.0, -5.0);
            let radius = 9.0;
            let points: Vec<egui::Pos2> = (0..=24)
                .map(|step| {
                    let angle = -std::f32::consts::FRAC_PI_2
                        + 0.45
                        + (step as f32 / 24.0) * (std::f32::consts::TAU - 0.9);
                    center + egui::vec2(angle.cos(), angle.sin()) * radius
                })
                .collect();
            ui.painter()
                .add(egui::Shape::line(points, Stroke::new(2.0_f32, color)));
            ui.painter().line_segment(
                [
                    center + egui::vec2(0.0, -radius - 2.0),
                    center + egui::vec2(0.0, 1.0),
                ],
                Stroke::new(2.0_f32, color),
            );
            ui.painter().text(
                rect.center() + egui::vec2(0.0, 13.0),
                egui::Align2::CENTER_CENTER,
                if connected { "ON" } else { "OFF" },
                theme::semibold(9.0),
                color,
            );
            if response.clicked() {
                toggle = true;
            }
            let _ = response.on_hover_text(if connected {
                t.toggle_disconnect
            } else if busy {
                t.toggle_cancel
            } else {
                t.toggle_connect
            });

            ui.add_space(8.0);
            let (dot_rect, dot_response) =
                ui.allocate_exact_size(Vec2::splat(9.0), egui::Sense::hover());
            ui.painter()
                .circle_filled(dot_rect.center(), 4.0, state_color);
            let _ = dot_response.on_hover_text(state_label);
            ui.label(
                RichText::new(&self.identity_name)
                    .font(theme::semibold(15.0))
                    .color(theme::TEXT),
            );
            ui.add_space(14.0);
            ui.label(
                RichText::new(&ip)
                    .font(FontId::new(13.0, FontFamily::Monospace))
                    .color(theme::TEXT),
            );
        });

        let rect = ui.max_rect();
        ui.painter().line_segment(
            [rect.left_bottom(), rect.right_bottom()],
            Stroke::new(1.0_f32, theme::BORDER),
        );

        if toggle {
            if let Some(index) = target {
                if connected || busy {
                    self.disconnect(index);
                } else {
                    self.reconnect(index);
                }
            }
        }
    }

    fn header(&mut self, ui: &mut Ui) {
        let t = self.language.strings();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("NUUVIA")
                    .font(theme::display(26.0))
                    .color(theme::ACCENT),
            );
            ui.label(
                RichText::new("VPN")
                    .font(theme::semibold(10.0))
                    .color(theme::MUTED),
            );
            ui.add_space(8.0);
            if ui.available_width() > 410.0 {
                let connected = self.rooms.iter().filter(|r| r.state.connected()).count();
                ui.label(
                    RichText::new(render(
                        t.rooms_summary,
                        &[
                            ("rooms", &self.rooms.len().to_string()),
                            ("connected", &connected.to_string()),
                        ],
                    ))
                    .size(11.0)
                    .color(theme::MUTED),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.menu_button(
                    RichText::new(t.menu_help)
                        .font(theme::semibold(12.0))
                        .color(theme::MUTED),
                    |ui| {
                        ui.set_min_width(150.0);
                        if ui.button(t.menu_settings).clicked() {
                            self.show_settings = true;
                            ui.close_menu();
                        }
                        if ui.button(t.menu_about).clicked() {
                            self.show_about = true;
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button(t.menu_quit).clicked() {
                            self.save_rooms();
                            self.quitting.store(true, Ordering::SeqCst);
                            std::process::exit(0);
                        }
                    },
                );
                ui.menu_button(
                    RichText::new(t.menu_network)
                        .font(theme::semibold(12.0))
                        .color(theme::ACCENT),
                    |ui| {
                        ui.set_min_width(168.0);
                        if ui.button(t.menu_create).clicked() {
                            self.open_add(AddMode::Create);
                            ui.close_menu();
                        }
                        if ui.button(t.menu_join).clicked() {
                            self.open_add(AddMode::Join);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button(t.section_firewall).clicked() {
                            self.firewall_global_mode = self.firewall_default.mode;
                            let (ports, icmp) = port_rows_from_profile(&self.firewall_default);
                            self.firewall_global_ports = ports;
                            self.firewall_global_icmp = icmp;
                            self.show_firewall = true;
                            ui.close_menu();
                        }
                    },
                )
                .response
                .on_hover_text(t.tooltip_network_menu);
                if !self.elevated {
                    theme::badge(ui, t.badge_limited, theme::AMBER);
                }
            });
        });
    }

    fn room_card(&self, ui: &mut Ui, index: usize, actions: &mut Vec<Action>) {
        let t = self.language.strings();
        let room = &self.rooms[index];
        let selected = self.selected == Some(index);
        let expanded = self.expanded_room.as_deref() == Some(room.name.as_str());
        let (state_label, state_color) = room.state.pill(self.language);

        let border = if selected {
            theme::ACCENT_SOFT
        } else {
            theme::BORDER
        };

        let row = Frame::none()
            .fill(if selected { theme::CARD_3 } else { theme::CARD })
            .rounding(Rounding::same(10.0))
            .stroke(Stroke::new(1.0_f32, border))
            .inner_margin(Margin::symmetric(10.0, 5.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (dot_rect, _) =
                        ui.allocate_exact_size(Vec2::splat(7.0), egui::Sense::hover());
                    ui.painter()
                        .circle_filled(dot_rect.center(), 3.0, state_color);
                    ui.add(
                        egui::Label::new(
                            RichText::new(&room.name)
                                .font(theme::semibold(14.0))
                                .color(theme::TEXT),
                        )
                        .truncate(),
                    );
                    let (fw_text, fw_color) = firewall_badge(room.firewall.as_ref(), t);
                    theme::badge(ui, &fw_text, fw_color);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        theme::badge(ui, state_label, state_color);
                    });
                });
            });

        let response = ui.interact(
            row.response.rect,
            ui.make_persistent_id(("room-row", room.name.as_str())),
            egui::Sense::click(),
        );
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            ui.painter().rect_stroke(
                row.response.rect,
                Rounding::same(10.0),
                Stroke::new(1.0_f32, theme::ACCENT_SOFT),
            );
        }
        if response.clicked() {
            actions.push(Action::ToggleExpand(index));
        }

        response.context_menu(|ui| {
            ui.set_min_width(132.0);
            let can_disconnect = room.state.connected() || room.state.busy();
            if ui
                .add_enabled(can_disconnect, egui::Button::new(t.context_disable))
                .clicked()
            {
                actions.push(Action::Disconnect(index));
                ui.close_menu();
            }
            if ui.button(t.context_firewall).clicked() {
                actions.push(Action::OpenFirewall(index));
                ui.close_menu();
            }
            ui.separator();
            if ui.button(t.context_leave).clicked() {
                actions.push(Action::Leave(index));
                ui.close_menu();
            }
        });

        if expanded {
            ui.add_space(4.0);
            Frame::none()
                .fill(theme::CARD_2)
                .rounding(Rounding::same(10.0))
                .stroke(Stroke::new(1.0_f32, theme::BORDER))
                .inner_margin(Margin::symmetric(10.0, 8.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new(t.participants)
                            .font(theme::semibold(8.5))
                            .color(theme::MUTED),
                    );
                    let mut copied = false;
                    copied |= participant_row(
                        ui,
                        &self.identity_name,
                        room.own_ip.as_deref(),
                        true,
                        state_color,
                        None,
                        t,
                    );
                    for peer in &room.participants {
                        let color = match peer_path(room, &peer.tag) {
                            Some(true) => theme::ACCENT,
                            Some(false) => theme::PURPLE,
                            None => theme::MUTED,
                        };
                        copied |= participant_row(
                            ui,
                            &peer.name,
                            Some(&peer.ip),
                            false,
                            color,
                            peer_ping(room, &peer.tag),
                            t,
                        );
                    }
                    if copied {
                        actions.push(Action::CopiedIp);
                    }
                    if room.participants.is_empty() && room.state.busy() {
                        ui.label(
                            RichText::new(t.waiting_participants)
                                .size(10.0)
                                .color(theme::MUTED),
                        );
                    }
                    if !room.participants.is_empty() {
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            for (color, label) in [
                                (theme::ACCENT, t.legend_direct),
                                (theme::PURPLE, t.legend_relay),
                                (theme::MUTED, t.legend_offline),
                            ] {
                                ui.label(RichText::new("●").size(8.0).color(color));
                                ui.label(RichText::new(label).size(9.0).color(theme::MUTED));
                                ui.add_space(4.0);
                            }
                        });
                    }
                });
        }

        ui.add_space(6.0);
    }

    fn rooms_list(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let t = self.language.strings();
        if self.rooms.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(t.no_networks)
                        .font(theme::semibold(14.0))
                        .color(theme::MUTED),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(t.no_networks_hint)
                        .size(12.0)
                        .color(theme::MUTED),
                );
            });
            return;
        }

        for index in 0..self.rooms.len() {
            self.room_card(ui, index, actions);
        }
    }

    fn add_window(&mut self, ctx: &Context) {
        if !self.show_add {
            return;
        }

        if self.add_mode == AddMode::Join
            && self
                .public_rooms_refresh_at
                .is_some_and(|at| Instant::now() >= at)
        {
            self.refresh_public_rooms();
        }

        let mut open = true;
        let mut close = false;
        let t = self.language.strings();
        let pending = self.pending_add.is_some();
        let creating = matches!(self.add_mode, AddMode::Create);
        let (title, kicker, hint, action) = match self.add_mode {
            AddMode::Create => (
                t.create_title,
                t.create_kicker,
                t.create_hint,
                t.create_action,
            ),
            AddMode::Join => (t.join_title, t.join_kicker, t.join_hint, t.join_action),
        };
        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .vscroll(true)
            .max_height(470.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -24.0])
            .frame(theme::modal_frame_scroll())
            .show(ctx, |ui| {
                ui.set_width(theme::MODAL_WIDTH);
                ui.label(
                    RichText::new(kicker)
                        .font(theme::semibold(10.0))
                        .color(theme::ACCENT),
                );
                ui.label(RichText::new(hint).size(11.0).color(theme::MUTED));
                ui.add_space(10.0);

                if !creating {
                    ui.label(
                        RichText::new(t.public_rooms)
                            .font(theme::semibold(10.0))
                            .color(theme::ACCENT),
                    )
                    .on_hover_text(t.public_rooms_hint);
                    ui.add_space(2.0);
                    let search = ui.add_sized(
                        [320.0, 34.0],
                        egui::Button::new(
                            RichText::new(t.public_search_action)
                                .size(11.0)
                                .color(theme::TEXT),
                        )
                        .fill(theme::CARD_2)
                        .stroke(Stroke::new(1.0_f32, theme::BORDER))
                        .rounding(Rounding::same(8.0)),
                    );
                    if search.clicked() {
                        self.show_public_rooms = true;
                        self.public_rooms_query.clear();
                        self.focus_public_search = true;
                        self.public_rooms_error = None;
                        self.public_rooms_refresh_at = Some(Instant::now());
                        self.refresh_public_rooms();
                    }
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(t.join_by_name)
                            .font(theme::semibold(10.0))
                            .color(theme::ACCENT),
                    );
                    ui.add_space(4.0);
                }

                ui.label(
                    RichText::new(t.field_room_name)
                        .font(theme::semibold(9.0))
                        .color(theme::MUTED),
                );
                let name = ui.add_sized(
                    [320.0, 36.0],
                    TextEdit::singleline(&mut self.new_name)
                        .hint_text(t.hint_room_name)
                        .char_limit(64)
                        .text_color(theme::TEXT)
                        .background_color(theme::CARD_2)
                        .interactive(!pending)
                        .vertical_align(Align::Center),
                );
                if self.focus_add {
                    name.request_focus();
                    self.focus_add = false;
                }

                ui.add_space(2.0);
                ui.label(
                    RichText::new(if creating {
                        t.field_password_optional
                    } else {
                        t.field_password
                    })
                    .font(theme::semibold(9.0))
                    .color(theme::MUTED),
                );
                let password = ui.add_sized(
                    [320.0, 36.0],
                    TextEdit::singleline(&mut self.new_password)
                        .password(!self.show_password)
                        .hint_text(t.hint_password_optional)
                        .text_color(theme::TEXT)
                        .background_color(theme::CARD_2)
                        .interactive(!pending)
                        .vertical_align(Align::Center),
                );
                if self.focus_password {
                    password.request_focus();
                    self.focus_password = false;
                }
                ui.checkbox(
                    &mut self.show_password,
                    RichText::new(t.show_password)
                        .size(11.0)
                        .color(theme::MUTED),
                );

                if creating {
                    ui.add_space(2.0);
                    ui.checkbox(
                        &mut self.new_public,
                        RichText::new(t.create_public).size(11.0).color(theme::TEXT),
                    );
                    ui.label(
                        RichText::new(t.create_public_hint)
                            .size(9.5)
                            .color(theme::MUTED),
                    );
                    if self.new_password.is_empty() {
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(t.warn_no_password)
                                .size(9.5)
                                .color(theme::AMBER),
                        );
                    }
                }

                let enabled = !pending && !self.new_name.trim().is_empty();
                let enter = !pending
                    && enabled
                    && (name.lost_focus() || password.lost_focus())
                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.add_space(4.0);
                let submitted =
                    theme::primary_button(ui, action, enabled).clicked() || (enabled && enter);
                if submitted {
                    self.join();
                }
                if pending {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new(t.state_connecting)
                                .font(theme::semibold(11.0))
                                .color(theme::ACCENT),
                        );
                        if ui.button(t.close).clicked() {
                            close = true;
                        }
                    });
                }

                if let Some(error) = &self.error {
                    ui.add_space(6.0);
                    Frame::none()
                        .fill(Color32::from_rgb(42, 12, 22))
                        .rounding(Rounding::same(8.0))
                        .inner_margin(Margin::same(8.0))
                        .show(ui, |ui| {
                            ui.label(RichText::new(error).color(theme::RED).size(11.0));
                        });
                }
            });
        self.show_add = open && !close;
        if !self.show_add {
            self.show_public_rooms = false;
            if let Some(pending) = self.pending_add.take() {
                self.backend.leave(&pending.room);
            }
            self.error = None;
            self.error_room = None;
        }
    }

    fn public_rooms_window(&mut self, ctx: &Context) {
        if !self.show_public_rooms {
            return;
        }
        if !self.show_add {
            self.show_public_rooms = false;
            return;
        }

        let t = self.language.strings();
        let mut open = self.show_public_rooms;
        let mut selected: Option<(String, bool)> = None;
        egui::Window::new(t.public_search_title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(340.0)
            .vscroll(true)
            .max_height(470.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -12.0])
            .frame(
                Frame::none()
                    .fill(theme::CARD)
                    .rounding(Rounding::same(14.0))
                    .stroke(Stroke::new(1.0_f32, theme::BORDER))
                    .inner_margin(Margin {
                        left: 18.0,
                        right: 4.0,
                        top: 14.0,
                        bottom: 14.0,
                    }),
            )
            .show(ctx, |ui| {
                let content_width = 326.0;
                ui.set_width(content_width);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(t.public_search_hint)
                            .size(10.0)
                            .color(theme::MUTED),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                !self.public_rooms_loading,
                                egui::Button::new(
                                    RichText::new(t.public_refresh)
                                        .font(theme::semibold(8.5))
                                        .color(theme::TEXT),
                                )
                                .min_size(Vec2::new(0.0, 22.0)),
                            )
                            .clicked()
                        {
                            self.refresh_public_rooms();
                        }
                    });
                });
                ui.add_space(4.0);
                let search = ui.add_sized(
                    [content_width, 34.0],
                    TextEdit::singleline(&mut self.public_rooms_query)
                        .hint_text(t.public_search_placeholder)
                        .text_color(theme::TEXT)
                        .background_color(theme::CARD_2)
                        .vertical_align(Align::Center),
                );
                if self.focus_public_search {
                    search.request_focus();
                    self.focus_public_search = false;
                }
                ui.add_space(6.0);

                if self.public_rooms_loading && self.public_rooms.is_empty() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new(t.public_searching)
                                .size(10.0)
                                .color(theme::MUTED),
                        );
                    });
                } else if let Some(error) = &self.public_rooms_error {
                    ui.label(RichText::new(t.public_error).size(10.0).color(theme::RED))
                        .on_hover_text(error);
                } else if self.public_rooms.is_empty() {
                    ui.label(RichText::new(t.public_empty).size(10.0).color(theme::MUTED));
                } else {
                    let query = self.public_rooms_query.trim().to_lowercase();
                    let filtered: Vec<&PublicRoom> = self
                        .public_rooms
                        .iter()
                        .filter(|room| {
                            query.is_empty() || room.name.to_lowercase().contains(&query)
                        })
                        .collect();
                    if filtered.is_empty() {
                        ui.label(
                            RichText::new(t.public_search_empty)
                                .size(10.0)
                                .color(theme::MUTED),
                        );
                    } else {
                        ScrollArea::vertical()
                            .max_height(260.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                let row_width = ui.available_width();
                                for room in filtered {
                                    let full = room.peers >= room.max_peers;
                                    let button_width = 58.0;
                                    let spacing = ui.spacing().item_spacing.x;
                                    let text_width =
                                        (row_width - button_width - spacing).max(100.0);
                                    ui.horizontal(|ui| {
                                        ui.allocate_ui_with_layout(
                                            Vec2::new(text_width, 34.0),
                                            Layout::top_down(Align::Min),
                                            |ui| {
                                                ui.set_min_width(text_width);
                                                ui.horizontal(|ui| {
                                                    ui.add(
                                                        egui::Label::new(
                                                            RichText::new(&room.name)
                                                                .font(theme::semibold(11.5))
                                                                .color(theme::TEXT),
                                                        )
                                                        .truncate(),
                                                    );
                                                    let (badge, color) = if room.has_password {
                                                        (t.public_locked, theme::AMBER)
                                                    } else {
                                                        (t.public_open, theme::ACCENT)
                                                    };
                                                    theme::badge(ui, badge, color);
                                                });
                                                ui.label(
                                                    RichText::new(format!(
                                                        "{}/{}",
                                                        room.peers, room.max_peers
                                                    ))
                                                    .size(9.0)
                                                    .color(theme::MUTED),
                                                );
                                            },
                                        );
                                        if full {
                                            ui.add_enabled(
                                                false,
                                                egui::Button::new(
                                                    RichText::new(t.public_full)
                                                        .font(theme::semibold(8.5))
                                                        .color(theme::MUTED),
                                                )
                                                .min_size(Vec2::new(button_width, 22.0)),
                                            );
                                        } else if ui
                                            .add_sized(
                                                [button_width, 22.0],
                                                egui::Button::new(
                                                    RichText::new(t.public_select)
                                                        .font(theme::semibold(9.0))
                                                        .color(theme::ACCENT),
                                                )
                                                .fill(theme::CARD_3)
                                                .stroke(Stroke::new(1.0_f32, theme::BORDER)),
                                            )
                                            .clicked()
                                        {
                                            selected = Some((room.name.clone(), room.has_password));
                                        }
                                    });
                                    ui.add_space(2.0);
                                }
                            });
                    }
                }
            });
        self.show_public_rooms = open;
        if let Some((name, has_password)) = selected {
            self.new_name = name;
            self.new_password.clear();
            self.new_public = true;
            self.show_password = false;
            self.focus_password = has_password;
            self.focus_add = false;
            self.show_public_rooms = false;
            self.error = None;
        }
    }

    fn settings_window(&mut self, ctx: &Context) {
        if !self.show_settings {
            return;
        }
        let t = self.language.strings();
        let mut open = self.show_settings;
        egui::Window::new(t.settings_title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(theme::MODAL_WIDTH)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -16.0])
            .frame(theme::modal_frame())
            .show(ctx, |ui| {
                ui.set_width(theme::MODAL_WIDTH);
                theme::section(ui, t.section_identity, t.identity_hint);
                ui.label(
                    RichText::new(self.identity_path.display().to_string())
                        .font(FontId::new(10.0, FontFamily::Monospace))
                        .color(theme::MUTED),
                );
                ui.add_space(4.0);
                let key = self
                    .identity_public
                    .as_deref()
                    .map(short_key)
                    .unwrap_or_else(|| t.not_generated.into());
                theme::key_text(ui, &key);
                ui.horizontal(|ui| {
                    if ui.small_button(t.reload).clicked() {
                        self.identity_public = Identity::load(&self.identity_path)
                            .ok()
                            .map(|identity| identity.public_key_hex());
                    }
                    if let Some(public) = &self.identity_public {
                        if ui.small_button(t.copy).clicked() {
                            ui.ctx().copy_text(public.clone());
                            self.toast(t.copied);
                        }
                    }
                });

                ui.add_space(12.0);
                theme::section(ui, t.section_privileges, "");
                if self.elevated {
                    theme::badge(ui, t.administrator, theme::ACCENT);
                } else {
                    theme::badge(ui, t.limited_mode, theme::AMBER);
                    ui.label(RichText::new(t.limited_hint).size(11.0).color(theme::MUTED));
                    #[cfg(unix)]
                    if ui.button(t.reopen_admin).clicked() {
                        relaunch_as_root();
                    }
                }

                ui.add_space(12.0);
                theme::section(ui, t.section_language, t.language_hint);
                let mut language = self.language;
                theme::segmented(
                    ui,
                    &mut language,
                    &[
                        (Lang::En, Lang::En.short_label()),
                        (Lang::Es, Lang::Es.short_label()),
                        (Lang::Pt, Lang::Pt.short_label()),
                    ],
                );
                if language != self.language {
                    self.language = language;
                    self.save_rooms();
                    self.tray
                        .set_language(ctx.clone(), self.quitting.clone(), language);
                    self.toast(language.strings().language_changed);
                }

                ui.add_space(12.0);
                ui.label(
                    RichText::new(format!(
                        "Nuuvia {} · egui/eframe",
                        env!("CARGO_PKG_VERSION")
                    ))
                    .size(10.0)
                    .color(theme::MUTED),
                );
            });
        self.show_settings = open;
    }
    fn firewall_modal(&mut self, ctx: &Context) {
        if !self.show_firewall {
            return;
        }
        let t = self.language.strings();
        let mut open = self.show_firewall;
        let mut open_room = None;

        egui::Window::new(t.firewall_title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(340.0)
            .vscroll(true)
            .max_height(470.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -12.0])
            .frame(
                Frame::none()
                    .fill(theme::CARD)
                    .rounding(Rounding::same(14.0))
                    .stroke(Stroke::new(1.0_f32, theme::BORDER))
                    .inner_margin(Margin {
                        left: 18.0,
                        right: 4.0,
                        top: 14.0,
                        bottom: 14.0,
                    }),
            )
            .show(ctx, |ui| {
                let content_width = 326.0;
                ui.set_width(content_width);
                ui.set_max_width(content_width);
                theme::section(ui, t.firewall_global, t.firewall_global_hint);
                theme::segmented_fill(
                    ui,
                    &mut self.firewall_global_mode,
                    &[
                        (FirewallMode::All, t.firewall_all),
                        (FirewallMode::Games, t.firewall_games),
                        (FirewallMode::Block, t.firewall_block),
                        (FirewallMode::Custom, t.firewall_custom),
                    ],
                    content_width,
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(firewall_mode_desc(self.firewall_global_mode, t))
                        .size(10.5)
                        .color(theme::MUTED),
                );
                let custom_profile = if self.firewall_global_mode == FirewallMode::Custom {
                    ui.add_space(4.0);
                    custom_rules_editor(
                        ui,
                        &mut self.firewall_global_ports,
                        &mut self.firewall_global_icmp,
                        "global",
                        content_width,
                        &mut self.firewall_global_apps,
                        t,
                    )
                } else {
                    None
                };
                let candidate = if self.firewall_global_mode == FirewallMode::Custom {
                    custom_profile
                } else {
                    Some(FirewallProfile {
                        mode: self.firewall_global_mode,
                        rules: String::new(),
                    })
                };
                let valid = candidate.is_some();
                ui.add_space(4.0);
                if theme::primary_button_sized(ui, t.firewall_apply, valid, content_width).clicked()
                {
                    if let Some(profile) = candidate {
                        self.firewall_default = profile;
                        self.save_rooms();
                        self.apply_global_firewall();
                        self.toast(t.firewall_updated);
                    }
                }

                ui.add_space(12.0);
                theme::section(ui, t.firewall_networks, t.firewall_networks_hint);
                if self.rooms.is_empty() {
                    ui.label(
                        RichText::new(t.no_networks_hint)
                            .size(11.0)
                            .color(theme::MUTED),
                    );
                } else {
                    for (index, room) in self.rooms.iter().enumerate() {
                        let (badge, color) = firewall_badge(room.firewall.as_ref(), t);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(&room.name)
                                    .font(theme::semibold(12.0))
                                    .color(theme::TEXT),
                            );
                            theme::badge(ui, &badge, color);
                            let button_width = 96.0;
                            let spacing = ui.spacing().item_spacing.x;
                            let spacer =
                                (ui.available_width() - button_width - spacing * 1.25).max(0.0);
                            ui.add_space(spacer);
                            if ui
                                .add_sized(
                                    [button_width, 24.0],
                                    egui::Button::new(
                                        RichText::new(t.firewall_configure)
                                            .font(theme::semibold(9.5))
                                            .color(theme::TEXT),
                                    ),
                                )
                                .clicked()
                            {
                                open_room = Some(index);
                            }
                        });
                        ui.add_space(2.0);
                    }
                }
            });
        self.show_firewall = open;
        if let Some(index) = open_room {
            self.open_firewall(index);
        }
    }

    fn firewall_window(&mut self, ctx: &Context) {
        let Some(mut edit) = self.firewall_edit.take() else {
            return;
        };
        let t = self.language.strings();
        let mut open = true;
        let mut apply = false;
        let mut apply_profile: Option<Option<FirewallProfile>> = None;

        egui::Window::new(format!("{} — {}", t.firewall_title, edit.room))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(320.0)
            .vscroll(true)
            .max_height(470.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -16.0])
            .frame(
                Frame::none()
                    .fill(theme::CARD)
                    .rounding(Rounding::same(14.0))
                    .stroke(Stroke::new(1.0_f32, theme::BORDER))
                    .inner_margin(Margin::same(18.0)),
            )
            .show(ctx, |ui| {
                let content_width = 320.0;
                ui.set_width(content_width);
                ui.set_max_width(content_width);
                ui.label(
                    RichText::new(t.section_firewall)
                        .font(theme::semibold(10.0))
                        .color(theme::ACCENT),
                );
                ui.checkbox(&mut edit.inherit, t.firewall_use_global);
                if edit.inherit {
                    ui.label(
                        RichText::new(t.firewall_inherit_hint)
                            .size(11.0)
                            .color(theme::MUTED),
                    );
                    let global_label = match self.firewall_default.mode {
                        FirewallMode::All => t.firewall_all,
                        FirewallMode::Games => t.firewall_games,
                        FirewallMode::Block => t.firewall_block,
                        FirewallMode::Custom => t.firewall_custom,
                    };
                    ui.label(
                        RichText::new(format!("{}: {}", t.firewall_global, global_label))
                            .size(10.0)
                            .color(theme::MUTED),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(firewall_mode_desc(self.firewall_default.mode, t))
                            .size(10.5)
                            .color(theme::MUTED),
                    );
                } else {
                    ui.add_space(4.0);
                    theme::segmented_fill(
                        ui,
                        &mut edit.mode,
                        &[
                            (FirewallMode::All, t.firewall_all),
                            (FirewallMode::Games, t.firewall_games),
                            (FirewallMode::Block, t.firewall_block),
                            (FirewallMode::Custom, t.firewall_custom),
                        ],
                        content_width,
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(firewall_mode_desc(edit.mode, t))
                            .size(10.5)
                            .color(theme::MUTED),
                    );
                }
                let custom_profile = if !edit.inherit && edit.mode == FirewallMode::Custom {
                    ui.add_space(4.0);
                    custom_rules_editor(
                        ui,
                        &mut edit.ports,
                        &mut edit.icmp,
                        "room",
                        content_width,
                        &mut edit.apps,
                        t,
                    )
                } else {
                    None
                };

                let candidate = if edit.inherit {
                    Some(None)
                } else if edit.mode == FirewallMode::Custom {
                    custom_profile.map(Some)
                } else {
                    Some(Some(FirewallProfile {
                        mode: edit.mode,
                        rules: String::new(),
                    }))
                };
                let valid = candidate.is_some();
                apply_profile = candidate;
                ui.add_space(8.0);
                if theme::primary_button_sized(ui, t.firewall_apply, valid, content_width).clicked()
                {
                    apply = true;
                }
            });

        if apply {
            if let Some(profile) = apply_profile {
                if let Some(room) = self.rooms.iter_mut().find(|room| room.name == edit.room) {
                    room.firewall = profile;
                    let effective = room
                        .firewall
                        .clone()
                        .unwrap_or_else(|| self.firewall_default.clone());
                    self.backend.set_firewall(&room.name, effective);
                }
                self.save_rooms();
                self.toast(t.firewall_updated);
            }
            self.firewall_edit = None;
            return;
        }
        if !open {
            self.firewall_edit = None;
        } else {
            self.firewall_edit = Some(edit);
        }
    }

    fn about_window(&mut self, ctx: &Context) {
        if !self.show_about {
            return;
        }
        let t = self.language.strings();
        let mut open = self.show_about;
        let mut close = false;
        egui::Window::new(t.about_title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(theme::MODAL_WIDTH)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -20.0])
            .frame(theme::modal_frame())
            .show(ctx, |ui| {
                ui.set_width(theme::MODAL_WIDTH);
                ui.label(
                    RichText::new("NUUVIA")
                        .font(theme::display(28.0))
                        .color(theme::ACCENT),
                );
                ui.label(
                    RichText::new(t.about_tagline)
                        .font(theme::semibold(13.0))
                        .color(theme::TEXT),
                );
                ui.add_space(4.0);
                ui.label(RichText::new(t.about_stack).size(11.0).color(theme::MUTED));
                ui.label(
                    RichText::new(format!("{} {}", t.version_label, env!("CARGO_PKG_VERSION")))
                        .font(FontId::new(11.0, FontFamily::Monospace))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new(t.about_platform)
                        .size(11.0)
                        .color(theme::MUTED),
                );
                ui.add_space(8.0);
                if ui.small_button(t.close).clicked() {
                    close = true;
                }
            });
        self.show_about = open && !close;
    }
}

impl eframe::App for NuviaApp {
    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        if ctx.input(|input| input.viewport().close_requested())
            && self.tray.is_active()
            && !self.quitting.load(Ordering::SeqCst)
        {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            // Tray icons owned by an elevated Windows process can appear but
            // not receive Explorer clicks because of UIPI. In that case keep
            // the window in the taskbar instead of hiding it forever.
            #[cfg(windows)]
            if backend::is_elevated() {
                ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
            } else {
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            }
            #[cfg(not(windows))]
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        }

        self.poll_backend(ctx);

        if let Some((_, at)) = &self.toast {
            if at.elapsed() > Duration::from_secs(2) {
                self.toast = None;
            }
        }

        egui::TopBottomPanel::top("header")
            .exact_height(56.0)
            .frame(
                Frame::none()
                    .fill(theme::SIDEBAR)
                    .inner_margin(Margin::symmetric(12.0, 12.0)),
            )
            .show(ctx, |ui| self.header(ui));

        egui::TopBottomPanel::top("status")
            .exact_height(64.0)
            .frame(
                Frame::none()
                    .fill(theme::BG_2)
                    .inner_margin(Margin::symmetric(12.0, 7.0)),
            )
            .show(ctx, |ui| self.status_bar(ui));

        egui::CentralPanel::default()
            .frame(
                Frame::none()
                    .fill(theme::BG)
                    .inner_margin(Margin::symmetric(12.0, 12.0)),
            )
            .show(ctx, |ui| {
                theme::paint_background(ui);
                let t = self.language.strings();
                let mut dismiss_error = false;
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let mut actions = Vec::new();
                        self.rooms_list(ui, &mut actions);
                        if let Some(error) = &self.error {
                            if !self.show_add {
                                Frame::none()
                                    .fill(Color32::from_rgb(42, 12, 22))
                                    .rounding(Rounding::same(9.0))
                                    .inner_margin(Margin::same(9.0))
                                    .show(ui, |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(error).color(theme::RED).size(11.0),
                                            )
                                            .wrap(),
                                        );
                                        ui.add_space(2.0);
                                        ui.allocate_ui_with_layout(
                                            egui::vec2(ui.available_width(), 18.0),
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                if ui.small_button(t.close).clicked() {
                                                    dismiss_error = true;
                                                }
                                            },
                                        );
                                    });
                            }
                        }
                        for action in actions {
                            match action {
                                Action::ToggleExpand(index) => {
                                    if let Some(room) = self.rooms.get(index) {
                                        let name = room.name.clone();
                                        self.selected = Some(index);
                                        self.expanded_room = if self.expanded_room.as_deref()
                                            == Some(name.as_str())
                                        {
                                            None
                                        } else {
                                            Some(name)
                                        };
                                    }
                                }
                                Action::Disconnect(index) => self.disconnect(index),
                                Action::Leave(index) => self.leave(index),
                                Action::OpenFirewall(index) => self.open_firewall(index),
                                Action::CopiedIp => {
                                    self.toast(self.language.strings().toast_ip_copied)
                                }
                            }
                        }
                    });
                if dismiss_error {
                    self.error = None;
                    self.error_room = None;
                }
            });

        self.settings_window(ctx);
        self.add_window(ctx);
        self.public_rooms_window(ctx);
        self.about_window(ctx);
        self.firewall_modal(ctx);
        self.firewall_window(ctx);

        if let Some((message, _)) = &self.toast {
            egui::Area::new("toast".into())
                .anchor(egui::Align2::CENTER_BOTTOM, Vec2::new(0.0, -28.0))
                .show(ctx, |ui| {
                    Frame::none()
                        .fill(theme::CARD_3)
                        .rounding(Rounding::same(11.0))
                        .stroke(Stroke::new(1.0_f32, theme::ACCENT_SOFT))
                        .inner_margin(Margin::symmetric(14.0, 9.0))
                        .show(ui, |ui| {
                            ui.label(RichText::new(message).color(theme::TEXT).size(12.0));
                        });
                });
        }
    }
}

fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::fs::PermissionsExt;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(text.as_bytes())?;
        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        drop(file);
        chown_to_session_user(path);
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, text)
    }
}

#[cfg(unix)]
fn chown_to_session_user(path: &std::path::Path) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let uid = std::env::var("SUDO_UID")
        .ok()
        .or_else(|| std::env::var("PKEXEC_UID").ok())
        .and_then(|value| value.parse::<u32>().ok());
    let Some(uid) = uid else {
        return;
    };
    let gid = std::env::var("SUDO_GID")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(-1);
    if let Ok(path) = CString::new(path.as_os_str().as_bytes()) {
        unsafe {
            libc::chown(path.as_ptr(), uid as libc::uid_t, gid as libc::gid_t);
        }
    }
}

const PEER_STALE_AFTER: Duration = Duration::from_secs(25);
const PEER_PING_STALE_AFTER: Duration = Duration::from_secs(8);

fn peer_path(entry: &RoomEntry, tag: &[u8; nuvia_protocol::PEER_TAG_LEN]) -> Option<bool> {
    let (direct, seen) = entry.peer_paths.get(tag)?;
    (seen.elapsed() < PEER_STALE_AFTER).then_some(*direct)
}

fn peer_ping(entry: &RoomEntry, tag: &[u8; nuvia_protocol::PEER_TAG_LEN]) -> Option<u32> {
    let (ms, seen) = entry.peer_pings.get(tag)?;
    (seen.elapsed() < PEER_PING_STALE_AFTER).then_some(*ms)
}

fn ping_color(ping: Option<u32>) -> Color32 {
    match ping {
        Some(ms) if ms <= 80 => theme::ACCENT,
        Some(ms) if ms <= 160 => theme::AMBER,
        Some(_) => theme::RED,
        None => theme::MUTED,
    }
}

fn firewall_badge(profile: Option<&FirewallProfile>, t: &Strings) -> (String, Color32) {
    match profile {
        None => (format!("FW {}", t.firewall_global), theme::MUTED),
        Some(profile) => match profile.mode {
            FirewallMode::All => (format!("FW {}", t.firewall_all), theme::MUTED),
            FirewallMode::Games => (format!("FW {}", t.firewall_games), theme::ACCENT),
            FirewallMode::Block => (format!("FW {}", t.firewall_block), theme::RED),
            FirewallMode::Custom => (format!("FW {}", t.firewall_custom), theme::PURPLE),
        },
    }
}

fn port_rows_from_profile(profile: &FirewallProfile) -> (Vec<PortRow>, bool) {
    let mut rows = Vec::new();
    let mut icmp = false;
    for rule in profile.port_rules().unwrap_or_default() {
        match rule.protocol {
            FirewallRuleProtocol::Tcp => rows.push(PortRow {
                start: rule.start.to_string(),
                end: if rule.start == rule.end {
                    String::new()
                } else {
                    rule.end.to_string()
                },
                protocol: PortChoice::Tcp,
            }),
            FirewallRuleProtocol::Udp => rows.push(PortRow {
                start: rule.start.to_string(),
                end: if rule.start == rule.end {
                    String::new()
                } else {
                    rule.end.to_string()
                },
                protocol: PortChoice::Udp,
            }),
            FirewallRuleProtocol::Icmp => icmp = true,
        }
    }
    (rows, icmp)
}

fn profile_from_port_rows(rows: &[PortRow], icmp: bool) -> Option<FirewallProfile> {
    let mut rules = Vec::new();
    for row in rows {
        let start_text = row.start.trim();
        let end_text = row.end.trim();
        if start_text.is_empty() && end_text.is_empty() {
            continue;
        }
        let start = start_text.parse::<u16>().ok()?;
        let end = if end_text.is_empty() {
            start
        } else {
            end_text.parse::<u16>().ok()?
        };
        if start == 0 || end == 0 || start > end {
            return None;
        }
        for protocol in row.protocol.protocols() {
            rules.push(FirewallRuleView {
                protocol: *protocol,
                start,
                end,
            });
        }
    }
    if icmp {
        rules.push(FirewallRuleView {
            protocol: FirewallRuleProtocol::Icmp,
            start: 0,
            end: 0,
        });
    }
    let mut profile = FirewallProfile::custom("");
    profile.set_port_rules(&rules).ok()?;
    Some(profile)
}

fn custom_rules_editor(
    ui: &mut Ui,
    rows: &mut Vec<PortRow>,
    icmp: &mut bool,
    id_salt: &str,
    content_width: f32,
    apps: &mut AppPicker,
    t: &Strings,
) -> Option<FirewallProfile> {
    ui.allocate_ui_with_layout(
        Vec2::new(content_width, 26.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.label(
                RichText::new(t.firewall_ports)
                    .font(theme::semibold(10.0))
                    .color(theme::ACCENT),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let add = ui
                    .add(
                        egui::Button::new(
                            RichText::new("+")
                                .font(theme::semibold(15.0))
                                .color(theme::ACCENT),
                        )
                        .min_size(Vec2::new(26.0, 26.0)),
                    )
                    .on_hover_text(t.firewall_rules_hint);
                if add.clicked() {
                    rows.insert(0, PortRow::default());
                }
                let apps_button = ui
                    .add(
                        egui::Button::new(
                            RichText::new(t.firewall_apps)
                                .font(theme::semibold(9.5))
                                .color(if apps.open {
                                    Color32::from_rgb(3, 18, 13)
                                } else {
                                    theme::ACCENT
                                }),
                        )
                        .fill(if apps.open {
                            theme::ACCENT
                        } else {
                            theme::CARD_2
                        })
                        .stroke(Stroke::new(
                            1.0_f32,
                            if apps.open {
                                theme::ACCENT
                            } else {
                                theme::BORDER
                            },
                        ))
                        .rounding(Rounding::same(9.0))
                        .min_size(Vec2::new(48.0, 26.0)),
                    )
                    .on_hover_text(t.firewall_apps_hint);
                if apps_button.clicked() {
                    apps.open = !apps.open;
                    if apps.open {
                        apps.entries = apps::scan();
                    }
                }
            });
        },
    );
    ui.label(
        RichText::new(t.firewall_rules_hint)
            .size(10.0)
            .color(theme::MUTED),
    );
    ui.add_space(4.0);
    if apps.open {
        app_picker(ui, apps, rows, t);
    }

    // The grid adds the item spacing after its last cell; trim it so the
    // table's right edge matches the segmented control and the APPLY button.
    let total_width = (content_width - 8.0).max(280.0);
    let protocol_width = 104.0;
    let x_width = 26.0;
    let spacing = 8.0 * 3.0;
    let field_width = ((total_width - protocol_width - x_width - spacing) / 2.0).max(56.0);
    let mut remove = None;
    if rows.is_empty() {
        ui.label(
            RichText::new(t.firewall_no_rules)
                .size(11.0)
                .color(theme::MUTED),
        );
    } else {
        Grid::new(("fw-port-grid", id_salt))
            .num_columns(4)
            .striped(true)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label(
                    RichText::new(t.firewall_port)
                        .font(theme::semibold(9.0))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new(t.firewall_end)
                        .font(theme::semibold(9.0))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new(t.firewall_protocol)
                        .font(theme::semibold(9.0))
                        .color(theme::MUTED),
                );
                ui.label("");
                ui.end_row();

                for (index, row) in rows.iter_mut().enumerate() {
                    ui.add_sized(
                        [field_width, 26.0],
                        TextEdit::singleline(&mut row.start)
                            .hint_text(t.firewall_port)
                            .text_color(theme::TEXT)
                            .background_color(theme::CARD_2)
                            .vertical_align(Align::Center),
                    );
                    ui.add_sized(
                        [field_width, 26.0],
                        TextEdit::singleline(&mut row.end)
                            .hint_text("=")
                            .text_color(theme::TEXT)
                            .background_color(theme::CARD_2)
                            .vertical_align(Align::Center),
                    );
                    let label = row.protocol.label(t);
                    let color = row.protocol.color();
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(label).font(theme::semibold(9.5)).color(color),
                            )
                            .fill(theme::CARD_3)
                            .min_size(Vec2::new(protocol_width, 24.0)),
                        )
                        .on_hover_text(t.firewall_rules_hint)
                        .clicked()
                    {
                        row.protocol = row.protocol.next();
                    }
                    let remove_button = ui.add_sized(
                        [x_width, 26.0],
                        egui::Button::new(
                            RichText::new("x")
                                .font(theme::semibold(10.0))
                                .color(theme::MUTED),
                        )
                        .fill(theme::CARD_3),
                    );
                    if remove_button.clicked() {
                        remove = Some(index);
                    }
                    ui.end_row();
                }
            });
        if let Some(index) = remove {
            rows.remove(index);
        }
    }

    ui.add_space(4.0);
    ui.checkbox(
        icmp,
        RichText::new(t.firewall_icmp)
            .size(11.0)
            .color(theme::MUTED),
    );
    let profile = profile_from_port_rows(rows, *icmp);
    if profile.is_none() {
        ui.label(
            RichText::new(t.firewall_invalid_port)
                .size(10.0)
                .color(theme::RED),
        );
    }
    profile
}

/// Lists processes with listening ports and imports their ports into the
/// custom rule table.
fn app_picker(ui: &mut Ui, picker: &mut AppPicker, rows: &mut Vec<PortRow>, t: &Strings) {
    Frame::none()
        .fill(theme::CARD_2)
        .stroke(Stroke::new(1.0_f32, theme::BORDER))
        .rounding(Rounding::same(10.0))
        .inner_margin(Margin::same(10.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(t.firewall_apps_section)
                        .font(theme::semibold(10.0))
                        .color(theme::ACCENT),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(t.firewall_apps_refresh)
                                    .font(theme::semibold(8.5))
                                    .color(theme::TEXT),
                            )
                            .min_size(Vec2::new(0.0, 22.0)),
                        )
                        .clicked()
                    {
                        picker.entries = apps::scan();
                    }
                });
            });
            ui.label(
                RichText::new(t.firewall_apps_hint)
                    .size(9.5)
                    .color(theme::MUTED),
            );
            ui.add_space(4.0);
            if picker.entries.is_empty() {
                ui.label(
                    RichText::new(t.firewall_apps_empty)
                        .size(10.0)
                        .color(theme::MUTED),
                );
            } else {
                ScrollArea::vertical()
                    .max_height(168.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let row_width = ui.available_width();
                        for app in &picker.entries {
                            let covered = app_is_covered(rows, app);
                            let button_width = 64.0;
                            let spacing = ui.spacing().item_spacing.x;
                            let text_width = (row_width - button_width - spacing).max(100.0);
                            ui.horizontal(|ui| {
                                ui.allocate_ui_with_layout(
                                    Vec2::new(text_width, 30.0),
                                    Layout::top_down(Align::Min),
                                    |ui| {
                                        ui.set_min_width(text_width);
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&app.process)
                                                    .font(theme::semibold(10.5))
                                                    .color(theme::TEXT),
                                            )
                                            .truncate(),
                                        );
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(app.ports_label())
                                                    .size(9.0)
                                                    .color(theme::MUTED),
                                            )
                                            .truncate(),
                                        );
                                    },
                                );
                                let (label, color) = if covered {
                                    (t.firewall_app_remove, theme::RED)
                                } else {
                                    (t.firewall_app_add, theme::ACCENT)
                                };
                                if ui
                                    .add_sized(
                                        [button_width, 22.0],
                                        egui::Button::new(
                                            RichText::new(label)
                                                .font(theme::semibold(8.5))
                                                .color(color),
                                        )
                                        .fill(theme::CARD_3)
                                        .stroke(
                                            Stroke::new(
                                                1.0_f32,
                                                if covered { theme::RED } else { theme::BORDER },
                                            ),
                                        ),
                                    )
                                    .clicked()
                                {
                                    if covered {
                                        remove_app_ports(rows, app);
                                    } else {
                                        add_app_ports(rows, app);
                                    }
                                }
                            });
                            ui.add_space(2.0);
                        }
                    });
            }
        });
    ui.add_space(4.0);
}

/// Adds the ports an app listens on as individual rules at the top of the
/// table, skipping protocols already covered by an existing row.
fn add_app_ports(rows: &mut Vec<PortRow>, app: &ListeningApp) -> usize {
    let mut batch = Vec::new();
    for entry in &app.ports {
        let mut need_tcp = entry.tcp;
        let mut need_udp = entry.udp;
        for row in rows.iter().chain(batch.iter()) {
            if !row_covers_port(row, entry.port) {
                continue;
            }
            match row.protocol {
                PortChoice::Both => {
                    need_tcp = false;
                    need_udp = false;
                }
                PortChoice::Tcp => need_tcp = false,
                PortChoice::Udp => need_udp = false,
            }
        }
        if !need_tcp && !need_udp {
            continue;
        }
        let protocol = match (need_tcp, need_udp) {
            (true, true) => PortChoice::Both,
            (true, false) => PortChoice::Tcp,
            (false, true) => PortChoice::Udp,
            (false, false) => continue,
        };
        batch.push(PortRow {
            start: entry.port.to_string(),
            end: entry.port.to_string(),
            protocol,
        });
    }
    batch.sort_by_key(|row| row.start.parse::<u16>().unwrap_or(u16::MAX));
    let added = batch.len();
    rows.splice(0..0, batch);
    added
}

fn app_is_covered(rows: &[PortRow], app: &ListeningApp) -> bool {
    app.ports.iter().all(|entry| {
        (!entry.tcp || port_allows(rows, entry.port, true))
            && (!entry.udp || port_allows(rows, entry.port, false))
    })
}

fn port_allows(rows: &[PortRow], port: u16, tcp: bool) -> bool {
    rows.iter().any(|row| {
        row_covers_port(row, port)
            && if tcp {
                row.protocol.covers_tcp()
            } else {
                row.protocol.covers_udp()
            }
    })
}

/// Removes the ports an app listens on, splitting ranges so the other ports
/// keep their rules.
fn remove_app_ports(rows: &mut Vec<PortRow>, app: &ListeningApp) {
    for entry in &app.ports {
        remove_port(rows, entry.port, entry.tcp, entry.udp);
    }
}

fn remove_port(rows: &mut Vec<PortRow>, port: u16, tcp: bool, udp: bool) {
    let mut result = Vec::with_capacity(rows.len() + 2);
    for row in rows.drain(..) {
        let Some((start, end)) = row_range(&row) else {
            result.push(row);
            continue;
        };
        if port < start || port > end {
            result.push(row);
            continue;
        }
        let mut keep_tcp = row.protocol.covers_tcp();
        let mut keep_udp = row.protocol.covers_udp();
        if tcp {
            keep_tcp = false;
        }
        if udp {
            keep_udp = false;
        }
        if start < port {
            result.push(range_row(start, port - 1, row.protocol));
        }
        if keep_tcp || keep_udp {
            let protocol = match (keep_tcp, keep_udp) {
                (true, true) => PortChoice::Both,
                (true, false) => PortChoice::Tcp,
                (false, true) => PortChoice::Udp,
                (false, false) => unreachable!(),
            };
            result.push(range_row(port, port, protocol));
        }
        if port < end {
            result.push(range_row(port + 1, end, row.protocol));
        }
    }
    *rows = result;
}

fn range_row(start: u16, end: u16, protocol: PortChoice) -> PortRow {
    PortRow {
        start: start.to_string(),
        end: end.to_string(),
        protocol,
    }
}

fn row_range(row: &PortRow) -> Option<(u16, u16)> {
    let start = row.start.trim().parse::<u16>().ok()?;
    let end = match row.end.trim() {
        "" | "=" => start,
        value => value.parse::<u16>().ok()?,
    };
    (start <= end).then_some((start, end))
}

fn row_covers_port(row: &PortRow, port: u16) -> bool {
    row_range(row).is_some_and(|(start, end)| start <= port && port <= end)
}

fn firewall_mode_desc(mode: FirewallMode, t: &Strings) -> &'static str {
    match mode {
        FirewallMode::All => t.firewall_desc_all,
        FirewallMode::Games => t.firewall_desc_games,
        FirewallMode::Block => t.firewall_desc_block,
        FirewallMode::Custom => t.firewall_desc_custom,
    }
}

/// Keeps a peer that the rendezvous server still lists as online visible even
/// before the first mesh frame arrives. A fresh direct/relay status wins; a
/// stale entry falls back to relay/unknown instead of gray.
fn mark_participants_online(entry: &mut RoomEntry, peers: &[PeerView]) {
    let now = Instant::now();
    entry
        .peer_paths
        .retain(|tag, _| peers.iter().any(|peer| peer.tag == *tag));
    for peer in peers {
        match entry.peer_paths.get_mut(&peer.tag) {
            Some((_, seen)) if seen.elapsed() < PEER_STALE_AFTER => {}
            Some((direct, seen)) => {
                *direct = false;
                *seen = now;
            }
            None => {
                entry.peer_paths.insert(peer.tag, (false, now));
            }
        }
    }
}

/// Summarizes the individual paths in the room badge: `DIRETO` only when
/// every active peer is direct, `RELAY` when at least one only reaches us
/// through the relay. Peers without a recent packet stay gray and are
/// ignored in this summary.
fn refresh_room_summary(entry: &mut RoomEntry) {
    if !matches!(
        entry.state,
        RoomState::Connecting | RoomState::Connected { .. }
    ) {
        return;
    }
    let mut known = 0usize;
    let mut all_direct = true;
    for peer in &entry.participants {
        if let Some(direct) = peer_path(entry, &peer.tag) {
            known += 1;
            all_direct &= direct;
        }
    }
    if known > 0 {
        entry.state = RoomState::Connected { relay: !all_direct };
    }
}

fn participant_row(
    ui: &mut Ui,
    name: &str,
    ip: Option<&str>,
    is_self: bool,
    dot_color: Color32,
    ping: Option<u32>,
    t: &Strings,
) -> bool {
    let row = ui.horizontal(|ui| {
        let (dot_rect, _) = ui.allocate_exact_size(Vec2::splat(6.0), egui::Sense::hover());
        ui.painter()
            .circle_filled(dot_rect.center(), 2.5, dot_color);
        ui.label(
            RichText::new(name)
                .font(theme::semibold(11.0))
                .color(theme::TEXT),
        );
        if is_self {
            ui.label(RichText::new(t.you).size(9.0).color(theme::MUTED));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(ip.unwrap_or("—"))
                    .font(FontId::new(10.0, FontFamily::Monospace))
                    .color(theme::MUTED),
            );
        });
    });

    let mut copied = false;
    if let Some(ip) = ip {
        let response = ui.interact(
            row.response.rect,
            ui.make_persistent_id(("participant-menu", name, ip)),
            egui::Sense::click(),
        );
        response.context_menu(|ui| {
            if ui.button(t.copy_ip).clicked() {
                ui.ctx().copy_text(ip.to_string());
                copied = true;
                ui.close_menu();
            }
        });
    }

    if !is_self && ui.rect_contains_pointer(row.response.rect) {
        let rect = row.response.rect;
        let color = ping_color(ping);
        let text = match ping {
            Some(ms) => format!("{ms} ms"),
            None => t.ping_measuring.to_string(),
        };
        let galley =
            ui.painter()
                .layout_no_wrap(text, FontId::new(9.5, FontFamily::Proportional), color);
        let text_pos = egui::pos2(
            rect.right() - 5.0 - galley.size().x,
            rect.center().y - galley.size().y / 2.0,
        );
        let dot = egui::pos2(text_pos.x - 8.0, rect.center().y);
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.right() - 120.0, rect.top() + 1.0),
                egui::pos2(rect.right(), rect.bottom() - 1.0),
            ),
            Rounding::same(4.0),
            theme::CARD_2,
        );
        ui.painter().circle_filled(dot, 3.0, color);
        ui.painter().galley(text_pos, galley, color);
    }

    copied
}

fn short_key(key: &str) -> String {
    if key.len() <= 18 {
        key.to_string()
    } else {
        format!("{}…{}", &key[..10], &key[key.len() - 6..])
    }
}

#[cfg(unix)]
fn relaunch_as_root() {
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let display = std::env::var("DISPLAY").unwrap_or_default();
    let xauthority = std::env::var("XAUTHORITY").unwrap_or_default();
    let _ = std::process::Command::new("pkexec")
        .arg("env")
        .arg(format!("DISPLAY={display}"))
        .arg(format!("XAUTHORITY={xauthority}"))
        .arg(exe)
        .exec();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn participants_are_online_before_the_first_mesh_frame() {
        let mut entry = RoomEntry {
            name: "casa".into(),
            password: "casa".into(),
            public: false,
            tap_name: "nuvia-vpn0".into(),
            bind_port: 42424,
            state: RoomState::Connected { relay: true },
            own_ip: None,
            participants: Vec::new(),
            peer_paths: HashMap::new(),
            peer_pings: HashMap::new(),
            log: VecDeque::new(),
            firewall: None,
        };
        let peer = PeerView {
            name: "WIN-TEST".into(),
            ip: "10.240.119.198".into(),
            tag: [7; nuvia_protocol::PEER_TAG_LEN],
        };

        mark_participants_online(&mut entry, std::slice::from_ref(&peer));
        assert_eq!(peer_path(&entry, &peer.tag), Some(false));

        entry.peer_paths.insert(peer.tag, (true, Instant::now()));
        mark_participants_online(&mut entry, std::slice::from_ref(&peer));
        assert_eq!(peer_path(&entry, &peer.tag), Some(true));
    }

    #[test]
    fn importing_app_ports_adds_missing_protocols_only() {
        use crate::apps::ListeningPort;

        let mut rows = vec![PortRow {
            start: "25565".into(),
            end: "25565".into(),
            protocol: PortChoice::Tcp,
        }];
        let app = ListeningApp {
            pid: 7,
            process: "game".into(),
            ports: vec![
                ListeningPort {
                    port: 25565,
                    tcp: true,
                    udp: true,
                },
                ListeningPort {
                    port: 19132,
                    tcp: false,
                    udp: true,
                },
            ],
        };

        assert_eq!(add_app_ports(&mut rows, &app), 2);
        assert!(app_is_covered(&rows, &app));
        assert_eq!(rows[0].start, "19132");
        assert_eq!(rows[0].protocol, PortChoice::Udp);
        assert_eq!(rows[1].start, "25565");
        assert_eq!(rows[1].protocol, PortChoice::Udp);
        assert_eq!(rows[2].start, "25565");
        assert_eq!(rows[2].protocol, PortChoice::Tcp);
        assert_eq!(add_app_ports(&mut rows, &app), 0);
    }

    #[test]
    fn removing_an_app_splits_ranges_and_drops_protocols() {
        use crate::apps::ListeningPort;

        let mut rows = vec![
            PortRow {
                start: "19000".into(),
                end: "19200".into(),
                protocol: PortChoice::Both,
            },
            PortRow {
                start: "25565".into(),
                end: "25565".into(),
                protocol: PortChoice::Tcp,
            },
        ];
        let app = ListeningApp {
            pid: 7,
            process: "game".into(),
            ports: vec![
                ListeningPort {
                    port: 19132,
                    tcp: false,
                    udp: true,
                },
                ListeningPort {
                    port: 25565,
                    tcp: true,
                    udp: false,
                },
            ],
        };

        assert!(app_is_covered(&rows, &app));
        remove_app_ports(&mut rows, &app);
        assert!(!app_is_covered(&rows, &app));
        assert_eq!(rows.len(), 3);
        assert_eq!(
            (rows[0].start.as_str(), rows[0].end.as_str()),
            ("19000", "19131")
        );
        assert_eq!(
            (rows[1].start.as_str(), rows[1].end.as_str()),
            ("19132", "19132")
        );
        assert_eq!(rows[1].protocol, PortChoice::Tcp);
        assert_eq!(
            (rows[2].start.as_str(), rows[2].end.as_str()),
            ("19133", "19200")
        );

        assert_eq!(add_app_ports(&mut rows, &app), 2);
        assert!(app_is_covered(&rows, &app));
    }

    #[test]
    fn ranges_cover_imported_ports() {
        let rows = [PortRow {
            start: "19000".into(),
            end: "19200".into(),
            protocol: PortChoice::Both,
        }];
        assert!(row_covers_port(&rows[0], 19132));
        assert!(!row_covers_port(&rows[0], 20000));
    }

    #[test]
    fn stored_rooms_roundtrip() {
        let stored = StoredRooms {
            rooms: vec![
                StoredRoom {
                    name: "casa".into(),
                    password: "segredo".into(),
                    public: true,
                    firewall: Some("games".into()),
                },
                StoredRoom {
                    name: "lan".into(),
                    password: String::new(),
                    public: false,
                    firewall: None,
                },
            ],
            language: Some(Lang::Pt.code().into()),
            firewall: Some("block".into()),
        };
        let text = toml::to_string_pretty(&stored).expect("serialize");
        let parsed: StoredRooms = toml::from_str(&text).expect("parse");
        assert_eq!(parsed.language.as_deref(), Some("pt"));
        assert_eq!(parsed.rooms.len(), 2);
        assert_eq!(parsed.rooms[0].name, "casa");
        assert_eq!(parsed.rooms[0].password, "segredo");
        assert!(parsed.rooms[0].public);
        assert_eq!(parsed.rooms[0].firewall.as_deref(), Some("games"));
        assert_eq!(parsed.rooms[1].name, "lan");
        assert!(parsed.rooms[1].password.is_empty());
        assert!(!parsed.rooms[1].public);
        assert_eq!(parsed.rooms[1].firewall, None);
        assert_eq!(parsed.firewall.as_deref(), Some("block"));
    }
}
