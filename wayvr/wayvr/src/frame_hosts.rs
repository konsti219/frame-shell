//! The PCs the client knows, keyed by their WiVRn server cookie (shared by a PC's packaged and dev servers). A
//! manual address connected to a PC shows as that PC; commands go out with the client's own key for it. Screens are
//! keyed "<cookie> <connector>", and the known ones are kept in frame-shell-screens for their placeholders.
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CStr,
    path::PathBuf,
    sync::{LazyLock, Mutex},
};
use wayvr_ipc::frame::HostIdentity;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Connection {
    #[default]
    Disconnected,
    Connecting,
    Pairing,
    Connected,
    Error,
}
impl Connection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disconnected => "idle",
            Self::Connecting => "connecting",
            Self::Pairing => "pin",
            Self::Connected => "connected",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Pc {
    /// The server cookie, or the client's key for a manual address that hasn't connected
    pub id: String,
    /// The client's key, which its commands take
    pub endpoint: String,
    pub name: String,
    pub hostname: String,
    pub version: String,
    pub source: Option<u32>,
    pub connection: Connection,
    pub available: bool,
    pub compatible: bool,
    pub autoconnect: bool,
    pub manual: bool,
    pub detail: String,
    pub background_pinned: bool,
    /// Its stream settings, as the client reports them
    pub settings: serde_json::Value,
}
impl Pc {
    /// Takes frame_host's lock: call without the registry's
    pub fn status(&self, background: bool) -> String {
        // The background PC shows nothing in VR until it runs an application
        let vr = background
            && self.source.is_some_and(|source| {
                crate::frame_host::clients(source)
                    .iter()
                    .any(|client| !client.overlay)
            });
        let status = match self.connection {
            Connection::Connected if vr => "Connected · VR background",
            Connection::Connected => "Connected",
            Connection::Connecting => "Connecting",
            Connection::Pairing => "Waiting for PIN",
            Connection::Error => "Connection failed",
            Connection::Disconnected if !self.compatible && self.available => {
                "Incompatible version"
            }
            Connection::Disconnected if self.available => "Available",
            Connection::Disconnected => "Not found",
        };
        [
            status,
            self.detail.as_str(),
            if self.background_pinned {
                "Background locked"
            } else {
                ""
            },
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
    }
}

struct Registry {
    /// The client's servers as it last reported them, keyed by its own keys
    views: Vec<Pc>,
    pcs: BTreeMap<String, Pc>,
    identities: BTreeMap<u32, HostIdentity>,
    selected_source: Option<u32>,
    screens: BTreeSet<String>,
}
fn screens_file() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join(".config"))
        .join("wayvr/frame-shell-screens")
}
static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| {
    let screens = match std::fs::read_to_string(screens_file()) {
        Ok(text) => text.lines().filter(|s| !s.is_empty()).map(String::from).collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
        Err(error) => panic!("Reading frame-shell-screens: {error}"),
    };
    Mutex::new(Registry {
        views: Vec::new(),
        pcs: BTreeMap::new(),
        identities: BTreeMap::new(),
        selected_source: None,
        screens,
    })
});
fn registry() -> std::sync::MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap()
}
impl Registry {
    /// One entry per PC: a connected route takes its server's identity and wins when a PC is reachable twice
    fn rebuild(&mut self) {
        let mut pcs = BTreeMap::<String, Pc>::new();
        for view in &self.views {
            let mut pc = view.clone();
            if let Some(identity) = pc.source.and_then(|s| self.identities.get(&s)) {
                pc.id = identity.id.clone();
                pc.name = identity.name.clone();
                pc.version = identity.version.clone();
            }
            if pcs
                .get(&pc.id)
                .is_some_and(|known| known.source.is_some() || (pc.source.is_none() && !pc.available))
            {
                continue;
            }
            pcs.insert(pc.id.clone(), pc);
        }
        self.pcs = pcs;
    }

    fn save_screens(&self) {
        let file = screens_file();
        let text: String = self.screens.iter().map(|s| format!("{s}\n")).collect();
        if let Err(error) = std::fs::create_dir_all(file.parent().unwrap())
            .and_then(|()| std::fs::write(&file, text))
        {
            log::warn!("Saving frame-shell-screens: {error}");
        }
    }
}

