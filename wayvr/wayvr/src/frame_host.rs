//! The connected PCs' host services (the frame shell's desktop/, linked into WiVRn's server), reached through the
//! WiVRn client as wayvr_ipc::frame messages, one source number per PC. Received state waits here for the shell's
//! frame thread.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};
use wayvr_ipc::{
    frame::{
        DesktopSettings, HostMessage, Notification, RuntimeClient, SessionFrame, ShellMessage,
    },
    packet_client::PacketClient,
};

/// A wayvr-ipc client on a PC (source), tunnelled to the IPC server
pub enum IpcEvent {
    Open(u32, u32),
    Packet(u32, u32, PacketClient),
    Close(u32, u32),
    /// The PC went away, and its clients with it
    Disconnected(u32),
}

/// One connected PC
struct Pc {
    games: Option<(Instant, Vec<String>)>,
    /// Its runtime's clients
    clients: Vec<RuntimeClient>,
    clients_known: bool,
    composition_supported: bool,
    sent_background: Option<bool>,
    sent_settings: Option<DesktopSettings>,
}

struct State {
    pcs: BTreeMap<u32, Pc>,
    /// What the PCs should encode their monitors with, from the headset's settings
    desktop_settings: Option<DesktopSettings>,
    /// The VR PC's stats and runtime frame timings, for the watch
    frames: VecDeque<SessionFrame>,
    errors: VecDeque<String>,
    notifications: VecDeque<Notification>,
    ipc: VecDeque<IpcEvent>,
}

static STATE: Mutex<State> = Mutex::new(State {
    pcs: BTreeMap::new(),
    desktop_settings: None,
    frames: VecDeque::new(),
    errors: VecDeque::new(),
    notifications: VecDeque::new(),
    ipc: VecDeque::new(),
});

/// Host state older than this is gone with its PC
const FRESH: Duration = Duration::from_secs(3);

fn state() -> MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// False without that PC connected
pub fn send(source: u32, message: &ShellMessage) -> bool {
    state().pcs.contains_key(&source)
        && crate::frame_shell::send_host_message(source, &message.encode())
}

pub fn is_connected(source: u32) -> bool {
    state().pcs.contains_key(&source)
}

/// The connected PCs, and whether each streams VR
pub fn sources() -> Vec<(u32, bool)> {
    let background = crate::frame_hosts::background_source();
    state()
        .pcs
        .keys()
        .map(|&source| (source, Some(source) == background))
        .collect()
}

pub fn vr_source() -> Option<u32> {
    crate::frame_hosts::background_source().filter(|&source| is_connected(source))
}

pub fn background_changed() {
    state().frames.clear();
}

pub fn connected(source: u32, _background: bool) {
    let mut state = state();
    if state.pcs.contains_key(&source) {
        return;
    }
    state.pcs.insert(
        source,
        Pc {
            games: None,
            clients: Vec::new(),
            clients_known: false,
            composition_supported: false,
            sent_background: None,
            sent_settings: None,
        },
    );
    drop(state);
    crate::frame_shell::send_host_message(source, &ShellMessage::RequestState.encode());
}

/// A message from a PC
pub fn receive(source: u32, data: &[u8]) {
    let message = match HostMessage::decode(data) {
        Ok(message) => message,
        Err(error) => {
            log::warn!("Invalid host message: {error:#}");
            return;
        }
    };
    let mut guard = state();
    let state = &mut *guard;
    let Some(pc) = state.pcs.get_mut(&source) else {
        return;
    };
    let vr = crate::frame_hosts::background_source() == Some(source);
    match message {
        HostMessage::Games(games) => {
            let changed = pc.games.as_ref().is_none_or(|(_, old)| *old != games);
            pc.games = Some((Instant::now(), games));
            if changed {
                crate::frame_shell::bump_lobby_state();
            }
        }
        HostMessage::Identity(identity) => {
            crate::frame_hosts::identity(source, identity);
            crate::frame_shell::bump_lobby_state();
        }
        HostMessage::CompositionSupported => {
            pc.composition_supported = true;
            crate::frame_shell::runtime_clients(source, &pc.clients, pc.clients_known, true);
        }
        HostMessage::RuntimeUnavailable => {
            if pc.clients_known {
                log::warn!("PC connection {source}: runtime state unavailable");
            }
            pc.clients_known = false;
            crate::frame_shell::runtime_clients(source, &[], false, pc.composition_supported);
            crate::frame_shell::bump_lobby_state();
        }
        HostMessage::Clients(clients) => {
            if !pc.clients_known {
                log::info!(
                    "PC connection {source}: runtime state available ({} XR clients)",
                    clients.len()
                );
            }
            crate::frame_shell::runtime_clients(source, &clients, true, pc.composition_supported);
            if !pc.clients_known || pc.clients != clients {
                pc.clients_known = true;
                pc.clients = clients;
                crate::frame_shell::bump_lobby_state();
            }
        }
        HostMessage::Frames(frames) => {
            if vr {
                state.frames.extend(frames);
                let excess = state.frames.len().saturating_sub(256);
                state.frames.drain(..excess);
            }
        }
        HostMessage::Error(error) => {
            if state.errors.len() < 8 {
                state.errors.push_back(error);
            }
        }
        HostMessage::Notification(notification) => {
            if state.notifications.len() < 16 {
                state.notifications.push_back(notification);
            }
        }
        HostMessage::IpcOpen(id) => state.ipc.push_back(IpcEvent::Open(source, id)),
        HostMessage::Ipc(id, packet) => {
            if state.ipc.len() < 1024 {
                state.ipc.push_back(IpcEvent::Packet(source, id, packet));
            }
        }
        HostMessage::IpcClose(id) => state.ipc.push_back(IpcEvent::Close(source, id)),
    }
    drop(guard);
    sync_presentation();
}

