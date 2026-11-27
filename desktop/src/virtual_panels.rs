//! Virtual headless panel lifecycle manager for interactive VR desktop panels.

use crate::{
    Output, Shared, capture,
    compositor::{
        self, CompositorBackend, CompositorEvent, MonitorGeometry, normalize_window_address,
        wlr_output::OutputConfigUpdate,
    },
    wayland_capture,
};
use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::JoinHandle,
};
use wayvr_ipc::frame::{
    HostMessage, PanelLayoutTarget, PhysicalMonitorInfo, VirtualCapabilities, VirtualPanelInfo,
    VirtualState, VirtualTargetKind, VirtualTargets, VirtualWindowTarget, VirtualWorkspaceTarget,
    virtual_surface_id,
};

#[derive(Debug)]
pub enum Command {
    RequestState,
    RefreshTargets,
    Create {
        source: VirtualTargetKind,
        size: [u32; 2],
    },
    Close {
        surface: u32,
    },
    SyncLayout {
        placements: Vec<PanelLayoutTarget>,
    },
    Event(CompositorEvent),
    Shutdown,
}

#[derive(Clone)]
enum PanelOwnership {
    Blank {
        workspace_id: i32,
    },
    Window {
        address: String,
        title: String,
        app_id: String,
        panel_workspace_id: i32,
        return_workspace_id: i32,
        return_monitor: String,
    },
    Workspace {
        workspace_id: i32,
        workspace_name: String,
        return_monitor: String,
    },
}

struct ActivePanel {
    surface: u32,
    output_name: String,
    title: String,
    width: u32,
    height: u32,
    logical_pos: [i32; 2],
    logical_size: [i32; 2],
    scale: f32,
    ownership: PanelOwnership,
    cancel: Arc<AtomicBool>,
    capture_thread: Option<JoinHandle<()>>,
}

impl ActivePanel {
    fn info(&self) -> VirtualPanelInfo {
        VirtualPanelInfo {
            surface: self.surface,
            output_name: self.output_name.clone(),
            title: self.title.clone(),
            width: self.width,
            height: self.height,
            logical_pos: self.logical_pos,
            logical_size: self.logical_size,
            scale: self.scale,
        }
    }
}

pub struct VirtualManager {
    tx: Sender<Command>,
    #[cfg(test)]
    capabilities: VirtualCapabilities,
}

