//! Messages between the frame shell on the headset and the host services of the PC it streams from, carried as
//! opaque bytes through WiVRn's connection (to_headset/from_headset::shell_message). Both ends are built together.

use serde::{Deserialize, Serialize};

use crate::{packet_client::PacketClient, packet_server::PacketServer};

/// Largest encoded message either side accepts
pub const MAX_MESSAGE: usize = 256 * 1024;

/// Virtual panel surface IDs start here (`0x8000_0000`), keeping physical monitors `0..N` separate.
pub const VIRTUAL_SURFACE_BASE: u32 = 1 << 31;
/// Highest valid virtual surface ID so `video_shard::desktop_streams (0x100) + surface` cannot overflow `u32`.
pub const VIRTUAL_SURFACE_MAX: u32 = 0xFFFF_FEFF;

pub const fn is_virtual_surface(surface: u32) -> bool {
	surface >= VIRTUAL_SURFACE_BASE
}

pub const fn virtual_surface_id(slot: u32) -> Option<u32> {
	match VIRTUAL_SURFACE_BASE.checked_add(slot) {
		Some(id) if id <= VIRTUAL_SURFACE_MAX => Some(id),
		_ => None,
	}
}

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
	/// Authoritative snapshot of virtual panel capabilities, active virtual panels, and physical monitors
	VirtualState(VirtualState),
	/// Available windows and workspaces that can be placed onto a virtual panel
	VirtualTargets(VirtualTargets),
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
	RefreshVirtualTargets,
	CreateVirtualPanel {
		source: VirtualTargetKind,
		size: [u32; 2],
	},
	CloseVirtualPanel {
		surface: u32,
	},
	SyncPanelLayout {
		placements: Vec<PanelLayoutTarget>,
	},
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualCapabilities {
	pub supported: bool,
	pub compositor: String,
	pub reason: Option<String>,
	pub max_panels: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PhysicalMonitorInfo {
	pub surface: Option<u32>,
	pub name: String,
	pub width: u32,
	pub height: u32,
	pub logical_pos: [i32; 2],
	pub logical_size: [i32; 2],
	pub scale: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VirtualTargetKind {
	Blank {
		width: u32,
		height: u32,
	},
	Window {
		address: String,
		title: String,
		app_id: String,
	},
	Workspace {
		id: i32,
		name: String,
	},
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VirtualPanelInfo {
	pub surface: u32,
	pub output_name: String,
	pub title: String,
	pub width: u32,
	pub height: u32,
	pub logical_pos: [i32; 2],
	pub logical_size: [i32; 2],
	pub scale: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualWindowTarget {
	pub address: String,
	pub title: String,
	pub app_id: String,
	pub workspace_id: i32,
	pub workspace_name: String,
	pub monitor: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualWorkspaceTarget {
	pub id: i32,
	pub name: String,
	pub monitor: String,
	pub windows: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VirtualState {
	pub capabilities: VirtualCapabilities,
	pub panels: Vec<VirtualPanelInfo>,
	pub physical_monitors: Vec<PhysicalMonitorInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualTargets {
	pub windows: Vec<VirtualWindowTarget>,
	pub workspaces: Vec<VirtualWorkspaceTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelLayoutTarget {
	pub surface: u32,
	/// Target top-left position in the compositor's 2D integer logical pixel space.
	pub x: i32,
	pub y: i32,
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
	pub gpu_time_ns: u64,
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn virtual_surface_id_bounds() {
		assert!(!is_virtual_surface(0));
		assert!(!is_virtual_surface(VIRTUAL_SURFACE_BASE - 1));
		assert!(is_virtual_surface(VIRTUAL_SURFACE_BASE));
		assert_eq!(virtual_surface_id(0), Some(VIRTUAL_SURFACE_BASE));
		assert_eq!(virtual_surface_id(0x7FFF_FEFF), Some(VIRTUAL_SURFACE_MAX));
		assert_eq!(VIRTUAL_SURFACE_MAX.checked_add(0x100), Some(u32::MAX));
		assert_eq!(virtual_surface_id(0x7FFF_FF00), None);
		assert_eq!(virtual_surface_id(u32::MAX), None);
	}

	#[test]
	fn virtual_messages_round_trip() {
		let state = HostMessage::VirtualState(VirtualState {
			capabilities: VirtualCapabilities {
				supported: true,
				compositor: "hyprland".into(),
				reason: None,
				max_panels: 3,
			},
			panels: vec![VirtualPanelInfo {
				surface: VIRTUAL_SURFACE_BASE,
				output_name: "FRAME-V1".into(),
				title: "Terminal".into(),
				width: 1920,
				height: 1080,
				logical_pos: [0, -1080],
				logical_size: [1920, 1080],
				scale: 1.0,
			}],
			physical_monitors: vec![PhysicalMonitorInfo {
				surface: Some(0),
				name: "DP-4".into(),
				width: 2560,
				height: 1080,
				logical_pos: [0, 0],
				logical_size: [2560, 1080],
				scale: 1.0,
			}],
		});
		let decoded = HostMessage::decode(&state.encode()).expect("decode VirtualState");
		assert!(matches!(
			decoded,
			HostMessage::VirtualState(ref s)
				if s.capabilities.supported
					&& s.panels.len() == 1
					&& s.panels[0].surface == VIRTUAL_SURFACE_BASE
					&& s.physical_monitors.len() == 1
		));

		let targets = HostMessage::VirtualTargets(VirtualTargets {
			windows: vec![VirtualWindowTarget {
				address: "0x1234".into(),
				title: "Editor".into(),
				app_id: "code".into(),
				workspace_id: 2,
				workspace_name: "2".into(),
				monitor: "DP-4".into(),
			}],
			workspaces: vec![VirtualWorkspaceTarget {
				id: 2,
				name: "2".into(),
				monitor: "DP-4".into(),
				windows: 3,
			}],
		});
		assert!(matches!(
			HostMessage::decode(&targets.encode()).expect("decode VirtualTargets"),
			HostMessage::VirtualTargets(ref t) if t.windows.len() == 1 && t.workspaces.len() == 1
		));

		let shell_msgs = [
			ShellMessage::RefreshVirtualTargets,
			ShellMessage::CreateVirtualPanel {
				source: VirtualTargetKind::Window {
					address: "0x1234".into(),
					title: "Editor".into(),
					app_id: "code".into(),
				},
				size: [1920, 1080],
			},
			ShellMessage::CloseVirtualPanel {
				surface: VIRTUAL_SURFACE_BASE,
			},
			ShellMessage::SyncPanelLayout {
				placements: vec![PanelLayoutTarget {
					surface: VIRTUAL_SURFACE_BASE,
					x: 320,
					y: -1080,
				}],
			},
		];
		for msg in &shell_msgs {
			let decoded = ShellMessage::decode(&msg.encode()).expect("decode ShellMessage");
			assert_eq!(msg.encode(), decoded.encode());
		}
	}
}
