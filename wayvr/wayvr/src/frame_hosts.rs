//! Persistent PC identities and their current connections; names are labels, source numbers are transient.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CStr,
    path::PathBuf,
    sync::{LazyLock, Mutex},
};
use wayvr_ipc::frame::HostIdentity;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Monitor {
    pub connector: String,
    pub layout_key: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pc {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: BTreeSet<String>,
    #[serde(default)]
    pub endpoints: BTreeSet<String>,
    #[serde(default)]
    pub monitors: Vec<Monitor>,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub build: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub remembered: bool,
    #[serde(skip)]
    pub endpoint: String,
    #[serde(skip)]
    pub source: Option<u32>,
    #[serde(skip)]
    pub connection: Connection,
    #[serde(skip)]
    pub available: bool,
    #[serde(skip)]
    pub compatible: bool,
    #[serde(skip)]
    pub autoconnect: bool,
    #[serde(skip)]
    pub manual: bool,
    #[serde(skip)]
    pub detail: String,
    #[serde(skip)]
    pub background_pinned: bool,
}
impl Pc {
    pub fn status(&self, background: bool) -> String {
        let status = match self.connection {
            Connection::Connected if background => "Connected · VR background",
            Connection::Connected => "Connected · Desktop + audio",
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
            self.build.as_str(),
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

#[derive(Default)]
struct Registry {
    pcs: BTreeMap<String, Pc>,
    identities: BTreeMap<u32, HostIdentity>,
    selected_source: Option<u32>,
}
fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join(".config"))
        .join("wayvr")
}
static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| {
    let pcs: BTreeMap<String, Pc> = match std::fs::read(config_dir().join("frame-shell-hosts.json"))
    {
        Ok(data) => serde_json::from_slice(&data).expect("Invalid frame-shell-hosts.json"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(error) => panic!("Reading frame-shell-hosts.json: {error}"),
    };
    let mut registry = Registry {
        pcs,
        ..Default::default()
    };
    for name in std::fs::read_to_string(config_dir().join("frame-shell-screens"))
        .unwrap_or_default()
        .lines()
        .filter(|s| !s.is_empty())
    {
        if registry
            .pcs
            .values()
            .any(|pc| pc.monitors.iter().any(|m| m.layout_key == name))
        {
            continue;
        }
        let Some((prefix, connector)) = name.rsplit_once(' ') else {
            continue;
        };
        let id = format!("legacy:{prefix}");
        let pc = registry.pcs.entry(id.clone()).or_insert_with(|| Pc {
            id,
            name: prefix.into(),
            aliases: BTreeSet::from([prefix.into()]),
            remembered: true,
            ..Default::default()
        });
        pc.monitors.push(Monitor {
            connector: connector.into(),
            layout_key: name.into(),
        });
    }
    Mutex::new(registry)
});
fn registry() -> std::sync::MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap()
}
impl Registry {
    fn save(&self) {
        let pcs: BTreeMap<_, _> = self.pcs.iter().filter(|(_, pc)| pc.remembered).collect();
        let dir = config_dir();
        let result = std::fs::create_dir_all(&dir).and_then(|()| {
            let data = serde_json::to_vec_pretty(&pcs).unwrap();
            let temp = dir.join("frame-shell-hosts.json.tmp");
            std::fs::write(&temp, data)?;
            std::fs::rename(temp, dir.join("frame-shell-hosts.json"))
        });
        if let Err(error) = result {
            log::warn!("Saving PC identities: {error}");
        }
    }
    fn merge_legacy(&mut self) {
        let matches: Vec<_> = self
            .pcs
            .values()
            .filter(|pc| pc.id.starts_with("legacy:"))
            .filter_map(|old| {
                let candidates: Vec<_> = self
                    .pcs
                    .values()
                    .filter(|pc| {
                        !pc.id.starts_with("legacy:") && !pc.aliases.is_disjoint(&old.aliases)
                    })
                    .collect();
                (candidates.len() == 1).then(|| (old.id.clone(), candidates[0].id.clone()))
            })
            .collect();
        for (old, id) in matches {
            let old = self.pcs.remove(&old).unwrap();
            let pc = self.pcs.get_mut(&id).unwrap();
            for monitor in old.monitors {
                if !pc.monitors.iter().any(|m| m.connector == monitor.connector) {
                    pc.monitors.push(monitor);
                }
            }
            pc.aliases.extend(old.aliases);
            pc.remembered = true;
        }
    }
    fn resolve(&self, key: &str) -> Option<&Pc> {
        if let Some(pc) = self.pcs.get(key) {
            return Some(pc);
        }
        let mut candidates = self
            .pcs
            .values()
            .filter(|pc| pc.endpoints.contains(key) || pc.name == key || pc.aliases.contains(key));
        let pc = candidates.next()?;
        candidates.next().is_none().then_some(pc)
    }
    fn for_source(&self, source: u32) -> Option<&Pc> {
        self.pcs.values().find(|pc| pc.source == Some(source))
    }
}

pub fn pcs() -> Vec<Pc> {
    registry().pcs.values().cloned().collect()
}
pub fn resolve(key: &str) -> Option<Pc> {
    registry().resolve(key).cloned()
}
pub fn for_source(source: u32) -> Option<Pc> {
    registry().for_source(source).cloned()
}
pub fn background_source() -> Option<u32> {
    registry().selected_source
}
pub fn owner(layout_key: &str) -> Option<Pc> {
    registry()
        .pcs
        .values()
        .find(|pc| pc.monitors.iter().any(|m| m.layout_key == layout_key))
        .cloned()
}
pub fn layouts() -> Vec<String> {
    registry()
        .pcs
        .values()
        .flat_map(|pc| pc.monitors.iter().map(|m| m.layout_key.clone()))
        .collect()
}
pub fn monitor_label(layout_key: &str) -> Option<String> {
    registry()
        .pcs
        .values()
        .flat_map(|pc| &pc.monitors)
        .find(|m| m.layout_key == layout_key)
        .map(|m| m.connector.clone())
}
pub fn target(key: &str) -> Option<String> {
    resolve(key).map(|pc| {
        if pc.endpoint.is_empty() {
            pc.endpoints.first().cloned().unwrap_or(pc.id)
        } else {
            pc.endpoint
        }
    })
}

pub fn forget(key: &str) {
    let mut registry = registry();
    let Some(pc) = registry.resolve(key).cloned() else {
        return;
    };
    registry.pcs.remove(&pc.id);
    let path = config_dir().join("frame-shell-screens");
    if let Ok(legacy) = std::fs::read_to_string(&path) {
        let keep = legacy
            .lines()
            .filter(|name| !pc.monitors.iter().any(|m| m.layout_key == *name))
            .collect::<Vec<_>>()
            .join("\n");
        if let Err(error) = std::fs::write(path, keep) {
            log::warn!("Forgetting legacy screen ownership: {error}");
        }
    }
    registry.save();
}

pub fn disconnected(source: u32) {
    let mut registry = registry();
    registry.identities.remove(&source);
    if registry.selected_source == Some(source) {
        registry.selected_source = None;
    }
    for pc in registry
        .pcs
        .values_mut()
        .filter(|pc| pc.source == Some(source))
    {
        pc.source = None;
        pc.connection = Connection::Disconnected;
    }
}

pub fn identity(source: u32, identity: HostIdentity) {
    let mut registry = registry();
    if let Some(old) = registry.for_source(source).cloned() {
        let mut pc = registry.pcs.remove(&old.id).unwrap();
        if let Some(known) = registry.pcs.remove(&identity.id) {
            let provisional = std::mem::replace(&mut pc.monitors, known.monitors);
            for monitor in provisional {
                if !pc.monitors.iter().any(|m| m.connector == monitor.connector) {
                    pc.monitors.push(monitor);
                }
            }
            pc.aliases.extend(known.aliases);
            pc.endpoints.extend(known.endpoints);
        }
        pc.aliases.insert(pc.name.clone());
        pc.id = identity.id.clone();
        pc.name = identity.name.clone();
        pc.build = identity.build.clone();
        pc.version = identity.version.clone();
        pc.remembered = true;
        registry.pcs.insert(pc.id.clone(), pc);
        registry.merge_legacy();
        registry.save();
    }
    registry.identities.insert(source, identity);
}

pub fn monitor(source: u32, connector: &str, legacy_name: &str) -> Option<String> {
    let mut registry = registry();
    let id = registry.for_source(source)?.id.clone();
    let hostname = legacy_name
        .rsplit_once(' ')
        .map(|(host, _)| host)
        .unwrap_or_default();
    if !hostname.is_empty() {
        registry
            .pcs
            .get_mut(&id)
            .unwrap()
            .aliases
            .insert(hostname.into());
    }
    registry.merge_legacy();
    let pc = registry.pcs.get_mut(&id)?;
    if let Some(monitor) = pc.monitors.iter().find(|m| m.connector == connector) {
        let key = monitor.layout_key.clone();
        registry.save();
        return Some(key);
    }
    let key = format!("pc:{} {}", pc.id, connector);
    pc.monitors.push(Monitor {
        connector: connector.into(),
        layout_key: key.clone(),
    });
    pc.remembered = true;
    registry.save();
    Some(key)
}

pub unsafe fn update(pcs: *const frame_abi::frame_shell_pc, count: usize, selected_source: u32) {
    let text = |p: *const std::ffi::c_char| {
        if p.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    };
    let mut registry = registry();
    let before = serde_json::to_vec(&registry.pcs).unwrap();
    registry.selected_source = (selected_source != 0).then_some(selected_source);
    for pc in registry.pcs.values_mut() {
        pc.available = false;
        pc.source = None;
        pc.connection = Connection::Disconnected;
        pc.detail.clear();
        pc.endpoint.clear();
        pc.autoconnect = false;
        pc.manual = false;
        pc.background_pinned = false;
    }
    let views = if count == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(pcs, count) }
    };
    for view in views {
        let endpoint = text(view.id);
        let identity = registry.identities.get(&view.source).cloned();
        let id = identity
            .as_ref()
            .map(|i| i.id.clone())
            .or_else(|| {
                registry
                    .pcs
                    .values()
                    .find(|pc| pc.endpoints.contains(&endpoint))
                    .map(|pc| pc.id.clone())
            })
            .unwrap_or_else(|| endpoint.clone());
        let pc = registry.pcs.entry(id.clone()).or_insert_with(|| Pc {
            id,
            ..Default::default()
        });
        let name = identity
            .as_ref()
            .map(|i| i.name.clone())
            .unwrap_or_else(|| {
                if view.manual && !pc.name.is_empty() {
                    pc.name.clone()
                } else {
                    text(view.name)
                }
            });
        if !pc.name.is_empty() {
            pc.aliases.insert(pc.name.clone());
        }
        pc.aliases.insert(name.clone());
        let choose = pc.endpoint.is_empty()
            || view.source != 0
            || (pc.source.is_none() && view.available && !pc.available);
        pc.endpoints.insert(endpoint.clone());
        pc.available |= view.available;
        pc.manual |= view.manual;
        pc.autoconnect |= view.autoconnect;
        if choose {
            pc.name = name;
            pc.hostname = text(view.hostname);
            let short = pc.hostname.split('.').next().unwrap_or_default();
            if !short.is_empty() && !short.chars().all(|c| c.is_ascii_digit()) {
                pc.aliases.insert(short.into());
            }
            pc.endpoint = endpoint;
            pc.source = (view.source != 0).then_some(view.source);
            pc.compatible = view.compatible;
            pc.connection = match view.connection {
                frame_abi::frame_pc_connection_FRAME_PC_CONNECTING => Connection::Connecting,
                frame_abi::frame_pc_connection_FRAME_PC_PAIRING => Connection::Pairing,
                frame_abi::frame_pc_connection_FRAME_PC_CONNECTED => Connection::Connected,
                frame_abi::frame_pc_connection_FRAME_PC_ERROR => Connection::Error,
                _ => Connection::Disconnected,
            };
            pc.detail = text(view.detail);
            pc.background_pinned = view.background_pinned;
            let build = identity
                .as_ref()
                .map(|i| i.build.clone())
                .unwrap_or_else(|| text(view.build));
            let version = identity
                .as_ref()
                .map(|i| i.version.clone())
                .unwrap_or_else(|| text(view.version));
            if view.source != 0 || !build.is_empty() {
                pc.build = build;
            }
            if view.source != 0 || !version.is_empty() {
                pc.version = version;
            }
        }
        pc.remembered |= pc.source.is_some() || pc.manual || pc.autoconnect;
    }
    registry.merge_legacy();
    registry
        .pcs
        .retain(|_, pc| pc.remembered || pc.available || pc.source.is_some());
    if serde_json::to_vec(&registry.pcs).unwrap() != before {
        registry.save();
    }
}

pub fn json() -> serde_json::Value {
    let registry = registry();
    registry.pcs.values().map(|pc| serde_json::json!({
        "cookie": pc.id, "name": pc.name, "hostname": pc.hostname, "build": pc.build, "version": pc.version,
        "visible": pc.available, "compatible": pc.compatible, "autoconnect": pc.autoconnect, "manual": pc.manual,
        "source": pc.source, "state": pc.connection.as_str(), "detail": pc.detail,
        "presenting": pc.source.is_some() && pc.source == registry.selected_source,
        "background_pinned": pc.background_pinned,
        "status": pc.status(pc.source.is_some() && pc.source == registry.selected_source)
    })).collect()
}
