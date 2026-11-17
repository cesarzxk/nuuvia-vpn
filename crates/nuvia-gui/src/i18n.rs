//! Minimal UI localization. The language is selected in Settings and stored in
//! `rooms.toml`, so the GUI, tray, and error messages use the same locale.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Es,
    Pt,
}

impl Lang {
    pub fn from_code(code: &str) -> Option<Self> {
        let code = code.trim().to_ascii_lowercase();
        if code.starts_with("pt") {
            Some(Self::Pt)
        } else if code.starts_with("es") {
            Some(Self::Es)
        } else if code.starts_with("en") {
            Some(Self::En)
        } else {
            None
        }
    }

    /// Explicit override for tests and screenshots.
    pub fn from_env() -> Option<Self> {
        std::env::var("NUVIA_LANG")
            .ok()
            .and_then(|value| Self::from_code(&value))
    }

    pub fn from_locale() -> Self {
        for key in ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"] {
            if let Ok(value) = std::env::var(key) {
                if let Some(lang) = value.split(':').next().and_then(Self::from_code) {
                    return lang;
                }
            }
        }
        Self::En
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Es => "es",
            Self::Pt => "pt",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::En => "EN",
            Self::Es => "ES",
            Self::Pt => "PT",
        }
    }

    pub fn strings(self) -> &'static Strings {
        match self {
            Self::En => &EN,
            Self::Es => &ES,
            Self::Pt => &PT,
        }
    }
}

pub fn render(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    out.push_str(template);
    for (key, value) in values {
        out = out.replace(&format!("{{{key}}}"), value);
    }
    out
}

pub struct Strings {
    pub user_fallback: &'static str,
    pub language_changed: &'static str,

    pub err_enter_room_name: &'static str,
    pub err_enter_password: &'static str,
    pub err_room_exists: &'static str,
    pub log_joining: &'static str,
    pub toast_room_added: &'static str,
    pub toast_room_removed: &'static str,
    pub log_disconnected: &'static str,
    pub toast_room_disconnected: &'static str,
    pub log_reconnecting: &'static str,
    pub toast_reconnecting: &'static str,
    pub toast_connect_before_activate: &'static str,
    pub err_need_admin_routes: &'static str,
    pub log_routes_installed: &'static str,
    pub toast_discovery_active: &'static str,
    pub phase_starting: &'static str,
    pub phase_searching: &'static str,
    pub phase_connecting: &'static str,
    pub log_connected_relay: &'static str,
    pub log_connected_direct: &'static str,
    pub log_error_prefix: &'static str,
    pub log_session_ended: &'static str,

    pub state_starting: &'static str,
    pub state_searching: &'static str,
    pub state_connecting: &'static str,
    pub state_relay: &'static str,
    pub state_direct: &'static str,
    pub state_off: &'static str,
    pub state_error: &'static str,
    pub status_no_network: &'static str,
    pub toggle_disconnect: &'static str,
    pub toggle_cancel: &'static str,
    pub toggle_connect: &'static str,
    pub rooms_summary: &'static str,

    pub menu_help: &'static str,
    pub menu_settings: &'static str,
    pub menu_about: &'static str,
    pub menu_quit: &'static str,
    pub menu_network: &'static str,
    pub menu_create: &'static str,
    pub menu_join: &'static str,
    pub tooltip_network_menu: &'static str,
    pub badge_limited: &'static str,

    pub context_disable: &'static str,
    pub context_leave: &'static str,
    pub participants: &'static str,
    pub waiting_participants: &'static str,
    pub legend_direct: &'static str,
    pub legend_relay: &'static str,
    pub legend_offline: &'static str,
    pub no_networks: &'static str,
    pub no_networks_hint: &'static str,
    pub you: &'static str,
    pub ping_measuring: &'static str,
    pub copy_ip: &'static str,
    pub toast_ip_copied: &'static str,

    pub create_title: &'static str,
    pub create_kicker: &'static str,
    pub create_hint: &'static str,
    pub create_action: &'static str,
    pub join_title: &'static str,
    pub join_kicker: &'static str,
    pub join_hint: &'static str,
    pub join_action: &'static str,
    pub field_room_name: &'static str,
    pub hint_room_name: &'static str,
    pub field_password: &'static str,
    pub hint_password: &'static str,
    pub show_password: &'static str,

