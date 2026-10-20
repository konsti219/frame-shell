//! The connected PC's host services (the frame shell's desktop/, linked into WiVRn's server), reached through the
//! WiVRn client as wayvr_ipc::frame messages. Received state waits here for the shell's frame thread.

use std::{
    collections::VecDeque,
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};
use wayvr_ipc::{
    frame::{DesktopSettings, HostMessage, Notification, SessionFrame, ShellMessage, SystemStats},
    packet_client::PacketClient,
};

/// A wayvr-ipc client on the PC, tunnelled to the IPC server
pub enum IpcEvent {
    Open(u32),
    Packet(u32, PacketClient),
    Close(u32),
    /// The PC went away, and its clients with it
    Disconnected,
}

struct State {
    system: Option<(Instant, SystemStats)>,
    games: Option<(Instant, Vec<String>)>,
    frames: VecDeque<SessionFrame>,
    errors: VecDeque<String>,
    notifications: VecDeque<Notification>,
    ipc: VecDeque<IpcEvent>,
    sent_settings: Option<DesktopSettings>,
}

static STATE: Mutex<State> = Mutex::new(State {
    system: None,
    games: None,
    frames: VecDeque::new(),
    errors: VecDeque::new(),
    notifications: VecDeque::new(),
    ipc: VecDeque::new(),
    sent_settings: None,
});

/// Host state older than this is gone with its PC
const FRESH: Duration = Duration::from_secs(3);

fn state() -> MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// False without a connected PC
pub fn send(message: &ShellMessage) -> bool {
    crate::frame_shell::send_host_message(&message.encode())
}

/// A message from the PC
pub fn receive(data: &[u8]) {
    let message = match HostMessage::decode(data) {
        Ok(message) => message,
        Err(error) => {
            log::warn!("Invalid host message: {error:#}");
            return;
        }
    };
    let mut state = state();
    match message {
        HostMessage::System(stats) => state.system = Some((Instant::now(), stats)),
        HostMessage::Games(games) => {
            let changed = state.games.as_ref().is_none_or(|(_, old)| *old != games);
            state.games = Some((Instant::now(), games));
            if changed {
                crate::frame_shell::bump_lobby_state();
            }
        }
        HostMessage::Frames(frames) => {
            state.frames.extend(frames);
            let excess = state.frames.len().saturating_sub(256);
            state.frames.drain(..excess);
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
        HostMessage::IpcOpen(id) => state.ipc.push_back(IpcEvent::Open(id)),
        HostMessage::Ipc(id, packet) => {
            if state.ipc.len() < 1024 {
                state.ipc.push_back(IpcEvent::Packet(id, packet));
            }
        }
        HostMessage::IpcClose(id) => state.ipc.push_back(IpcEvent::Close(id)),
    }
}

/// The PC connection ended
pub fn disconnected() {
    let mut state = state();
    state.system = None;
    state.games = None;
    state.frames.clear();
    state.errors.clear();
    state.sent_settings = None;
    state.ipc.clear();
    state.ipc.push_back(IpcEvent::Disconnected);
}

/// Sends the desktop stream settings when they differ from what the PC has
pub fn sync_desktop_settings(settings: DesktopSettings) {
    let mut state = state();
    if state.sent_settings != Some(settings) && send(&ShellMessage::DesktopSettings(settings)) {
        state.sent_settings = Some(settings);
    }
}

pub fn system() -> Option<SystemStats> {
    state()
        .system
        .as_ref()
        .filter(|(updated, _)| updated.elapsed() < FRESH)
        .map(|(_, stats)| stats.clone())
}

/// Steam app ids of the PC's running games
pub fn games() -> Option<Vec<String>> {
    state()
        .games
        .as_ref()
        .filter(|(updated, _)| updated.elapsed() < FRESH)
        .map(|(_, games)| games.clone())
}

/// Whether the games list went stale since the last call
pub fn games_expired() -> bool {
    let mut state = state();
    let expired = state
        .games
        .as_ref()
        .is_some_and(|(updated, _)| updated.elapsed() >= FRESH);
    if expired {
        state.games = None;
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