impl VirtualManager {
    /// Initializes virtual panel support, synchronously cleans up any leftover `FRAME-V*` outputs
    /// before physical capture starts, and spawns the virtual-panel manager worker thread.
    pub fn start(output: Output, shared: Arc<Shared>) -> (Self, JoinHandle<()>) {
        let (backend, capabilities) = compositor::detect();
        if let Some(ref backend) = backend
            && let Err(err) = orphan_sweep(backend.as_ref())
        {
            eprintln!("Virtual panel orphan sweep failed: {err:#}");
        }
        let (tx, rx) = mpsc::channel();
        let tx_events = tx.clone();
        #[cfg(test)]
        let test_caps = capabilities.clone();
        let handle = std::thread::Builder::new()
            .name("frame-virtual".into())
            .spawn(move || {
                run_worker(backend, capabilities, output, shared, rx, tx_events);
            })
            .expect("spawning frame-virtual thread");
        (
            Self {
                tx,
                #[cfg(test)]
                capabilities: test_caps,
            },
            handle,
        )
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    #[cfg(test)]
    pub fn capabilities(&self) -> &VirtualCapabilities {
        &self.capabilities
    }
}

/// Evacuates any windows on leftover `FRAME-V*` outputs and removes those headless outputs.
pub fn orphan_sweep(backend: &dyn CompositorBackend) -> Result<()> {
    let monitors = backend.list_monitors()?;
    let orphans: Vec<MonitorGeometry> = monitors
        .iter()
        .filter(|m| capture::is_virtual_output_name(&m.name))
        .cloned()
        .collect();
    if orphans.is_empty() {
        return Ok(());
    }
    let fallback_ws = pick_primary_physical(&monitors)
        .map(|m| m.active_workspace_id.max(1))
        .unwrap_or(1);
    if let Ok(windows) = backend.list_windows() {
        for win in windows {
            if capture::is_virtual_output_name(&win.monitor_name) {
                let _ = backend.move_window_to_workspace(&win.address, &fallback_ws.to_string());
            }
        }
    }
    for orphan in orphans {
        eprintln!("Cleaning up orphaned virtual output {}", orphan.name);
        let _ = backend.remove_output(&orphan.name);
    }
    Ok(())
}

fn pick_primary_physical(monitors: &[MonitorGeometry]) -> Option<&MonitorGeometry> {
    monitors
        .iter()
        .filter(|m| !capture::is_virtual_output_name(&m.name))
        .min_by_key(|m| (m.logical_pos != (0, 0), !m.focused, m.id))
}

fn clamp_even_size(size: [u32; 2]) -> (u32, u32) {
    let clamp_axis = |v: u32, default: u32, min: u32, max: u32| -> u32 {
        let v = if v == 0 { default } else { v }.clamp(min, max);
        (v / 2) * 2
    };
    (
        clamp_axis(size[0], 1920, 640, 3840),
        clamp_axis(size[1], 1080, 480, 2160),
    )
}

fn to_output_geos(monitors: &[MonitorGeometry]) -> Vec<capture::OutputGeometry> {
    monitors
        .iter()
        .map(|m| capture::OutputGeometry {
            name: m.name.clone(),
            logical_pos: m.logical_pos,
            logical_size: m.logical_size,
            physical_size: (m.width as i32, m.height as i32),
        })
        .collect()
}

struct Worker {
    backend: Option<Arc<dyn CompositorBackend>>,
    capabilities: VirtualCapabilities,
    output: Output,
    shared: Arc<Shared>,
    panels: BTreeMap<u32, ActivePanel>,
    next_slot: u32,
}

impl Worker {
    fn physical_monitors_info(&self, monitors: &[MonitorGeometry]) -> Vec<PhysicalMonitorInfo> {
        let layout = crate::lock(&self.shared.output_layout);
        monitors
            .iter()
            .filter(|m| !capture::is_virtual_output_name(&m.name))
            .map(|m| PhysicalMonitorInfo {
                surface: layout.stream_id_for_output(&m.name),
                name: m.name.clone(),
                width: m.width,
                height: m.height,
                logical_pos: [m.logical_pos.0, m.logical_pos.1],
                logical_size: [m.logical_size.0, m.logical_size.1],
                scale: m.scale,
            })
            .collect()
    }

    fn refresh_panel_geometries(&mut self, monitors: &[MonitorGeometry]) {
        for panel in self.panels.values_mut() {
            if let Some(m) = monitors.iter().find(|m| m.name == panel.output_name) {
                panel.width = m.width;
                panel.height = m.height;
                panel.logical_pos = [m.logical_pos.0, m.logical_pos.1];
                panel.logical_size = [m.logical_size.0, m.logical_size.1];
                panel.scale = m.scale;
            }
        }
    }

    fn broadcast_state(&mut self) {
        let monitors = self
            .backend
            .as_ref()
            .and_then(|b| b.list_monitors().ok())
            .unwrap_or_default();
        if !monitors.is_empty() {
            self.shared
                .update_outputs(&self.output, to_output_geos(&monitors));
            self.refresh_panel_geometries(&monitors);
        }
        let physical_monitors = self.physical_monitors_info(&monitors);
        let panels = self.panels.values().map(ActivePanel::info).collect();
        self.output.send(&HostMessage::VirtualState(VirtualState {
            capabilities: self.capabilities.clone(),
            panels,
            physical_monitors,
        }));
    }

    fn broadcast_targets(&self) {
        let Some(ref backend) = self.backend else {
            return;
        };
        let windows = match backend.list_windows() {
            Ok(w) => w,
            Err(err) => {
                eprintln!("Virtual list_windows failed: {err:#}");
                Vec::new()
            }
        };
        let workspaces = match backend.list_workspaces() {
            Ok(ws) => ws,
            Err(err) => {
                eprintln!("Virtual list_workspaces failed: {err:#}");
                Vec::new()
            }
        };
        let owned_workspaces: Vec<i32> = self
            .panels
            .values()
            .filter_map(|p| match &p.ownership {
                PanelOwnership::Blank { workspace_id } => Some(*workspace_id),
                _ => None,
            })
            .collect();

        let windows = windows
            .into_iter()
            .map(|w| VirtualWindowTarget {
                address: w.address,
                title: w.title,
                app_id: w.app_id,
                workspace_id: w.workspace_id,
                workspace_name: w.workspace_name,
                monitor: w.monitor_name,
            })
            .collect();
        let workspaces = workspaces
            .into_iter()
            .filter(|ws| !owned_workspaces.contains(&ws.id))
            .map(|ws| VirtualWorkspaceTarget {
                id: ws.id,
                name: ws.name,
                monitor: ws.monitor_name,
                windows: ws.windows,
            })
            .collect();
        self.output
            .send(&HostMessage::VirtualTargets(VirtualTargets {
                windows,
                workspaces,
            }));
    }