    pub settings_title: &'static str,
    pub section_identity: &'static str,
    pub identity_hint: &'static str,
    pub not_generated: &'static str,
    pub reload: &'static str,
    pub copy: &'static str,
    pub copied: &'static str,
    pub section_privileges: &'static str,
    pub administrator: &'static str,
    pub limited_mode: &'static str,
    pub limited_hint: &'static str,
    #[cfg_attr(not(unix), allow(dead_code))]
    pub reopen_admin: &'static str,
    pub section_language: &'static str,
    pub language_hint: &'static str,
    pub version_label: &'static str,

    pub about_title: &'static str,
    pub about_tagline: &'static str,
    pub about_stack: &'static str,
    pub about_platform: &'static str,
    pub close: &'static str,

    pub tray_open: &'static str,
    pub tray_quit: &'static str,
    pub tray_tooltip: &'static str,

    pub backend_already_connected: &'static str,
    pub backend_empty_password: &'static str,
    pub backend_loading_identity: &'static str,
    pub backend_deriving_room: &'static str,
    pub backend_port_busy: &'static str,
    pub backend_rendezvous_client: &'static str,
    pub backend_joining_room: &'static str,
    pub backend_resolving_relay: &'static str,
    pub backend_preparing_tap: &'static str,
}

pub static EN: Strings = Strings {
    user_fallback: "user",
    language_changed: "Language changed.",

    err_enter_room_name: "Enter the network name.",
    err_enter_password: "Enter the network password.",
    err_room_exists: "That network is already in the list.",
    log_joining: "Joining the network...",
    toast_room_added: "Network added.",
    toast_room_removed: "Network {name} removed.",
    log_disconnected: "Disconnected.",
    toast_room_disconnected: "Network {name} disconnected.",
    log_reconnecting: "Reconnecting...",
    toast_reconnecting: "Reconnecting.",
    toast_connect_before_activate: "Connect the network before activating it for games.",
    err_need_admin_routes: "Run nuvia as administrator to install the game routes.",
    log_routes_installed: "Discovery routes installed on the shared TAP.",
    toast_discovery_active: "Game discovery active on the shared TAP.",
    phase_starting: "Preparing identity and socket.",
    phase_searching: "Joining the room.",
    phase_connecting: "Preparing the TAP and relay.",
    log_connected_relay: "Connected via relay.",
    log_connected_direct: "Connected directly.",
    log_error_prefix: "ERROR",
    log_session_ended: "Session ended.",

    state_starting: "STARTING",
    state_searching: "SEARCHING",
    state_connecting: "CONNECTING",
    state_relay: "RELAY",
    state_direct: "DIRECT",
    state_off: "OFF",
    state_error: "ERROR",
    status_no_network: "NO NETWORK",
    toggle_disconnect: "Disconnect this network",
    toggle_cancel: "Cancel the connection",
    toggle_connect: "Connect this network",
    rooms_summary: "{rooms} room(s) · {connected} connected",

    menu_help: "HELP",
    menu_settings: "SETTINGS",
    menu_about: "ABOUT",
    menu_quit: "QUIT",
    menu_network: "NETWORK",
    menu_create: "CREATE NETWORK",
    menu_join: "JOIN NETWORK",
    tooltip_network_menu: "Create or join a network",
    badge_limited: "LIMITED MODE",

    context_disable: "DISABLE",
    context_leave: "LEAVE",
    participants: "PARTICIPANTS",
    waiting_participants: "Waiting for other participants...",
    legend_direct: "direct",
    legend_relay: "relay",
    legend_offline: "offline",
    no_networks: "NO NETWORKS",
    no_networks_hint: "Use the NETWORK menu at the top to create or join. Connected networks share the same local TAP/IP.",
    you: "you",
    ping_measuring: "measuring...",
    copy_ip: "COPY IP",
    toast_ip_copied: "IP copied.",

    create_title: "Create network",
    create_kicker: "CREATE NETWORK",
    create_hint: "Create the network on this PC and use the same name and password on the other to join.",
    create_action: "CREATE NETWORK",
    join_title: "Join network",
    join_kicker: "JOIN NETWORK",
    join_hint: "Use the agreed name and password. If the network does not exist yet, it is created now.",
    join_action: "JOIN",
    field_room_name: "NETWORK NAME",
    hint_room_name: "e.g. home",
    field_password: "PASSWORD",
    hint_password: "password shared between PCs",
    show_password: "Show password",

    settings_title: "Settings",
    section_identity: "IDENTITY",
    identity_hint: "X25519 key used in every network.",
    not_generated: "not generated",
    reload: "RELOAD",
    copy: "COPY",
    copied: "Key copied.",
    section_privileges: "PRIVILEGES",
    administrator: "ADMINISTRATOR",
    limited_mode: "LIMITED MODE",
    limited_hint: "Without root/admin the TAP and game routes are not created.",
    reopen_admin: "REOPEN AS ADMIN",
    section_language: "LANGUAGE",
    language_hint: "Interface language.",
    version_label: "version",

    about_title: "About",
    about_tagline: "Encrypted virtual LAN for games.",
    about_stack: "Layer 2 · Noise XX · ChaCha20-Poly1305",
    about_platform: "Rust + egui · Linux and Windows",
    close: "CLOSE",

    tray_open: "Open nuvia",
    tray_quit: "Quit",
    tray_tooltip: "nuvia VPN",

    backend_already_connected: "network {name} is already connected",
    backend_empty_password: "the network password is empty",
    backend_loading_identity: "loading identity {path}",
    backend_deriving_room: "deriving the room",
    backend_port_busy: "UDP port {port} is busy",
    backend_rendezvous_client: "creating rendezvous client",
    backend_joining_room: "joining the room",
    backend_resolving_relay: "resolving relay {server}",
    backend_preparing_tap: "preparing the shared TAP",
};

