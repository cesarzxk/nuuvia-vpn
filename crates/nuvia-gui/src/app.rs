use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, Context, FontFamily, FontId, Frame, Layout, Margin, RichText, Rounding,
    ScrollArea, Stroke, TextEdit, Ui, Vec2, ViewportCommand,
};
use nuvia_core::tunnel::DiscoveryRoutes;
use nuvia_core::Identity;
use serde::{Deserialize, Serialize};

use crate::backend::{self, Backend, Event, PeerView, Phase, RoomConfig};
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
    fn pill(self) -> (&'static str, Color32) {
        match self {
            RoomState::Starting => ("INICIANDO", theme::AMBER),
            RoomState::Searching => ("BUSCANDO", theme::AMBER),
            RoomState::Connecting => ("CONECTANDO", theme::ACCENT),
            RoomState::Connected { relay: true } => ("RELAY", theme::PURPLE),
            RoomState::Connected { relay: false } => ("DIRETO", theme::ACCENT),
            RoomState::Stopped => ("OFF", theme::MUTED),
            RoomState::Error => ("ERRO", theme::RED),
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
    tap_name: String,
    bind_port: u16,
    state: RoomState,
    own_ip: Option<String>,
    participants: Vec<PeerView>,
    peer_paths: HashMap<[u8; nuvia_protocol::PEER_TAG_LEN], (bool, Instant)>,
    log: VecDeque<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredRooms {
    #[serde(default)]
    rooms: Vec<StoredRoom>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredRoom {
    name: String,
    password: String,
}

enum Action {
    ToggleExpand(usize),
    Disconnect(usize),
    Leave(usize),
}

pub struct NuviaApp {
    backend: Backend,
    rooms: Vec<RoomEntry>,
    selected: Option<usize>,
    expanded_room: Option<String>,

    new_name: String,
    new_password: String,
    show_password: bool,
    show_add: bool,
    add_mode: AddMode,
    focus_add: bool,

    identity_path: PathBuf,
    identity_public: Option<String>,
    identity_name: String,
    rooms_path: PathBuf,
    elevated: bool,
    show_settings: bool,
    show_about: bool,

    discovery: Option<DiscoveryRoutes>,
    active_room: Option<String>,

    tray: Tray,
    quitting: Arc<AtomicBool>,

    error: Option<String>,
    toast: Option<(String, Instant)>,
}

impl NuviaApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let identity_path = backend::user_identity_path();
        let identity = Identity::load(&identity_path).ok();
        let identity_public = identity.as_ref().map(|identity| identity.public_key_hex());
        let identity_name = identity
            .as_ref()
            .and_then(|identity| identity.name().map(str::to_string))
            .or_else(|| std::env::var("USER").ok())
            .or_else(|| std::env::var("USERNAME").ok())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "usuário".into());

        let rooms_path = identity_path.with_file_name("rooms.toml");
        let mut backend = Backend::new().expect("tokio runtime");
        let mut rooms = Vec::new();
        if let Ok(text) = std::fs::read_to_string(&rooms_path) {
            match toml::from_str::<StoredRooms>(&text) {
                Ok(stored) => {
                    for saved in stored.rooms {
                        let name = saved.name.trim().to_string();
                        if name.is_empty()
                            || saved.password.is_empty()
                            || rooms.iter().any(|room: &RoomEntry| room.name == name)
                        {
                            continue;
                        }
                        let (tap_name, bind_port) = backend.allocate_slot();
                        rooms.push(RoomEntry {
                            name,
                            password: saved.password,
                            tap_name,
                            bind_port,
                            state: RoomState::Stopped,
                            own_ip: None,
                            participants: Vec::new(),
                            peer_paths: HashMap::new(),
                            log: VecDeque::new(),
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
        let tray = Tray::spawn(cc.egui_ctx.clone(), quitting.clone());

        Self {
            backend,
            rooms,
            selected,
            expanded_room: None,
            new_name: String::new(),
            new_password: String::new(),
            show_password: false,
            show_add: false,
            add_mode: AddMode::Create,
            focus_add: false,
            identity_path,
            identity_public,
            identity_name,
            rooms_path,
            elevated: backend::is_elevated(),
            show_settings: false,
            show_about: false,
            discovery: None,
            active_room: None,
            tray,
            quitting,
            error: None,
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
                })
                .collect(),
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
        self.error = None;
        self.new_name.clear();
        self.new_password.clear();
        self.add_mode = mode;
        self.show_add = true;
        self.focus_add = true;
    }

    fn join(&mut self) -> bool {
        self.error = None;
        let name = self.new_name.trim().to_string();
        let password = self.new_password.clone();

        if name.is_empty() {
            self.error = Some("Digite o nome da rede.".into());
            return false;
        }
        if password.is_empty() {
            self.error = Some("Digite a senha da rede.".into());
            return false;
        }
        if self.rooms.iter().any(|room| room.name == name) {
            self.error = Some("Essa rede já está na lista.".into());
            return false;
        }

        let (tap_name, bind_port) = self.backend.allocate_slot();
        let config = RoomConfig {
            name: name.clone(),
            password: password.clone(),
            tap_name: tap_name.clone(),
            bind_port,
            identity_path: self.identity_path.clone(),
        };

        match self.backend.join(config) {
            Ok(()) => {
                let mut log = VecDeque::new();
                log.push_back("Entrando na rede...".to_string());
                self.rooms.insert(
                    0,
                    RoomEntry {
                        name,
                        password,
                        tap_name,
                        bind_port,
                        state: RoomState::Starting,
                        own_ip: None,
                        participants: Vec::new(),
                        peer_paths: HashMap::new(),
                        log,
                    },
                );
                self.selected = Some(0);
                self.new_password.clear();
                self.new_name.clear();
                self.toast("Rede adicionada.");
                self.save_rooms();
                true
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                false
            }
        }
    }

    fn leave(&mut self, index: usize) {
        if index >= self.rooms.len() {
            return;
        }
        let room = self.rooms.remove(index);
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
        self.toast(format!("Rede {} removida.", room.name));
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
            entry.log.push_back("Desconectado.".into());
            while entry.log.len() > 40 {
                entry.log.pop_front();
            }
        }
        self.toast(format!("Rede {name} desconectada."));
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
        let (name, password, tap_name, bind_port) = {
            let room = &self.rooms[index];
            (
                room.name.clone(),
                room.password.clone(),
                room.tap_name.clone(),
                room.bind_port,
            )
        };
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
        };
        match self.backend.join(config) {
            Ok(()) => {
                let room = &mut self.rooms[index];
                room.state = RoomState::Starting;
                room.own_ip = None;
                room.participants.clear();
                room.peer_paths.clear();
                room.log.clear();
                room.log.push_back("Reconectando...".into());
                self.toast("Reconectando.");
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
            }
        }
    }

    fn activate(&mut self, index: usize) {
        if index >= self.rooms.len() {
            return;
        }
        let room = &self.rooms[index];
        if !room.state.connected() {
            self.toast("Conecte a rede antes de ativá-la para jogos.");
            return;
        }
        if !self.elevated {
            self.error =
                Some("Execute o nuvia como administrador para instalar as rotas de jogos.".into());
            return;
        }

        let tap = room.tap_name.clone();
        let name = room.name.clone();
        self.active_room = Some(name.clone());
        // The TAP is shared by every room; install the discovery routes only
        // once per app session, not every time a room is toggled.
        if self.discovery.is_none() {
            self.discovery = Some(DiscoveryRoutes::install(&tap));
            self.log_for(
                &name,
                "Rotas de descoberta instaladas no TAP compartilhado.",
            );
            self.toast("Descoberta de jogos ativa no TAP compartilhado.");
        }
    }

    fn poll_backend(&mut self, ctx: &Context) {
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
                        Phase::Starting => "Preparando identidade e socket.",
                        Phase::Searching => "Entrando na sala.",
                        Phase::Connecting => "Preparando o TAP e o relay.",
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
                    if let Some(entry) = self.rooms.iter_mut().find(|entry| entry.name == room) {
                        entry.own_ip = Some(own_ip);
                        entry
                            .peer_paths
                            .retain(|tag, _| peers.iter().any(|peer| peer.tag == *tag));
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
                Event::Connected { room, relay } => {
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
                            "Conectado via relay.".into()
                        } else {
                            "Conectado direto.".into()
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
                    if self.active_room.as_deref() == Some(room.as_str()) {
                        self.active_room = None;
                        self.activate_next_connected();
                    }
                    if let Some(entry) = self.rooms.iter_mut().find(|entry| entry.name == room) {
                        entry.state = RoomState::Error;
                        entry.log.push_back(format!("ERRO: {message}"));
                        self.error = Some(format!("{}: {message}", entry.name));
                    }
                }
                Event::Stopped { room } => {
                    if self.active_room.as_deref() == Some(room.as_str()) {
                        self.active_room = None;
                        self.activate_next_connected();
                    }
                    if let Some(entry) = self.rooms.iter_mut().find(|entry| entry.name == room) {
                        if entry.state != RoomState::Error {
                            entry.state = RoomState::Stopped;
                            entry.log.push_back("Sessão encerrada.".into());
                        }
                    }
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
        let target = self.bar_target();
        let (ip, state_label, state_color, connected, busy) = match target {
            Some(index) => {
                let room = &self.rooms[index];
                let (label, color) = room.state.pill();
                (
                    room.own_ip.clone().unwrap_or_else(|| "—".into()),
                    label,
                    color,
                    room.state.connected(),
                    room.state.busy(),
                )
            }
            None => ("—".to_string(), "SEM REDE", theme::MUTED, false, false),
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
                "Desconectar esta rede"
            } else if busy {
                "Cancelar a conexão"
            } else {
                "Conectar esta rede"
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
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("NUVIA")
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
                    RichText::new(format!(
                        "{} rede(s) · {} conectada(s)",
                        self.rooms.len(),
                        connected
                    ))
                    .size(11.0)
                    .color(theme::MUTED),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.menu_button(
                    RichText::new("AJUDA")
                        .font(theme::semibold(12.0))
                        .color(theme::MUTED),
                    |ui| {
                        ui.set_min_width(150.0);
                        if ui.button("AJUSTES").clicked() {
                            self.show_settings = true;
                            ui.close_menu();
                        }
                        if ui.button("SOBRE").clicked() {
                            self.show_about = true;
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("SAIR").clicked() {
                            self.save_rooms();
                            self.quitting.store(true, Ordering::SeqCst);
                            std::process::exit(0);
                        }
                    },
                );
                ui.menu_button(
                    RichText::new("REDE")
                        .font(theme::semibold(12.0))
                        .color(theme::ACCENT),
                    |ui| {
                        ui.set_min_width(168.0);
                        if ui.button("CRIAR REDE").clicked() {
                            self.open_add(AddMode::Create);
                            ui.close_menu();
                        }
                        if ui.button("ENTRAR EM REDE").clicked() {
                            self.open_add(AddMode::Join);
                            ui.close_menu();
                        }
                    },
                )
                .response
                .on_hover_text("Criar ou entrar em uma rede");
                if !self.elevated {
                    theme::badge(ui, "MODO LIMITADO", theme::AMBER);
                }
            });
        });
    }

    fn room_card(&self, ui: &mut Ui, index: usize, actions: &mut Vec<Action>) {
        let room = &self.rooms[index];
        let selected = self.selected == Some(index);
        let expanded = self.expanded_room.as_deref() == Some(room.name.as_str());
        let (state_label, state_color) = room.state.pill();

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
                .add_enabled(can_disconnect, egui::Button::new("DESATIVAR"))
                .clicked()
            {
                actions.push(Action::Disconnect(index));
                ui.close_menu();
            }
            if ui.button("SAIR").clicked() {
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
                        RichText::new("PARTICIPANTES")
                            .font(theme::semibold(8.5))
                            .color(theme::MUTED),
                    );
                    participant_row(
                        ui,
                        &self.identity_name,
                        room.own_ip.as_deref(),
                        true,
                        state_color,
                    );
                    for peer in &room.participants {
                        let color = match peer_path(room, &peer.tag) {
                            Some(true) => theme::ACCENT,
                            Some(false) => theme::PURPLE,
                            None => theme::MUTED,
                        };
                        participant_row(ui, &peer.name, Some(&peer.ip), false, color);
                    }
                    if room.participants.is_empty() && room.state.busy() {
                        ui.label(
                            RichText::new("Aguardando outros participantes...")
                                .size(10.0)
                                .color(theme::MUTED),
                        );
                    }
                    if !room.participants.is_empty() {
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            for (color, label) in [
                                (theme::ACCENT, "direto"),
                                (theme::PURPLE, "relay"),
                                (theme::MUTED, "offline"),
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
        if self.rooms.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("NENHUMA REDE")
                        .font(theme::semibold(14.0))
                        .color(theme::MUTED),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Use o menu REDE no topo para criar ou entrar. Cada rede recebe um IP virtual próprio.",
                    )
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

        let mut open = true;
        let mut close = false;
        let (title, kicker, hint, action) = match self.add_mode {
            AddMode::Create => (
                "Criar rede",
                "CRIAR REDE",
                "Crie a rede neste PC e use o mesmo nome e senha no outro para entrar.",
                "CRIAR REDE",
            ),
            AddMode::Join => (
                "Entrar em rede",
                "ENTRAR EM REDE",
                "Use o nome e a senha combinados. Se a rede ainda não existir, ela é criada agora.",
                "ENTRAR",
            ),
        };
        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -24.0])
            .frame(
                Frame::none()
                    .fill(theme::CARD)
                    .rounding(Rounding::same(14.0))
                    .stroke(Stroke::new(1.0_f32, theme::BORDER))
                    .inner_margin(Margin::same(18.0)),
            )
            .show(ctx, |ui| {
                ui.set_width(320.0);
                ui.label(
                    RichText::new(kicker)
                        .font(theme::semibold(10.0))
                        .color(theme::ACCENT),
                );
                ui.label(RichText::new(hint).size(11.0).color(theme::MUTED));
                ui.add_space(10.0);

                ui.label(
                    RichText::new("NOME DA REDE")
                        .font(theme::semibold(9.0))
                        .color(theme::MUTED),
                );
                let name = ui.add_sized(
                    [320.0, 36.0],
                    TextEdit::singleline(&mut self.new_name)
                        .hint_text("ex.: casa")
                        .text_color(theme::TEXT)
                        .background_color(theme::CARD_2)
                        .vertical_align(Align::Center),
                );
                if self.focus_add {
                    name.request_focus();
                    self.focus_add = false;
                }

                ui.add_space(2.0);
                ui.label(
                    RichText::new("SENHA")
                        .font(theme::semibold(9.0))
                        .color(theme::MUTED),
                );
                let password = ui.add_sized(
                    [320.0, 36.0],
                    TextEdit::singleline(&mut self.new_password)
                        .password(!self.show_password)
                        .hint_text("senha combinada entre os PCs")
                        .text_color(theme::TEXT)
                        .background_color(theme::CARD_2)
                        .vertical_align(Align::Center),
                );
                ui.checkbox(
                    &mut self.show_password,
                    RichText::new("Mostrar senha")
                        .size(11.0)
                        .color(theme::MUTED),
                );

                let enabled = !self.new_name.trim().is_empty() && !self.new_password.is_empty();
                let enter = password.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.add_space(4.0);
                let submitted =
                    theme::primary_button(ui, action, enabled).clicked() || (enabled && enter);
                if submitted && self.join() {
                    close = true;
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
    }

    fn settings_window(&mut self, ctx: &Context) {
        if !self.show_settings {
            return;
        }
        let mut open = self.show_settings;
        egui::Window::new("Ajustes")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(300.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -16.0])
            .frame(
                Frame::none()
                    .fill(theme::CARD)
                    .rounding(Rounding::same(14.0))
                    .stroke(Stroke::new(1.0_f32, theme::BORDER))
                    .inner_margin(Margin::same(14.0)),
            )
            .show(ctx, |ui| {
                ui.set_width(300.0);
                theme::section(ui, "IDENTIDADE", "Chave X25519 usada em todas as redes.");
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
                    .unwrap_or_else(|| "não gerada".into());
                theme::key_text(ui, &key);
                ui.horizontal(|ui| {
                    if ui.small_button("RECARREGAR").clicked() {
                        self.identity_public = Identity::load(&self.identity_path)
                            .ok()
                            .map(|identity| identity.public_key_hex());
                    }
                    if let Some(public) = &self.identity_public {
                        if ui.small_button("COPIAR").clicked() {
                            ui.ctx().copy_text(public.clone());
                            self.toast("Chave copiada.");
                        }
                    }
                });

                ui.add_space(12.0);
                theme::section(ui, "PRIVILÉGIOS", "");
                if self.elevated {
                    theme::badge(ui, "ADMINISTRADOR", theme::ACCENT);
                } else {
                    theme::badge(ui, "MODO LIMITADO", theme::AMBER);
                    ui.label(
                        RichText::new("Sem root/admin o TAP e as rotas de jogos não são criados.")
                            .size(11.0)
                            .color(theme::MUTED),
                    );
                    #[cfg(unix)]
                    if ui.button("REABRIR COMO ADMIN").clicked() {
                        relaunch_as_root();
                    }
                }

                ui.add_space(12.0);
                ui.label(
                    RichText::new(format!("nuvia {} · egui/eframe", env!("CARGO_PKG_VERSION")))
                        .size(10.0)
                        .color(theme::MUTED),
                );
            });
        self.show_settings = open;
    }
    fn about_window(&mut self, ctx: &Context) {
        if !self.show_about {
            return;
        }
        let mut open = self.show_about;
        let mut close = false;
        egui::Window::new("Sobre")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -20.0])
            .frame(
                Frame::none()
                    .fill(theme::CARD)
                    .rounding(Rounding::same(14.0))
                    .stroke(Stroke::new(1.0_f32, theme::BORDER))
                    .inner_margin(Margin::same(18.0)),
            )
            .show(ctx, |ui| {
                ui.set_width(300.0);
                ui.label(
                    RichText::new("NUVIA")
                        .font(theme::display(28.0))
                        .color(theme::ACCENT),
                );
                ui.label(
                    RichText::new("LAN virtual criptografada para jogos.")
                        .font(theme::semibold(13.0))
                        .color(theme::TEXT),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Layer 2 · Noise XX · ChaCha20-Poly1305")
                        .size(11.0)
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new(format!("versão {}", env!("CARGO_PKG_VERSION")))
                        .font(FontId::new(11.0, FontFamily::Monospace))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new("Rust + egui · Linux e Windows")
                        .size(11.0)
                        .color(theme::MUTED),
                );
                ui.add_space(8.0);
                if ui.small_button("FECHAR").clicked() {
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
                                        ui.label(RichText::new(error).color(theme::RED).size(11.0));
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
                            }
                        }
                    });
            });

        self.settings_window(ctx);
        self.add_window(ctx);
        self.about_window(ctx);

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

fn peer_path(entry: &RoomEntry, tag: &[u8; nuvia_protocol::PEER_TAG_LEN]) -> Option<bool> {
    let (direct, seen) = entry.peer_paths.get(tag)?;
    (seen.elapsed() < PEER_STALE_AFTER).then_some(*direct)
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

fn participant_row(ui: &mut Ui, name: &str, ip: Option<&str>, is_self: bool, dot_color: Color32) {
    ui.horizontal(|ui| {
        let (dot_rect, _) = ui.allocate_exact_size(Vec2::splat(6.0), egui::Sense::hover());
        ui.painter()
            .circle_filled(dot_rect.center(), 2.5, dot_color);
        ui.label(
            RichText::new(name)
                .font(theme::semibold(11.0))
                .color(theme::TEXT),
        );
        if is_self {
            ui.label(RichText::new("você").size(9.0).color(theme::MUTED));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(ip.unwrap_or("—"))
                    .font(FontId::new(10.0, FontFamily::Monospace))
                    .color(theme::MUTED),
            );
        });
    });
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
    fn stored_rooms_roundtrip() {
        let stored = StoredRooms {
            rooms: vec![
                StoredRoom {
                    name: "casa".into(),
                    password: "segredo".into(),
                },
                StoredRoom {
                    name: "lan".into(),
                    password: "outra".into(),
                },
            ],
        };
        let text = toml::to_string_pretty(&stored).expect("serialize");
        let parsed: StoredRooms = toml::from_str(&text).expect("parse");
        assert_eq!(parsed.rooms.len(), 2);
        assert_eq!(parsed.rooms[0].name, "casa");
        assert_eq!(parsed.rooms[0].password, "segredo");
        assert_eq!(parsed.rooms[1].name, "lan");
    }
}
