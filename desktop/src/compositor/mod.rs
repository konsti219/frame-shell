//! Compositor control abstraction for dynamic headless outputs, layout, and window/workspace migration.

pub mod hyprland;
pub mod wlr_output;

use anyhow::Result;
use std::sync::{
    Arc,
    atomic::AtomicBool,
    mpsc::Sender,
};
use wayvr_ipc::frame::VirtualCapabilities;

pub const DEFAULT_MAX_VIRTUAL_PANELS: u32 = 3;

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorGeometry {
    pub id: i64,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub logical_pos: (i32, i32),
    pub logical_size: (i32, i32),
    pub scale: f32,
    pub focused: bool,
    pub active_workspace_id: i32,
    pub active_workspace_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub address: String,
    pub title: String,
    pub app_id: String,
    pub workspace_id: i32,
    pub workspace_name: String,
    pub monitor_id: i64,
    pub monitor_name: String,
    pub mapped: bool,
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceInfo {
    pub id: i32,
    pub name: String,
    pub monitor_name: String,
    pub windows: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompositorEvent {
    WindowClosed {
        address: String,
    },
    WindowMoved {
        address: String,
        workspace_id: i32,
        workspace_name: String,
    },
    MonitorRemoved {
        name: String,
    },
}

pub fn normalize_window_address(addr: &str) -> String {
    let trimmed = addr.trim();
    let hex = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")).unwrap_or(trimmed);
    format!("0x{}", hex.to_ascii_lowercase())
}

pub trait CompositorBackend: Send + Sync {
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
    fn list_monitors(&self) -> Result<Vec<MonitorGeometry>>;
    fn list_windows(&self) -> Result<Vec<WindowInfo>>;
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>>;
    fn create_headless_output(&self, name: &str) -> Result<()>;
    fn configure_outputs(&self, updates: &[wlr_output::OutputConfigUpdate]) -> Result<()>;
    fn remove_output(&self, name: &str) -> Result<()>;
    fn move_window_to_workspace(&self, address: &str, workspace_target: &str) -> Result<()>;
    fn move_workspace_to_output(&self, workspace_id: i32, output: &str) -> Result<()>;
    fn subscribe_events(&self, tx: Sender<CompositorEvent>, stop: Arc<AtomicBool>) -> Result<()>;
}

/// Detects the active compositor and verifies virtual-panel prerequisites.
pub fn detect() -> (Option<Arc<dyn CompositorBackend>>, VirtualCapabilities) {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let is_hyprland = desktop
        .split(':')
        .any(|entry| entry.eq_ignore_ascii_case("Hyprland"))
        || std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some();

    if !is_hyprland {
        let name = if desktop.is_empty() {
            "unknown"
        } else {
            desktop.as_str()
        };
        return (
            None,
            VirtualCapabilities {
                supported: false,
                compositor: name.to_ascii_lowercase(),
                reason: Some("Virtual panels currently require Hyprland.".into()),
                max_panels: 0,
            },
        );
    }

    let backend = match hyprland::HyprlandBackend::from_env() {
        Ok(backend) => backend,
        Err(err) => {
            return (
                None,
                VirtualCapabilities {
                    supported: false,
                    compositor: "hyprland".into(),
                    reason: Some(format!("Hyprland IPC socket unavailable: {err:#}")),
                    max_panels: 0,
                },
            );
        }
    };

    if let Err(err) = backend.list_monitors() {
        return (
            None,
            VirtualCapabilities {
                supported: false,
                compositor: "hyprland".into(),
                reason: Some(format!("Cannot query Hyprland IPC socket: {err:#}")),
                max_panels: 0,
            },
        );
    }

    if !crate::wayland_capture::supports_ext_image_copy_capture() {
        return (
            None,
            VirtualCapabilities {
                supported: false,
                compositor: "hyprland".into(),
                reason: Some(
                    "Wayland compositor does not advertise ext-image-copy-capture-v1.".into(),
                ),
                max_panels: 0,
            },
        );
    }

    if !wlr_output::supports_wlr_output_management() {
        return (
            None,
            VirtualCapabilities {
                supported: false,
                compositor: "hyprland".into(),
                reason: Some(
                    "Wayland compositor does not advertise zwlr_output_manager_v1.".into(),
                ),
                max_panels: 0,
            },
        );
    }

    (
        Some(Arc::new(backend)),
        VirtualCapabilities {
            supported: true,
            compositor: "hyprland".into(),
            reason: None,
            max_panels: DEFAULT_MAX_VIRTUAL_PANELS,
        },
    )
}