    fn handle_create(&mut self, source: VirtualTargetKind, size: [u32; 2]) {
        let Some(backend) = self.backend.clone() else {
            self.output.send(&HostMessage::Error(
                self.capabilities
                    .reason
                    .clone()
                    .unwrap_or_else(|| "Virtual panels require Hyprland.".into()),
            ));
            return;
        };
        if !self.capabilities.supported {
            self.output.send(&HostMessage::Error(
                self.capabilities
                    .reason
                    .clone()
                    .unwrap_or_else(|| "Virtual panels are unavailable on this host.".into()),
            ));
            return;
        }
        if self.panels.len() >= self.capabilities.max_panels as usize {
            self.output.send(&HostMessage::Error(format!(
                "Virtual panel limit reached ({}/{})",
                self.panels.len(),
                self.capabilities.max_panels
            )));
            self.broadcast_state();
            return;
        }

        // Duplicate target checks
        match &source {
            VirtualTargetKind::Blank { .. } => {}
            VirtualTargetKind::Window { address, .. } => {
                let norm = normalize_window_address(address);
                if self.panels.values().any(|p| {
                    matches!(&p.ownership, PanelOwnership::Window { address: a, .. } if *a == norm)
                }) {
                    self.output.send(&HostMessage::Error(
                        "That window already has an active virtual panel".into(),
                    ));
                    self.broadcast_state();
                    return;
                }
            }
            VirtualTargetKind::Workspace { id, .. } => {
                if self.panels.values().any(|p| {
                    matches!(&p.ownership, PanelOwnership::Workspace { workspace_id, .. } if workspace_id == id)
                }) {
                    self.output.send(&HostMessage::Error(format!(
                        "Workspace {id} already has an active virtual panel"
                    )));
                    self.broadcast_state();
                    return;
                }
            }
        }

        let Some(surface) = virtual_surface_id(self.next_slot) else {
            self.output
                .send(&HostMessage::Error("Virtual surface ID limit reached".into()));
            return;
        };
        self.next_slot += 1;
        let output_name = format!("FRAME-V{}", self.next_slot);
        let req_size = match &source {
            VirtualTargetKind::Blank { width, height } if *width > 0 && *height > 0 => {
                [*width, *height]
            }
            _ => size,
        };
        let (width, height) = clamp_even_size(req_size);

        let attempt = (|| -> Result<ActivePanel> {
            let monitors = backend.list_monitors()?;
            let primary_phys = pick_primary_physical(&monitors)
                .context("No physical monitor available")?;
            let primary_phys_name = primary_phys.name.clone();
            let primary_phys_ws = primary_phys.active_workspace_id.max(1);
            let spawn_x = monitors
                .iter()
                .map(|m| m.logical_pos.0 + m.logical_size.0)
                .max()
                .unwrap_or(0);
            let spawn_y = primary_phys.logical_pos.1;

            let (title, ownership) = match &source {
                VirtualTargetKind::Blank { .. } => (
                    output_name.clone(),
                    PanelOwnership::Blank {
                        workspace_id: -1,
                    },
                ),
                VirtualTargetKind::Window { address, .. } => {
                    let norm = normalize_window_address(address);
                    let win = backend
                        .list_windows()?
                        .into_iter()
                        .find(|w| w.address == norm)
                        .context("Selected window is no longer open")?;
                    let return_monitor = if capture::is_virtual_output_name(&win.monitor_name)
                        || win.monitor_name.is_empty()
                    {
                        primary_phys_name.clone()
                    } else {
                        win.monitor_name.clone()
                    };
                    let return_ws = if capture::is_virtual_output_name(&win.monitor_name) {
                        primary_phys_ws
                    } else {
                        win.workspace_id.max(1)
                    };
                    let label = if !win.title.is_empty() {
                        win.title.clone()
                    } else if !win.app_id.is_empty() {
                        win.app_id.clone()
                    } else {
                        output_name.clone()
                    };
                    (
                        label,
                        PanelOwnership::Window {
                            address: norm,
                            title: win.title,
                            app_id: win.app_id,
                            panel_workspace_id: -1,
                            return_workspace_id: return_ws,
                            return_monitor,
                        },
                    )
                }
                VirtualTargetKind::Workspace { id, name } => {
                    let ws = backend
                        .list_workspaces()?
                        .into_iter()
                        .find(|w| w.id == *id)
                        .with_context(|| format!("Workspace {id} no longer exists"))?;
                    let return_monitor = if capture::is_virtual_output_name(&ws.monitor_name)
                        || ws.monitor_name.is_empty()
                    {
                        primary_phys_name.clone()
                    } else {
                        ws.monitor_name.clone()
                    };
                    let label = if !ws.name.is_empty() {
                        format!("Workspace {}", ws.name)
                    } else if !name.is_empty() {
                        format!("Workspace {name}")
                    } else {
                        format!("Workspace {id}")
                    };
                    (
                        label,
                        PanelOwnership::Workspace {
                            workspace_id: ws.id,
                            workspace_name: ws.name,
                            return_monitor,
                        },
                    )
                }
            };

            let pin_updates: Vec<OutputConfigUpdate> = monitors
                .iter()
                .map(|m| OutputConfigUpdate {
                    name: m.name.clone(),
                    mode: None,
                    pos: Some(m.logical_pos),
                    scale: Some(m.scale),
                })
                .collect();

            backend.create_headless_output(&output_name)?;
            let setup_res = (|| -> Result<PanelOwnership> {
                let mut config_updates = pin_updates;
                config_updates.push(OutputConfigUpdate {
                    name: output_name.clone(),
                    mode: Some((width, height, 60)),
                    pos: Some((spawn_x, spawn_y)),
                    scale: Some(1.0),
                });
                backend.configure_outputs(&config_updates)?;

                let monitors_after = backend.list_monitors()?;
                let vmon = monitors_after
                    .iter()
                    .find(|m| m.name == output_name)
                    .with_context(|| format!("Created output {output_name} missing"))?;
                let default_ws_id = vmon.active_workspace_id.max(1);

                match ownership {
                    PanelOwnership::Blank { .. } => Ok(PanelOwnership::Blank {
                        workspace_id: default_ws_id,
                    }),
                    PanelOwnership::Window {
                        address,
                        title,
                        app_id,
                        return_workspace_id,
                        return_monitor,
                        ..
                    } => {
                        backend.move_window_to_workspace(&address, &default_ws_id.to_string())?;
                        Ok(PanelOwnership::Window {
                            address,
                            title,
                            app_id,
                            panel_workspace_id: default_ws_id,
                            return_workspace_id,
                            return_monitor,
                        })
                    }
                    PanelOwnership::Workspace {
                        workspace_id,
                        workspace_name,
                        return_monitor,
                    } => {
                        backend.move_workspace_to_output(workspace_id, &output_name)?;
                        Ok(PanelOwnership::Workspace {
                            workspace_id,
                            workspace_name,
                            return_monitor,
                        })
                    }
                }
            })();

            let ownership = match setup_res {
                Ok(own) => own,
                Err(err) => {
                    let _ = backend.remove_output(&output_name);
                    return Err(err);
                }
            };

            let updated_monitors = backend.list_monitors()?;
            self.shared
                .update_outputs(&self.output, to_output_geos(&updated_monitors));
            let vmon = updated_monitors
                .iter()
                .find(|m| m.name == output_name)
                .cloned();
            let (actual_w, actual_h, pos, logical_size, scale) = vmon.map_or(
                (width, height, [spawn_x, spawn_y], [width as i32, height as i32], 1.0),
                |m| {
                    (
                        m.width,
                        m.height,
                        [m.logical_pos.0, m.logical_pos.1],
                        [m.logical_size.0, m.logical_size.1],
                        m.scale,
                    )
                },
            );

            self.shared.register_stream(surface, output_name.clone());
            let cancel = Arc::new(AtomicBool::new(false));
            let cancel_thread = cancel.clone();
            let output_thread = self.output.clone();
            let shared_thread = self.shared.clone();
            let name_thread = output_name.clone();
            let capture_thread = std::thread::Builder::new()
                .name(format!("vcap-{}", output_name))
                .spawn(move || {
                    if let Err(err) = wayland_capture::stream_virtual_output(
                        surface,
                        &name_thread,
                        &cancel_thread,
                        &output_thread,
                        &shared_thread,
                    ) {
                        eprintln!("Virtual capture {name_thread} failed: {err:#}");
                        output_thread.send(&HostMessage::Error(format!(
                            "Virtual panel {name_thread} capture stopped: {err:#}"
                        )));
                    }
                    shared_thread.unregister_stream(surface);
                    output_thread.removed(surface);
                })?;

            Ok(ActivePanel {
                surface,
                output_name,
                title,
                width: actual_w,
                height: actual_h,
                logical_pos: pos,
                logical_size,
                scale,
                ownership,
                cancel,
                capture_thread: Some(capture_thread),
            })
        })();

        match attempt {
            Ok(panel) => {
                eprintln!(
                    "Created virtual panel {} (surface {surface:#x}, {}x{} at {:?})",
                    panel.output_name, panel.width, panel.height, panel.logical_pos
                );
                self.panels.insert(surface, panel);
                self.broadcast_state();
                self.broadcast_targets();
            }
            Err(err) => {
                eprintln!("Failed to create virtual panel: {err:#}");
                self.output
                    .send(&HostMessage::Error(format!("Virtual panel failed: {err:#}")));
                self.broadcast_state();
                self.broadcast_targets();
            }
        }
    }

