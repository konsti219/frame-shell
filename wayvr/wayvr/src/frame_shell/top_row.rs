//! The keyboard's top row: one group per PC and the headset's own clients

use super::*;

// ---- Keyboard top row: one group per PC, and the headset's own clients ----

/// An application running on a PC or on the headset, for the keyboard's top row
pub struct RunningClient {
    pub name: String,
    /// Steam app id: its artwork, matching pins and stopping it
    pub app_id: Option<String>,
    /// Button action making it the application shown in VR
    pub show: String,
    pub client_id: u32,
    pub selected: bool,
    pub overlay: bool,
    pub presentation_mode: u32,
}

/// A PC in the keyboard's top row, identified by its server cookie and connection source.
pub struct TopRowHost {
    pub name: String,
    pub cookie: String,
    pub source: Option<u32>,
    pub presenting: bool,
    pub clients: Vec<RunningClient>,
    pub status: String,
}
impl TopRowHost {
    pub fn owns_screen(&self, name: &str) -> bool {
        crate::frame_hosts::owner(name).is_some_and(|pc| pc.id == self.cookie)
    }
}
pub fn top_row_hosts() -> Vec<TopRowHost> {
    let background = crate::frame_hosts::background_source();
    crate::frame_hosts::pcs()
        .into_iter()
        .map(|pc| {
            let presenting = pc.source.is_some() && pc.source == background;
            let mut status = pc.status(presenting);
            if pc
                .source
                .is_some_and(|s| !crate::frame_host::clients_known(s))
            {
                status.push_str(" · Runtime state unavailable");
            }
            TopRowHost {
                name: pc.name,
                cookie: pc.id,
                source: pc.source,
                presenting,
                clients: pc.source.map_or_else(Vec::new, pc_clients),
                status,
            }
        })
        .collect()
}

/// A runtime's clients as running applications; `source` is the PC's, None for the headset
pub(crate) fn running_clients(
    source: Option<u32>,
    clients: Vec<wayvr_ipc::frame::RuntimeClient>,
) -> Vec<RunningClient> {
    let target = source.map_or_else(|| "native".to_owned(), |s| s.to_string());
    clients
        .into_iter()
        .map(|client| RunningClient {
            name: client.name,
            app_id: client.steam_app_id,
            show: format!("::RuntimeFocus {target} {}", client.id),
            client_id: client.id,
            overlay: client.overlay,
            presentation_mode: client.presentation_mode,
            selected: client.visible
                && (client.overlay || client.primary)
                && source.is_none_or(|source| {
                    (crate::frame_hosts::background_source() == Some(source)
                        || (client.overlay && crate::frame_host::composition_supported(source)))
                        && REMOTE_LAYER_VISIBLE.load(Ordering::Relaxed)
                }),
        })
        .collect()
}

pub(crate) fn pc_clients(source: u32) -> Vec<RunningClient> {
    running_clients(Some(source), crate::frame_host::clients(source))
}

/// The headset's runtime, for its own clients
pub(crate) static NATIVE_RUNTIME: Mutex<Option<wayvr_ipc::runtime::Runtime>> = Mutex::new(None);

pub(crate) fn native_runtime<T>(
    f: impl FnOnce(&wayvr_ipc::runtime::Runtime) -> anyhow::Result<T>,
) -> Option<T> {
    let mut runtime = NATIVE_RUNTIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if runtime.is_none() {
        *runtime = wayvr_ipc::runtime::Runtime::connect().ok();
    }
    let result = f(runtime.as_ref()?);
    if result.is_err() {
        *runtime = None;
    }
    result.ok()
}

/// The headset's own clients, not the shell's (WayVR and the WiVRn client run in this process)
pub fn native_clients(_app: &AppState) -> Vec<RunningClient> {
    let own = std::process::id() as i32;
    let clients = native_runtime(|runtime| runtime.clients()).unwrap_or_default();
    REMOTE_LAYER_VISIBLE.store(
        clients
            .iter()
            .any(|c| c.pid == own && c.name == "WiVRn" && c.visible),
        Ordering::Relaxed,
    );
    let clients = clients
        .into_iter()
        .filter(|client| client.pid != own)
        .collect();
    running_clients(None, clients)
}