pub static ES: Strings = Strings {
    user_fallback: "usuario",
    language_changed: "Idioma cambiado.",

    err_enter_room_name: "Ingresa el nombre de la red.",
    err_enter_password: "Ingresa la contraseña de la red.",
    err_room_exists: "Esa red ya está en la lista.",
    log_joining: "Entrando en la red...",
    toast_room_added: "Red agregada.",
    toast_room_removed: "Red {name} eliminada.",
    log_disconnected: "Desconectado.",
    toast_room_disconnected: "Red {name} desconectada.",
    log_reconnecting: "Reconectando...",
    toast_reconnecting: "Reconectando.",
    toast_connect_before_activate: "Conecta la red antes de activarla para juegos.",
    err_need_admin_routes: "Ejecuta nuvia como administrador para instalar las rutas de juegos.",
    log_routes_installed: "Rutas de descubrimiento instaladas en el TAP compartido.",
    toast_discovery_active: "Descubrimiento de juegos activo en el TAP compartido.",
    phase_starting: "Preparando identidad y socket.",
    phase_searching: "Entrando en la sala.",
    phase_connecting: "Preparando el TAP y el relay.",
    log_connected_relay: "Conectado vía relay.",
    log_connected_direct: "Conectado directamente.",
    log_error_prefix: "ERROR",
    log_session_ended: "Sesión finalizada.",

    state_starting: "INICIANDO",
    state_searching: "BUSCANDO",
    state_connecting: "CONECTANDO",
    state_relay: "RELAY",
    state_direct: "DIRECTO",
    state_off: "OFF",
    state_error: "ERROR",
    status_no_network: "SIN RED",
    toggle_disconnect: "Desconectar esta red",
    toggle_cancel: "Cancelar la conexión",
    toggle_connect: "Conectar esta red",
    rooms_summary: "{rooms} red(es) · {connected} conectada(s)",

    menu_help: "AYUDA",
    menu_settings: "AJUSTES",
    menu_about: "ACERCA DE",
    menu_quit: "SALIR",
    menu_network: "RED",
    menu_create: "CREAR RED",
    menu_join: "UNIRSE A RED",
    tooltip_network_menu: "Crear o unirse a una red",
    badge_limited: "MODO LIMITADO",

    context_disable: "DESACTIVAR",
    context_leave: "SALIR",
    participants: "PARTICIPANTES",
    waiting_participants: "Esperando a otros participantes...",
    legend_direct: "directo",
    legend_relay: "relay",
    legend_offline: "sin conexión",
    no_networks: "SIN REDES",
    no_networks_hint: "Usa el menú RED de arriba para crear o unirte. Las redes conectadas comparten el mismo TAP/IP local.",
    you: "tú",
    ping_measuring: "midiendo...",
    copy_ip: "COPIAR IP",
    toast_ip_copied: "IP copiada.",

    create_title: "Crear red",
    create_kicker: "CREAR RED",
    create_hint: "Crea la red en este PC y usa el mismo nombre y contraseña en el otro para unirte.",
    create_action: "CREAR RED",
    join_title: "Unirse a una red",
    join_kicker: "UNIRSE A RED",
    join_hint: "Usa el nombre y la contraseña acordados. Si la red aún no existe, se crea ahora.",
    join_action: "UNIRSE",
    field_room_name: "NOMBRE DE LA RED",
    hint_room_name: "ej.: casa",
    field_password: "CONTRASEÑA",
    hint_password: "contraseña acordada entre los PCs",
    show_password: "Mostrar contraseña",

    settings_title: "Ajustes",
    section_identity: "IDENTIDAD",
    identity_hint: "Clave X25519 usada en todas las redes.",
    not_generated: "no generada",
    reload: "RECARGAR",
    copy: "COPIAR",
    copied: "Clave copiada.",
    section_privileges: "PERMISOS",
    administrator: "ADMINISTRADOR",
    limited_mode: "MODO LIMITADO",
    limited_hint: "Sin root/admin no se crean el TAP ni las rutas de juegos.",
    reopen_admin: "REABRIR COMO ADMIN",
    section_language: "IDIOMA",
    language_hint: "Idioma de la interfaz.",
    version_label: "versión",

    about_title: "Acerca de",
    about_tagline: "LAN virtual cifrada para juegos.",
    about_stack: "Layer 2 · Noise XX · ChaCha20-Poly1305",
    about_platform: "Rust + egui · Linux y Windows",
    close: "CERRAR",

    tray_open: "Abrir nuvia",
    tray_quit: "Salir",
    tray_tooltip: "nuvia VPN",

    backend_already_connected: "la red {name} ya está conectada",
    backend_empty_password: "la contraseña de la red está vacía",
    backend_loading_identity: "cargando identidad {path}",
    backend_deriving_room: "derivando la red",
    backend_port_busy: "puerto UDP {port} ocupado",
    backend_rendezvous_client: "creando cliente rendezvous",
    backend_joining_room: "entrando en la sala",
    backend_resolving_relay: "resolviendo relay {server}",
    backend_preparing_tap: "preparando el TAP compartido",
};