    fn evacuate_output(&self, backend: &dyn CompositorBackend, panel: &ActivePanel, restore_owned: bool) {
        let Ok(monitors) = backend.list_monitors() else {
            return;
        };
        let Some(fallback) = pick_primary_physical(&monitors).cloned() else {
            return;
        };
        let fallback_ws = fallback.active_workspace_id.max(1);
        let windows = backend.list_windows().unwrap_or_default();

        match &panel.ownership {
            PanelOwnership::Blank { workspace_id } => {
                for win in &windows {
                    if win.monitor_name == panel.output_name || win.workspace_id == *workspace_id {
                        let _ = backend
                            .move_window_to_workspace(&win.address, &fallback_ws.to_string());
                    }
                }
            }
            PanelOwnership::Window {
                address,
                panel_workspace_id,
                return_workspace_id,
                return_monitor,
                ..
            } => {
                let target_ws = if restore_owned {
                    let return_mon_exists = monitors
                        .iter()
                        .any(|m| !capture::is_virtual_output_name(&m.name) && m.name == *return_monitor);
                    if return_mon_exists {
                        *return_workspace_id
                    } else {
                        fallback_ws
                    }
                } else {
                    fallback_ws
                };
                for win in &windows {
                    if win.address == *address && restore_owned {
                        let _ = backend.move_window_to_workspace(&win.address, &target_ws.to_string());
                    } else if win.monitor_name == panel.output_name || win.workspace_id == *panel_workspace_id
                    {
                        let _ = backend
                            .move_window_to_workspace(&win.address, &fallback_ws.to_string());
                    }
                }
            }
            PanelOwnership::Workspace {
                workspace_id,
                return_monitor,
                ..
            } => {
                let target_monitor = monitors
                    .iter()
                    .find(|m| !capture::is_virtual_output_name(&m.name) && m.name == *return_monitor)
                    .map_or(fallback.name.as_str(), |m| m.name.as_str());
                let ws_exists = backend
                    .list_workspaces()
                    .unwrap_or_default()
                    .iter()
                    .any(|ws| ws.id == *workspace_id);
                if ws_exists {
                    let _ = backend.move_workspace_to_output(*workspace_id, target_monitor);
                }
                for win in &windows {
                    if win.monitor_name == panel.output_name && win.workspace_id != *workspace_id {
                        let _ = backend
                            .move_window_to_workspace(&win.address, &fallback_ws.to_string());
                    }
                }
            }
        }
    }