/// What runs on the headset (not the shell itself) and on each PC, for `wayvrctl apps`
pub fn app_list_json() -> String {
    let own = std::process::id() as i32;
    let native: Vec<_> = native_runtime(|runtime| runtime.clients())
        .unwrap_or_default()
        .into_iter()
        .filter(|client| client.pid != own)
        .collect();
    let pcs: Vec<_> = crate::frame_hosts::pcs()
        .into_iter()
        .map(|pc| {
            let source = pc.source;
            serde_json::json!({
                "name": pc.name,
                "id": pc.id,
                "connected": source.is_some_and(crate::frame_host::is_connected),
                "clients_known": source.is_some_and(crate::frame_host::clients_known),
                "steam_running": source.and_then(crate::frame_host::games).unwrap_or_default(),
                "clients": source.map(crate::frame_host::clients).unwrap_or_default(),
            })
        })
        .collect();
    serde_json::json!({ "native": native, "pcs": pcs }).to_string()
}

/// Shows a runtime client: a PC's (presenting that PC first) or the headset's own (None)
pub fn focus_client(source: Option<u32>, client: u32) {
    control_client(source, client, wayvr_ipc::frame::ClientAction::Select);
}

pub fn control_client(source: Option<u32>, client: u32, action: wayvr_ipc::frame::ClientAction) {
    let Some(source) = source else {
        native_runtime(|runtime| runtime.control(client, action));
        return;
    };
    if !crate::frame_host::is_connected(source) {
        crate::frame_host::error("That PC is no longer connected".into());
        return;
    }
    if !crate::frame_host::clients_known(source) {
        crate::frame_host::error("The PC's runtime state is currently unavailable".into());
        return;
    }
    let state = crate::frame_host::clients(source)
        .into_iter()
        .find(|c| c.id == client);
    let action = if matches!(action, wayvr_ipc::frame::ClientAction::Select)
        && state.as_ref().is_some_and(|c| !c.overlay)
        && !(state.as_ref().is_some_and(|c| c.primary && c.visible)
            && crate::frame_hosts::background_source() == Some(source)
            && REMOTE_LAYER_VISIBLE.load(Ordering::Relaxed))
    {
        wayvr_ipc::frame::ClientAction::Show
    } else {
        action
    };
    let is_default = matches!(
        action,
        wayvr_ipc::frame::ClientAction::Default | wayvr_ipc::frame::ClientAction::Show
    );
    if is_default
        && crate::frame_hosts::background_source() != Some(source)
        && let Some(pc) = crate::frame_hosts::for_source(source)
    {
        send_command(&format!("present {}", pc.id));
    }
    if is_default {
        let own = std::process::id() as i32;
        native_runtime(|runtime| {
            if let Some(stream) = runtime
                .clients()?
                .into_iter()
                .find(|c| c.pid == own && c.name == "WiVRn")
            {
                runtime.control(stream.id, wayvr_ipc::frame::ClientAction::Default)?;
            }
            Ok(())
        });
    }
    let command = if matches!(action, wayvr_ipc::frame::ClientAction::Show)
        && !crate::frame_host::composition_supported(source)
    {
        wayvr_ipc::frame::ShellMessage::FocusClient(client)
    } else {
        wayvr_ipc::frame::ShellMessage::ClientAction { client, action }
    };
    crate::frame_host::send(source, &command);
}

/// Changes when what the keyboard's top row shows of PCs and clients does
pub fn top_row_signature(app: &AppState) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for host in top_row_hosts() {
        (
            &host.name,
            &host.cookie,
            host.source,
            host.presenting,
            &host.status,
        )
            .hash(&mut hasher);
        for client in &host.clients {
            (
                &client.name,
                &client.app_id,
                &client.show,
                client.selected,
                client.overlay,
                client.presentation_mode,
            )
                .hash(&mut hasher);
        }
    }
    for client in native_clients(app) {
        (
            &client.name,
            &client.app_id,
            &client.show,
            client.selected,
            client.overlay,
            client.presentation_mode,
        )
            .hash(&mut hasher);
    }
    hasher.finish()
}

/// Stops a Steam game on a PC (by source number) or on the headset (None)
pub fn stop_game(source: Option<u32>, app_id: &str, force: bool) {
    match source {
        Some(source) => {
            crate::frame_host::send(
                source,
                &wayvr_ipc::frame::ShellMessage::StopGame {
                    app_id: app_id.to_owned(),
                    force,
                },
            );
        }
        None => {
            if let Err(error) = wlx_common::steam::stop(app_id, force) {
                log::warn!("Could not stop {app_id}: {error:?}");
            }
        }
    }
}