pub static PT: Strings = Strings {
    user_fallback: "usuário",
    language_changed: "Idioma alterado.",

    err_enter_room_name: "Digite o nome da rede.",
    err_enter_password: "Digite a senha da rede.",
    err_room_exists: "Essa rede já está na lista.",
    log_joining: "Entrando na rede...",
    toast_room_added: "Rede adicionada.",
    toast_room_removed: "Rede {name} removida.",
    log_disconnected: "Desconectado.",
    toast_room_disconnected: "Rede {name} desconectada.",
    log_reconnecting: "Reconectando...",
    toast_reconnecting: "Reconectando.",
    toast_connect_before_activate: "Conecte a rede antes de ativá-la para jogos.",
    err_need_admin_routes: "Execute o nuvia como administrador para instalar as rotas de jogos.",
    log_routes_installed: "Rotas de descoberta instaladas no TAP compartilhado.",
    toast_discovery_active: "Descoberta de jogos ativa no TAP compartilhado.",
    phase_starting: "Preparando identidade e socket.",
    phase_searching: "Entrando na sala.",
    phase_connecting: "Preparando o TAP e o relay.",
    log_connected_relay: "Conectado via relay.",
    log_connected_direct: "Conectado direto.",
    log_error_prefix: "ERRO",
    log_session_ended: "Sessão encerrada.",

    state_starting: "INICIANDO",
    state_searching: "BUSCANDO",
    state_connecting: "CONECTANDO",
    state_relay: "RELAY",
    state_direct: "DIRETO",
    state_off: "OFF",
    state_error: "ERRO",
    status_no_network: "SEM REDE",
    toggle_disconnect: "Desconectar esta rede",
    toggle_cancel: "Cancelar a conexão",
    toggle_connect: "Conectar esta rede",
    rooms_summary: "{rooms} rede(s) · {connected} conectada(s)",

    menu_help: "AJUDA",
    menu_settings: "AJUSTES",
    menu_about: "SOBRE",
    menu_quit: "SAIR",
    menu_network: "REDE",
    menu_create: "CRIAR REDE",
    menu_join: "ENTRAR EM REDE",
    tooltip_network_menu: "Criar ou entrar em uma rede",
    badge_limited: "MODO LIMITADO",

    context_disable: "DESATIVAR",
    context_leave: "SAIR",
    participants: "PARTICIPANTES",
    waiting_participants: "Aguardando outros participantes...",
    legend_direct: "direto",
    legend_relay: "relay",
    legend_offline: "offline",
    no_networks: "NENHUMA REDE",
    no_networks_hint: "Use o menu REDE no topo para criar ou entrar. As redes conectadas compartilham o mesmo TAP/IP local.",
    you: "você",
    ping_measuring: "medindo...",
    copy_ip: "COPIAR IP",
    toast_ip_copied: "IP copiado.",

    create_title: "Criar rede",
    create_kicker: "CRIAR REDE",
    create_hint: "Crie a rede neste PC e use o mesmo nome e senha no outro para entrar.",
    create_action: "CRIAR REDE",
    join_title: "Entrar em rede",
    join_kicker: "ENTRAR EM REDE",
    join_hint: "Use o nome e a senha combinados. Se a rede ainda não existir, ela é criada agora.",
    join_action: "ENTRAR",
    field_room_name: "NOME DA REDE",
    hint_room_name: "ex.: casa",
    field_password: "SENHA",
    hint_password: "senha combinada entre os PCs",
    show_password: "Mostrar senha",

    settings_title: "Ajustes",
    section_identity: "IDENTIDADE",
    identity_hint: "Chave X25519 usada em todas as redes.",
    not_generated: "não gerada",
    reload: "RECARREGAR",
    copy: "COPIAR",
    copied: "Chave copiada.",
    section_privileges: "PRIVILÉGIOS",
    administrator: "ADMINISTRADOR",
    limited_mode: "MODO LIMITADO",
    limited_hint: "Sem root/admin o TAP e as rotas de jogos não são criados.",
    reopen_admin: "REABRIR COMO ADMIN",
    section_language: "IDIOMA",
    language_hint: "Idioma da interface.",
    version_label: "versão",

    about_title: "Sobre",
    about_tagline: "LAN virtual criptografada para jogos.",
    about_stack: "Layer 2 · Noise XX · ChaCha20-Poly1305",
    about_platform: "Rust + egui · Linux e Windows",
    close: "FECHAR",

    tray_open: "Abrir nuvia",
    tray_quit: "Sair",
    tray_tooltip: "nuvia VPN",

    backend_already_connected: "a rede {name} já está conectada",
    backend_empty_password: "a senha da rede está vazia",
    backend_loading_identity: "carregando identidade {path}",
    backend_deriving_room: "derivando a rede",
    backend_port_busy: "porta UDP {port} ocupada",
    backend_rendezvous_client: "criando cliente rendezvous",
    backend_joining_room: "entrando na sala",
    backend_resolving_relay: "resolvendo relay {server}",
    backend_preparing_tap: "preparando o TAP compartilhado",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_codes() {
        assert_eq!(Lang::from_code("pt-BR"), Some(Lang::Pt));
        assert_eq!(Lang::from_code("es_ES.UTF-8"), Some(Lang::Es));
        assert_eq!(Lang::from_code("en_US"), Some(Lang::En));
        assert_eq!(Lang::from_code("C"), None);
    }

    #[test]
    fn renders_named_placeholders() {
        let t = Lang::En.strings();
        assert_eq!(
            render(t.toast_room_removed, &[("name", "home")]),
            "Network home removed."
        );
        assert_eq!(
            render(t.rooms_summary, &[("rooms", "2"), ("connected", "1")]),
            "2 room(s) · 1 connected"
        );
    }
}