pub fn composition_supported(source: u32) -> bool {
    state()
        .pcs
        .get(&source)
        .is_some_and(|pc| pc.composition_supported)
}

pub fn sync_presentation() {
    let background = crate::frame_hosts::background_source();
    let mut state = state();
    for (&source, pc) in &mut state.pcs {
        let visible = background == Some(source);
        if pc.composition_supported
            && pc.sent_background != Some(visible)
            && crate::frame_shell::send_host_message(
                source,
                &ShellMessage::BackgroundVisible(visible).encode(),
            )
        {
            pc.sent_background = Some(visible);
        }
    }
}

/// A PC's connection ended
pub fn disconnected(source: u32) {
    let was_background = crate::frame_hosts::background_source() == Some(source);
    crate::frame_hosts::disconnected(source);
    let mut state = state();
    let Some(_pc) = state.pcs.remove(&source) else {
        return;
    };
    if was_background {
        state.frames.clear();
    }
    state.ipc.retain(|event| match event {
        IpcEvent::Open(from, _) | IpcEvent::Packet(from, _, _) | IpcEvent::Close(from, _) => {
            *from != source
        }
        IpcEvent::Disconnected(_) => true,
    });
    state.ipc.push_back(IpcEvent::Disconnected(source));
}

/// Sends the desktop stream settings to each PC whose differ
pub fn sync_desktop_settings(settings: DesktopSettings) {
    state().desktop_settings = Some(settings);
    resync_desktop_settings();
}

/// Sends the last desktop stream settings to PCs that don't have them, e.g. a newly connected one
pub fn resync_desktop_settings() {
    let Some(settings) = state().desktop_settings else {
        return;
    };
    let sources: Vec<u32> = {
        let state = state();
        state
            .pcs
            .iter()
            .filter(|(_, pc)| pc.sent_settings != Some(settings))
            .map(|(&source, _)| source)
            .collect()
    };
    for source in sources {
        if send(source, &ShellMessage::DesktopSettings(settings))
            && let Some(pc) = state().pcs.get_mut(&source)
        {
            pc.sent_settings = Some(settings);
        }
    }
}

/// Steam app ids of a PC's running games
pub fn games(source: u32) -> Option<Vec<String>> {
    state()
        .pcs
        .get(&source)
        .and_then(|pc| pc.games.as_ref())
        .filter(|(updated, _)| updated.elapsed() < FRESH)
        .map(|(_, games)| games.clone())
}

/// A PC runtime's clients
pub fn clients(source: u32) -> Vec<RuntimeClient> {
    state()
        .pcs
        .get(&source)
        .filter(|pc| pc.clients_known)
        .map(|pc| pc.clients.clone())
        .unwrap_or_default()
}

pub fn error(message: String) {
    state().errors.push_back(message);
}

pub fn clients_known(source: u32) -> bool {
    state().pcs.get(&source).is_some_and(|pc| pc.clients_known)
}

/// Whether a games list went stale since the last call
pub fn games_expired() -> bool {
    let mut expired = false;
    for pc in state().pcs.values_mut() {
        if pc
            .games
            .as_ref()
            .is_some_and(|(updated, _)| updated.elapsed() >= FRESH)
        {
            pc.games = None;
            expired = true;
        }
    }
    expired
}

pub fn take_frames() -> Vec<SessionFrame> {
    state().frames.drain(..).collect()
}

pub fn take_errors() -> Vec<String> {
    state().errors.drain(..).collect()
}

pub fn take_notifications() -> Vec<Notification> {
    state().notifications.drain(..).collect()
}

pub fn take_ipc_events() -> Vec<IpcEvent> {
    state().ipc.drain(..).collect()
}

#[cfg(feature = "feat-monado-metrics")]
impl From<SessionFrame> for crate::subsystem::monado_metrics::proto::SessionFrame {
    fn from(f: SessionFrame) -> Self {
        Self {
            session_id: f.session_id,
            frame_id: f.frame_id,
            predicted_frame_time_ns: f.predicted_frame_time_ns,
            predicted_wake_up_time_ns: f.predicted_wake_up_time_ns,
            predicted_gpu_done_time_ns: f.predicted_gpu_done_time_ns,
            predicted_display_time_ns: f.predicted_display_time_ns,
            predicted_display_period_ns: f.predicted_display_period_ns,
            display_time_ns: f.display_time_ns,
            when_predicted_ns: f.when_predicted_ns,
            when_wait_woke_ns: f.when_wait_woke_ns,
            when_begin_ns: f.when_begin_ns,
            when_delivered_ns: f.when_delivered_ns,
            when_gpu_done_ns: f.when_gpu_done_ns,
            discarded: f.discarded,
            when_submit_begin_ns: f.when_submit_begin_ns,
            when_submit_end_ns: f.when_submit_end_ns,
        }
    }
}
