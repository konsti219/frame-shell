//! Messages between the frame shell on the headset and the host services of the PC it streams from, carried as
//! opaque bytes through WiVRn's connection (to_headset/from_headset::shell_message). Both ends are built together.

use serde::{Deserialize, Serialize};

use crate::{packet_client::PacketClient, packet_server::PacketServer};

/// Largest encoded message either side accepts
pub const MAX_MESSAGE: usize = 256 * 1024;

/// PC to headset
#[derive(Debug, Serialize, Deserialize)]
pub enum HostMessage {
	/// Steam app ids of the running games
	Games(Vec<String>),
	/// Host runtime frame timings, for the watch
	Frames(Vec<SessionFrame>),
	/// The PC runtime's clients
	Clients(Vec<RuntimeClient>),
	Error(String),
	Notification(Notification),
	/// A wayvr-ipc client on the PC (wayvrctl, scripts) connected to the shell through the host
	IpcOpen(u32),
	Ipc(u32, PacketClient),
	IpcClose(u32),
	Identity(HostIdentity),
	RuntimeUnavailable,
	CompositionSupported,
	/// Where monitor capture stands, sent whole on every change
	Capture(CaptureStatus),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CaptureStatus {
	/// Capture as a whole failed, before any monitor
	pub failed: Option<String>,
	/// By connector
	pub monitors: Vec<(String, MonitorStatus)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MonitorStatus {
	/// Behind another monitor's screen sharing request
	Queued,
	/// The PC's screen sharing dialog asks for this monitor
	Dialog,
	Streaming,
	Failed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostIdentity {
	pub id: String,
	pub name: String,
	pub version: String,
}

/// Headset to PC
#[derive(Debug, Serialize, Deserialize)]
pub enum ShellMessage {
	DesktopSettings(DesktopSettings),
	/// Whether the shell shows a monitor: hidden ones aren't encoded
	DesktopVisibility {
		surface: u32,
		visible: bool,
	},
	StopGame {
		app_id: String,
		force: bool,
	},
	/// Make a PC runtime client the one shown
	FocusClient(u32),
	ClientAction {
		client: u32,
		action: ClientAction,
	},
	Ipc(u32, PacketServer),
	IpcClose(u32),
	BackgroundVisible(bool),
	RequestState,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DesktopSettings {
	/// kbit/s for 1920×1080 worth of pixels, scaled to each monitor
	pub bitrate_per_1080p: u32,
	pub hevc: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum NotificationSource {
	/// The PC's desktop notifications
	Desktop,
	/// XSOverlay-style notifications sent to the PC (VRCX's "WayVR notifications")
	Overlay,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
	pub source: NotificationSource,
	pub title: String,
	pub body: String,
	pub timeout_seconds: f32,
	pub sound: bool,
}

/// A host runtime frame, as in WayVR's Monado metrics schema
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionFrame {
	pub session_id: i64,
	pub frame_id: i64,
	pub predicted_frame_time_ns: u64,
	pub predicted_wake_up_time_ns: u64,
	pub predicted_gpu_done_time_ns: u64,
	pub predicted_display_time_ns: u64,
	pub predicted_display_period_ns: u64,
	pub display_time_ns: u64,
	pub when_predicted_ns: u64,
	pub when_wait_woke_ns: u64,
	pub when_begin_ns: u64,
	pub when_delivered_ns: u64,
	pub when_gpu_done_ns: u64,
	pub discarded: bool,
	pub when_submit_begin_ns: u64,
	pub when_submit_end_ns: u64,
}

impl HostMessage {
	pub fn encode(&self) -> Vec<u8> {
		crate::ipc::data_encode(self)
	}
	pub fn decode(data: &[u8]) -> anyhow::Result<Self> {
		anyhow::ensure!(data.len() <= MAX_MESSAGE, "Host message too large");
		crate::ipc::data_decode(data)
	}
}

impl ShellMessage {
	pub fn encode(&self) -> Vec<u8> {
		crate::ipc::data_encode(self)
	}
	pub fn decode(data: &[u8]) -> anyhow::Result<Self> {
		anyhow::ensure!(data.len() <= MAX_MESSAGE, "Shell message too large");
		crate::ipc::data_decode(data)
	}
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ClientAction {
	Select,
	Automatic,
	Default,
	Overlay,
	Show,
}

/// An OpenXR runtime client with an active session (frame shell runtimes, through libmonado)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuntimeClient {
	pub id: u32,
	pub name: String,
	pub pid: i32,
	/// The Steam game it is, from its process environment
	pub steam_app_id: Option<String>,
	/// Selected default application, beneath any visible overlays
	pub primary: bool,
	pub overlay: bool,
	#[serde(default)]
	pub visible: bool,
	#[serde(default)]
	pub presentation_mode: u32,
}