    fn close_panel(&mut self, surface: u32, restore_owned: bool, broadcast: bool) {
        let Some(mut panel) = self.panels.remove(&surface) else {
            return;
        };
        panel.cancel.store(true, Ordering::Release);
        if let Some(handle) = panel.capture_thread.take() {
            let _ = handle.join();
        }
        self.shared.unregister_stream(surface);
        crate::lock(&self.shared.visibility).remove(&surface);
        crate::lock(&self.shared.keyframes).remove(&surface);

        if let Some(backend) = self.backend.as_ref() {
            let before_monitors = backend.list_monitors().unwrap_or_default();
            self.evacuate_output(backend.as_ref(), &panel, restore_owned);
            let _ = backend.remove_output(&panel.output_name);
            let after_monitors = backend.list_monitors().unwrap_or_default();
            let restore_updates: Vec<OutputConfigUpdate> = before_monitors
                .iter()
                .filter(|b| b.name != panel.output_name)
                .filter_map(|b| {
                    let after = after_monitors.iter().find(|a| a.name == b.name)?;
                    (after.logical_pos != b.logical_pos).then(|| OutputConfigUpdate {
                        name: b.name.clone(),
                        mode: None,
                        pos: Some(b.logical_pos),
                        scale: Some(b.scale),
                    })
                })
                .collect();
            if !restore_updates.is_empty() {
                let _ = backend.configure_outputs(&restore_updates);
            }
            if let Ok(monitors) = backend.list_monitors() {
                self.shared
                    .update_outputs(&self.output, to_output_geos(&monitors));
            }
        }
        self.output.removed(surface);
        eprintln!("Closed virtual panel {}", panel.output_name);
        if broadcast {
            self.broadcast_state();
            self.broadcast_targets();
        }
    }