pub fn pcs() -> Vec<Pc> {
    registry().pcs.values().cloned().collect()
}
pub fn resolve(id: &str) -> Option<Pc> {
    registry().pcs.get(id).cloned()
}
pub fn for_source(source: u32) -> Option<Pc> {
    registry()
        .pcs
        .values()
        .find(|pc| pc.source == Some(source))
        .cloned()
}
pub fn background_source() -> Option<u32> {
    registry().selected_source
}
/// The PC a screen belongs to, by its key
pub fn owner(screen: &str) -> Option<Pc> {
    resolve(screen.split_once(' ')?.0)
}
pub fn layouts() -> Vec<String> {
    registry().screens.iter().cloned().collect()
}
pub fn monitor_label(screen: &str) -> Option<String> {
    screen.split_once(' ').map(|(_, connector)| connector.to_owned())
}
/// A screen as people know it: its PC's name and connector, the connector alone for a PC no longer known
pub fn screen_title(screen: &str) -> String {
    let connector = monitor_label(screen).unwrap_or_else(|| screen.to_owned());
    owner(screen).map_or(connector.clone(), |pc| format!("{} {connector}", pc.name))
}
/// The client's key for a PC, for commands
pub fn target(id: &str) -> Option<String> {
    resolve(id).map(|pc| pc.endpoint)
}

pub fn forget(id: &str) {
    let mut registry = registry();
    let prefix = format!("{id} ");
    registry.screens.retain(|s| !s.starts_with(&prefix));
    registry.save_screens();
}

pub fn disconnected(source: u32) {
    let mut registry = registry();
    registry.identities.remove(&source);
    if registry.selected_source == Some(source) {
        registry.selected_source = None;
    }
    for pc in registry
        .views
        .iter_mut()
        .filter(|pc| pc.source == Some(source))
    {
        pc.source = None;
        pc.connection = Connection::Disconnected;
    }
    registry.rebuild();
}

/// The connected server's own identity, which a manual address doesn't have until it connects
pub fn identity(source: u32, identity: HostIdentity) {
    let mut registry = registry();
    registry.identities.insert(source, identity);
    registry.rebuild();
}

/// Registers a connected PC's monitor and gives its screen key
pub fn monitor(source: u32, connector: &str) -> Option<String> {
    let mut registry = registry();
    let pc = registry.pcs.values().find(|pc| pc.source == Some(source))?;
    // A manual address is keyed by its server's identity, which may arrive after the monitors
    if pc.manual && !registry.identities.contains_key(&source) {
        return None;
    }
    let key = format!("{} {connector}", pc.id);
    if registry.screens.insert(key.clone()) {
        registry.save_screens();
    }
    Some(key)
}

/// The client's servers and the selected background
pub unsafe fn update(views: *const frame_abi::frame_shell_pc, count: usize, selected_source: u32) {
    let text = |p: *const std::ffi::c_char| {
        if p.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    };
    let views = if count == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(views, count) }
    };
    let mut registry = registry();
    registry.selected_source = (selected_source != 0).then_some(selected_source);
    registry.views = views
        .iter()
        .map(|view| Pc {
            id: text(view.id),
            endpoint: text(view.id),
            name: text(view.name),
            hostname: text(view.hostname),
            version: text(view.version),
            source: (view.source != 0).then_some(view.source),
            connection: match view.connection {
                frame_abi::frame_pc_connection_FRAME_PC_CONNECTING => Connection::Connecting,
                frame_abi::frame_pc_connection_FRAME_PC_PAIRING => Connection::Pairing,
                frame_abi::frame_pc_connection_FRAME_PC_CONNECTED => Connection::Connected,
                frame_abi::frame_pc_connection_FRAME_PC_ERROR => Connection::Error,
                _ => Connection::Disconnected,
            },
            available: view.available,
            compatible: view.compatible,
            autoconnect: view.autoconnect,
            manual: view.manual,
            detail: text(view.detail),
            background_pinned: view.background_pinned,
            settings: serde_json::from_str(&text(view.settings)).unwrap(),
        })
        .collect();
    registry.rebuild();
}

pub fn json() -> serde_json::Value {
    let selected = background_source();
    pcs().iter().map(|pc| serde_json::json!({
        "cookie": pc.id, "name": pc.name, "hostname": pc.hostname, "version": pc.version,
        "visible": pc.available, "compatible": pc.compatible, "autoconnect": pc.autoconnect, "manual": pc.manual,
        "source": pc.source, "state": pc.connection.as_str(), "detail": pc.detail,
        "presenting": pc.source.is_some() && pc.source == selected,
        "background_pinned": pc.background_pinned, "settings": pc.settings,
        "status": pc.status(pc.source.is_some() && pc.source == selected)
    })).collect()
}
