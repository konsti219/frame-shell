//! PCs for the dashboard: their state, connecting, launching apps and commands to the client

use super::*;

pub(crate) fn registry_lobby(mut lobby: serde_json::Value) -> serde_json::Value {
    let mut pcs = crate::frame_hosts::json();
    for pc in pcs.as_array_mut().unwrap() {
        if let Some(source) = pc["source"].as_u64() {
            pc["stream"] = pc_stream(source as u32);
            if !crate::frame_host::clients_known(source as u32) {
                let status = pc["status"].as_str().unwrap_or_default();
                pc["status"] = format!("{status} · Runtime state unavailable").into();
            }
        }
    }
    lobby["servers"] = pcs;
    if let Some(source) = crate::frame_hosts::background_source()
        && let Some(pc) = crate::frame_hosts::for_source(source)
    {
        lobby["server"] = pc.name.into();
        lobby["server_id"] = pc.id.into();
    }
    lobby
}
pub(crate) fn pc_stream(source: u32) -> serde_json::Value {
    let mut stream = STREAM_STATES
        .lock()
        .unwrap()
        .get(&source)
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    stream["steam_running"] = crate::frame_host::games(source).unwrap_or_default().into();
    stream["runtime_known"] = crate::frame_host::clients_known(source).into();
    stream["running"] = crate::frame_host::clients(source).into_iter().map(|client| serde_json::json!({
        "id": client.id, "name": client.name, "overlay": client.overlay, "active": client.primary, "visible": client.visible
    })).collect();
    stream
}

/// The WiVRn state for the dashboard, with the digits typed on the PIN pad added
pub fn dashboard_state() -> Option<(u64, String, String)> {
    if !EMBEDDED.load(Ordering::Relaxed) {
        return None;
    }
    let typed = PIN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let (version, lobby) = LOBBY_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let mut lobby: serde_json::Value =
        serde_json::from_str(&lobby).unwrap_or_else(|_| serde_json::json!({}));
    lobby = registry_lobby(lobby);
    lobby["pin_typed"] = typed.into();
    let stream =
        crate::frame_hosts::background_source().map_or_else(|| serde_json::json!({}), pc_stream);
    Some((version, lobby.to_string(), stream.to_string()))
}

/// Starts an application on a PC; connects first (and starts it once connected) when needed
/// Starts an application on a connected PC; one on standby then presents VR
pub(crate) fn start_app_on(source: u32, app_id: &str) {
    send_command(&format!("on {source} start_app {app_id}"));
}

pub fn launch_remote(app: &mut AppState, host: &str, app_id: &str, name: &str) {
    if let Some(source) = source_for_host(host) {
        start_app_on(source, app_id);
        let label =
            crate::frame_hosts::for_source(source).map_or_else(|| host.to_owned(), |pc| pc.name);
        toast(app, format!("Starting {name} on {label}"));
        return;
    }
    let Some(pc) = crate::frame_hosts::resolve(host) else {
        toast(app, format!("{host} is not a known PC"));
        return;
    };
    send_command(&format!("connect {}", pc.id));
    PENDING_LAUNCH
        .lock()
        .unwrap()
        .push_back((pc.id, app_id.into(), name.into()));
    toast(app, format!("Connecting to {} to start {name}", pc.name));
}

pub fn connect_screen_host(app: &mut AppState, screen: &str) {
    let Some(pc) = crate::frame_hosts::owner(screen) else {
        toast(
            app,
            "This monitor has not been associated with a PC yet".into(),
        );
        return;
    };
    // The placeholder already says what a connected or connecting PC is waiting for
    if pc.source.is_none()
        && !matches!(
            pc.connection,
            crate::frame_hosts::Connection::Connecting | crate::frame_hosts::Connection::Pairing
        )
    {
        connect_host(app, &pc.id, true);
    }
}

pub fn toggle_screen_host(app: &mut AppState, screen: &str) {
    if let Some(pc) = crate::frame_hosts::owner(screen) {
        connect_host(app, &pc.id, pc.source.is_none());
    }
}

pub fn connect_host(app: &mut AppState, key: &str, connect: bool) {
    if let Some(pc) = crate::frame_hosts::resolve(key) {
        send_command(&format!(
            "{} {}",
            if connect { "connect" } else { "disconnect" },
            pc.id
        ));
        toast(
            app,
            format!(
                "{} {}",
                if connect {
                    "Connecting to"
                } else {
                    "Disconnecting from"
                },
                pc.name
            ),
        );
    } else {
        toast(app, format!("{key} is not a known PC"));
    }
}

/// Queues a command line for the WiVRn client; PIN pad keys are collected here into one `pin` command.
pub fn send_command(command: &str) {
    let translated;
    let command = if let Some((verb, rest)) = command.split_once(' ') {
        if matches!(
            verb,
            "connect" | "disconnect" | "present" | "pin_vr" | "autoconnect" | "forget" | "setting"
        ) {
            let (key, tail) = rest.split_once(' ').unwrap_or((rest, ""));
            if matches!(verb, "disconnect" | "forget") {
                PENDING_LAUNCH
                    .lock()
                    .unwrap()
                    .retain(|(host, _, _)| host != key);
            }
            if verb == "forget" {
                crate::frame_hosts::forget(key);
            }
            if let Some(target) = crate::frame_hosts::target(key) {
                translated = format!(
                    "{verb} {target}{}{}",
                    if tail.is_empty() { "" } else { " " },
                    tail
                );
                translated.as_str()
            } else {
                command
            }
        } else {
            command
        }
    } else {
        command
    };

    let line = command;
    let mut pin = PIN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let line = if let Some(digit) = line.strip_prefix("pin_digit ") {
        pin.push_str(digit);
        if pin.len() < 6 {
            bump_lobby_state();
            return;
        }
        format!("pin {}", std::mem::take(&mut *pin))
    } else if line == "pin_back" {
        pin.pop();
        bump_lobby_state();
        return;
    } else {
        if line == "pin_cancel" {
            pin.clear();
        }
        line.to_owned()
    };
    drop(pin);
    let mut commands = COMMANDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if commands.len() < 64 {
        commands.push_back(line);
    }
}

/// Makes the PC panel redraw (the PIN display isn't part of the client's state)
pub(crate) fn bump_lobby_state() {
    LOBBY_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .0 += 1;
}

pub fn remote_game_running(host: &str, app_id: &str) -> bool {
    source_for_host(host)
        .and_then(crate::frame_host::games)
        .is_some_and(|games| games.iter().any(|game| game == app_id))
}

/// A PC's current connection, by its ID
pub(crate) fn source_for_host(host: &str) -> Option<u32> {
    crate::frame_hosts::resolve(host)?.source
}