    fn handle_sync_layout(&mut self, placements: Vec<PanelLayoutTarget>) {
        let Some(backend) = self.backend.clone() else {
            return;
        };
        let mut updates = Vec::new();
        for target in &placements {
            if let Some(panel) = self.panels.get(&target.surface)
                && panel.logical_pos != [target.x, target.y]
            {
                updates.push(OutputConfigUpdate {
                    name: panel.output_name.clone(),
                    mode: Some((panel.width, panel.height, 60)),
                    pos: Some((target.x, target.y)),
                    scale: Some(panel.scale),
                });
            }
        }
        if updates.is_empty() {
            return;
        }
        match backend.configure_outputs(&updates) {
            Ok(()) => {
                if let Ok(monitors) = backend.list_monitors() {
                    self.shared
                        .update_outputs(&self.output, to_output_geos(&monitors));
                    for panel in self.panels.values_mut() {
                        if let Some(mon) = monitors.iter().find(|m| m.name == panel.output_name) {
                            panel.width = mon.width;
                            panel.height = mon.height;
                            panel.logical_pos = [mon.logical_pos.0, mon.logical_pos.1];
                            panel.logical_size = [mon.logical_size.0, mon.logical_size.1];
                            panel.scale = mon.scale;
                        }
                    }
                } else {
                    for target in &placements {
                        if let Some(panel) = self.panels.get_mut(&target.surface) {
                            panel.logical_pos = [target.x, target.y];
                        }
                    }
                }
            }
            Err(err) => {
                eprintln!("Virtual panel layout sync failed: {err:#}");
                self.output.send(&HostMessage::Error(format!(
                    "Failed to sync virtual panel layout: {err:#}"
                )));
            }
        }
        self.broadcast_state();
    }

    fn handle_event(&mut self, event: CompositorEvent) {
        match event {
            CompositorEvent::WindowClosed { address } => {
                let norm = normalize_window_address(&address);
                let victim = self.panels.iter().find_map(|(&id, p)| {
                    matches!(&p.ownership, PanelOwnership::Window { address: a, .. } if *a == norm)
                        .then_some(id)
                });
                if let Some(surface) = victim {
                    eprintln!("Popped-out window {norm} closed; closing virtual panel");
                    self.close_panel(surface, false, true);
                } else {
                    self.broadcast_targets();
                }
            }
            CompositorEvent::WindowMoved {
                address,
                workspace_id,
                ..
            } => {
                let norm = normalize_window_address(&address);
                let moved_off = self.panels.iter().find_map(|(&id, p)| {
                    matches!(
                        &p.ownership,
                        PanelOwnership::Window {
                            address: a,
                            panel_workspace_id,
                            ..
                        } if *a == norm && *panel_workspace_id != workspace_id
                    )
                    .then_some(id)
                });
                if let Some(surface) = moved_off {
                    eprintln!(
                        "Popped-out window {norm} moved away from virtual workspace; closing virtual panel"
                    );
                    self.close_panel(surface, false, true);
                } else {
                    self.broadcast_targets();
                }
            }
            CompositorEvent::MonitorRemoved { name } => {
                if let Some(surface) = self
                    .panels
                    .iter()
                    .find_map(|(&id, p)| (p.output_name == name).then_some(id))
                {
                    eprintln!("Virtual output {name} removed externally; closing panel");
                    self.close_panel(surface, true, true);
                } else {
                    self.broadcast_state();
                }
            }
        }
    }

    fn shutdown_all(&mut self) {
        let ids: Vec<u32> = self.panels.keys().copied().collect();
        for id in ids {
            self.close_panel(id, true, false);
        }
    }
}

fn run_worker(
    backend: Option<Arc<dyn CompositorBackend>>,
    capabilities: VirtualCapabilities,
    output: Output,
    shared: Arc<Shared>,
    rx: Receiver<Command>,
    tx_events: Sender<Command>,
) {
    let event_stop = Arc::new(AtomicBool::new(false));
    let event_thread = backend.as_ref().map(|b| {
        let b = b.clone();
        let stop = event_stop.clone();
        let (ev_tx, ev_rx) = mpsc::channel();
        let fwd_tx = tx_events.clone();
        let sub_handle = std::thread::Builder::new()
            .name("frame-hypr-events".into())
            .spawn(move || {
                let _ = b.subscribe_events(ev_tx, stop);
            })
            .expect("spawning event listener");
        let fwd_handle = std::thread::Builder::new()
            .name("frame-hypr-fwd".into())
            .spawn(move || {
                while let Ok(ev) = ev_rx.recv() {
                    if fwd_tx.send(Command::Event(ev)).is_err() {
                        break;
                    }
                }
            })
            .expect("spawning event forwarder");
        (sub_handle, fwd_handle)
    });

    let mut worker = Worker {
        backend,
        capabilities,
        output,
        shared,
        panels: BTreeMap::new(),
        next_slot: 0,
    };
    worker.broadcast_state();

    while let Ok(cmd) = rx.recv() {
        if worker.output.stopped() && !matches!(cmd, Command::Shutdown) {
            break;
        }
        match cmd {
            Command::RequestState => {
                worker.broadcast_state();
                worker.broadcast_targets();
            }
            Command::RefreshTargets => worker.broadcast_targets(),
            Command::Create { source, size } => worker.handle_create(source, size),
            Command::Close { surface } => worker.close_panel(surface, true, true),
            Command::SyncLayout { placements } => worker.handle_sync_layout(placements),
            Command::Event(ev) => worker.handle_event(ev),
            Command::Shutdown => break,
        }
    }

    worker.shutdown_all();
    event_stop.store(true, Ordering::Release);
    drop(tx_events);
    if let Some((sub, fwd)) = event_thread {
        let _ = sub.join();
        let _ = fwd.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_void;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    #[ignore = "requires a running Hyprland desktop session and GStreamer hardware encoder"]
    #[test]
    fn live_hyprland_blank_panel_lifecycle() {
        #[derive(Default)]
        struct Events {
            messages: Vec<HostMessage>,
            surfaces: Vec<u32>,
            removed: Vec<u32>,
            videos: usize,
        }

        extern "C" fn on_surface(user: *mut c_void, surface: *const frame_abi::frame_host_surface) {
            let events = unsafe { &*(user as *const Mutex<Events>) };
            let surface = unsafe { &*surface };
            events.lock().unwrap().surfaces.push(surface.id);
        }
        extern "C" fn on_region(_user: *mut c_void, _id: u32, _region: *const f32) {}
        extern "C" fn on_removed(user: *mut c_void, id: u32) {
            let events = unsafe { &*(user as *const Mutex<Events>) };
            events.lock().unwrap().removed.push(id);
        }
        extern "C" fn on_video(user: *mut c_void, _id: u32, _data: *const u8, size: usize) {
            if size > 0 {
                let events = unsafe { &*(user as *const Mutex<Events>) };
                events.lock().unwrap().videos += 1;
            }
        }
        extern "C" fn on_message(user: *mut c_void, data: *const u8, size: usize) {
            let slice = unsafe { std::slice::from_raw_parts(data, size) };
            if let Ok(msg) = HostMessage::decode(slice) {
                let events = unsafe { &*(user as *const Mutex<Events>) };
                events.lock().unwrap().messages.push(msg);
            }
        }

        struct CleanupGuard {
            output: Option<Output>,
            manager: Option<VirtualManager>,
            handle: Option<std::thread::JoinHandle<()>>,
            user_ptr: *mut Mutex<Events>,
        }
        impl Drop for CleanupGuard {
            fn drop(&mut self) {
                if let Some(out) = self.output.take() {
                    out.stop();
                }
                if let Some(mgr) = self.manager.take() {
                    mgr.send(Command::Shutdown);
                }
                if let Some(handle) = self.handle.take() {
                    let _ = handle.join();
                }
                for slot in 1..=4 {
                    let _ = std::process::Command::new("hyprctl")
                        .args(["output", "remove", &format!("FRAME-V{slot}")])
                        .output();
                }
                unsafe {
                    drop(Box::from_raw(self.user_ptr));
                }
            }
        }

        let _ = std::process::Command::new("hyprctl")
            .args(["output", "remove", "FRAME-V1"])
            .output();
        gstreamer::init().expect("gstreamer init");

        let events: Box<Mutex<Events>> = Box::default();
        let user_ptr = Box::into_raw(events);
        let output = Output::from_callbacks(frame_abi::frame_host_callbacks {
            user: user_ptr.cast(),
            surface: Some(on_surface),
            surface_region: Some(on_region),
            surface_removed: Some(on_removed),
            video: Some(on_video),
            message: Some(on_message),
        });
        let shared = Arc::new(Shared::default());
        let (manager, handle) = VirtualManager::start(output.clone(), shared.clone());
        let mut guard = CleanupGuard {
            output: Some(output.clone()),
            manager: Some(manager),
            handle: Some(handle),
            user_ptr,
        };
        let manager_ref = guard.manager.as_ref().unwrap();
        assert!(
            manager_ref.capabilities().supported,
            "{:?}",
            manager_ref.capabilities()
        );

        let events_ref = unsafe { &*user_ptr };
        manager_ref.send(Command::Create {
            source: VirtualTargetKind::Blank {
                width: 1280,
                height: 720,
            },
            size: [1280, 720],
        });

        let deadline = Instant::now() + Duration::from_secs(6);
        let mut created_surface = None;
        while Instant::now() < deadline {
            let guard = events_ref.lock().unwrap();
            let panel_surface = guard.messages.iter().rev().find_map(|msg| match msg {
                HostMessage::VirtualState(s) => s.panels.first().map(|p| p.surface),
                _ => None,
            });
            if let Some(id) = panel_surface
                && guard.surfaces.contains(&id)
                && guard.videos > 0
            {
                created_surface = Some(id);
                break;
            }
            drop(guard);
            std::thread::sleep(Duration::from_millis(50));
        }
        let surface_id = created_surface.expect("virtual panel created and streamed");

        manager_ref.send(Command::SyncLayout {
            placements: vec![PanelLayoutTarget {
                surface: surface_id,
                x: 0,
                y: -720,
            }],
        });

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut moved = false;
        while Instant::now() < deadline {
            let guard = events_ref.lock().unwrap();
            if guard.messages.iter().rev().any(|msg| match msg {
                HostMessage::VirtualState(s) => s
                    .panels
                    .iter()
                    .any(|p| p.surface == surface_id && p.logical_pos == [0, -720]),
                _ => false,
            }) {
                moved = true;
                break;
            }
            drop(guard);
            std::thread::sleep(Duration::from_millis(30));
        }
        assert!(moved, "expected SyncLayout to reposition FRAME-V1 to (0, -720)");

        manager_ref.send(Command::Close {
            surface: surface_id,
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut closed = false;
        while Instant::now() < deadline {
            let guard = events_ref.lock().unwrap();
            let gone_in_state = guard
                .messages
                .iter()
                .rev()
                .find_map(|msg| match msg {
                    HostMessage::VirtualState(s) => Some(s.panels.is_empty()),
                    _ => None,
                })
                .unwrap_or(false);
            if gone_in_state && guard.removed.contains(&surface_id) {
                closed = true;
                break;
            }
            drop(guard);
            std::thread::sleep(Duration::from_millis(30));
        }
        assert!(closed, "expected virtual panel close and removal");

        guard.output.take().unwrap().stop();
        guard.manager.take().unwrap().send(Command::Shutdown);
        guard.handle.take().unwrap().join().expect("join virtual worker");
        let (backend, _) = compositor::detect();
        if let Some(backend) = backend {
            let leftover: Vec<_> = backend
                .list_monitors()
                .unwrap()
                .into_iter()
                .filter(|m| capture::is_virtual_output_name(&m.name))
                .collect();
            assert!(leftover.is_empty(), "leftover virtual outputs: {leftover:?}");
        }
        drop(guard);
    }
}
