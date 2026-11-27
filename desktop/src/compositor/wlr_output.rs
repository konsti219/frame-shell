//! Live output configuration via `zwlr_output_manager_v1`.

use anyhow::{Context, Result};
use std::collections::HashMap;
use wlx_capture::wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
    backend::ObjectId,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_output::Transform, wl_registry::{self, WlRegistry}},
};
use wlx_capture::wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1,
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, AdaptiveSyncState, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};

#[derive(Debug, Clone)]
pub struct OutputConfigUpdate {
    pub name: String,
    pub mode: Option<(u32, u32, u32)>,
    pub pos: Option<(i32, i32)>,
    pub scale: Option<f32>,
}

pub fn supports_wlr_output_management() -> bool {
    let Ok(conn) = Connection::connect_to_env() else {
        return false;
    };
    let Ok((globals, _queue)) = registry_queue_init::<ProbeState>(&conn) else {
        return false;
    };
    globals
        .contents()
        .clone_list()
        .iter()
        .any(|g| g.interface == ZwlrOutputManagerV1::interface().name)
}

struct ProbeState;

impl Dispatch<WlRegistry, GlobalListContents> for ProbeState {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

#[derive(Clone)]
struct ModeInfo {
    proxy: ZwlrOutputModeV1,
    width: i32,
    height: i32,
    refresh_mhz: i32,
    finished: bool,
}

struct HeadInfo {
    proxy: ZwlrOutputHeadV1,
    name: String,
    enabled: bool,
    position: (i32, i32),
    scale: f64,
    transform: Transform,
    current_mode: Option<ObjectId>,
    modes: Vec<ObjectId>,
    finished: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyOutcome {
    Pending,
    Succeeded,
    Failed,
    Cancelled,
}

struct WlrState {
    heads: HashMap<ObjectId, HeadInfo>,
    modes: HashMap<ObjectId, ModeInfo>,
    serial: Option<u32>,
    outcome: ApplyOutcome,
}

impl WlrState {
    fn new() -> Self {
        Self {
            heads: HashMap::new(),
            modes: HashMap::new(),
            serial: None,
            outcome: ApplyOutcome::Pending,
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for WlrState {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for WlrState {
    fn event(
        state: &mut Self,
        _proxy: &ZwlrOutputManagerV1,
        event: zwlr_output_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_output_manager_v1::Event::Head { head } => {
                let id = head.id();
                state.heads.insert(
                    id,
                    HeadInfo {
                        proxy: head,
                        name: String::new(),
                        enabled: false,
                        position: (0, 0),
                        scale: 1.0,
                        transform: Transform::Normal,
                        current_mode: None,
                        modes: Vec::new(),
                        finished: false,
                    },
                );
            }
            zwlr_output_manager_v1::Event::Done { serial } => {
                state.serial = Some(serial);
            }
            _ => {}
        }
    }

    wlx_capture::wayland_client::event_created_child!(WlrState, ZwlrOutputManagerV1, [
        zwlr_output_manager_v1::EVT_HEAD_OPCODE => (ZwlrOutputHeadV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputHeadV1, ()> for WlrState {
    fn event(
        state: &mut Self,
        proxy: &ZwlrOutputHeadV1,
        event: zwlr_output_head_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        let head_id = proxy.id();
        match event {
            zwlr_output_head_v1::Event::Name { name } => {
                if let Some(head) = state.heads.get_mut(&head_id) {
                    head.name = name;
                }
            }
            zwlr_output_head_v1::Event::Mode { mode } => {
                let mode_id = mode.id();
                state.modes.insert(
                    mode_id.clone(),
                    ModeInfo {
                        proxy: mode,
                        width: 0,
                        height: 0,
                        refresh_mhz: 0,
                        finished: false,
                    },
                );
                if let Some(head) = state.heads.get_mut(&head_id) {
                    head.modes.push(mode_id);
                }
            }
            zwlr_output_head_v1::Event::Enabled { enabled } => {
                if let Some(head) = state.heads.get_mut(&head_id) {
                    head.enabled = enabled != 0;
                }
            }
            zwlr_output_head_v1::Event::CurrentMode { mode } => {
                if let Some(head) = state.heads.get_mut(&head_id) {
                    head.current_mode = Some(mode.id());
                }
            }
            zwlr_output_head_v1::Event::Position { x, y } => {
                if let Some(head) = state.heads.get_mut(&head_id) {
                    head.position = (x, y);
                }
            }
            zwlr_output_head_v1::Event::Transform {
                transform: WEnum::Value(transform),
            } => {
                if let Some(head) = state.heads.get_mut(&head_id) {
                    head.transform = transform;
                }
            }
            zwlr_output_head_v1::Event::Scale { scale } => {
                if let Some(head) = state.heads.get_mut(&head_id) {
                    head.scale = scale;
                }
            }
            zwlr_output_head_v1::Event::Finished => {
                if let Some(head) = state.heads.get_mut(&head_id) {
                    head.finished = true;
                }
            }
            zwlr_output_head_v1::Event::AdaptiveSync {
                state: WEnum::Value(AdaptiveSyncState::Disabled | AdaptiveSyncState::Enabled),
            }
            | _ => {}
        }
    }

    wlx_capture::wayland_client::event_created_child!(WlrState, ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ZwlrOutputModeV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputModeV1, ()> for WlrState {
    fn event(
        state: &mut Self,
        proxy: &ZwlrOutputModeV1,
        event: zwlr_output_mode_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        let Some(mode) = state.modes.get_mut(&proxy.id()) else {
            return;
        };
        match event {
            zwlr_output_mode_v1::Event::Size { width, height } => {
                mode.width = width;
                mode.height = height;
            }
            zwlr_output_mode_v1::Event::Refresh { refresh } => {
                mode.refresh_mhz = refresh;
            }
            zwlr_output_mode_v1::Event::Finished => {
                mode.finished = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, ()> for WlrState {
    fn event(
        state: &mut Self,
        _proxy: &ZwlrOutputConfigurationV1,
        event: zwlr_output_configuration_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        state.outcome = match event {
            zwlr_output_configuration_v1::Event::Succeeded => ApplyOutcome::Succeeded,
            zwlr_output_configuration_v1::Event::Failed => ApplyOutcome::Failed,
            zwlr_output_configuration_v1::Event::Cancelled => ApplyOutcome::Cancelled,
            _ => state.outcome,
        };
    }
}

impl Dispatch<ZwlrOutputConfigurationHeadV1, ()> for WlrState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrOutputConfigurationHeadV1,
        _event: <ZwlrOutputConfigurationHeadV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

fn dispatch_until(
    queue: &mut EventQueue<WlrState>,
    state: &mut WlrState,
    mut done: impl FnMut(&WlrState) -> bool,
) -> Result<()> {
    while !done(state) {
        queue.blocking_dispatch(state)?;
    }
    Ok(())
}

/// Atomically applies `updates` across all active compositor outputs, pinning unchanged outputs
/// at their current position, scale, and mode so auto-positioned physical monitors stay anchored.
pub fn apply_output_layout(updates: &[OutputConfigUpdate]) -> Result<()> {
    let conn = Connection::connect_to_env().context("Connect to Wayland for output layout")?;
    let (globals, mut queue) = registry_queue_init::<WlrState>(&conn)?;
    let qh = queue.handle();
    let mgr: ZwlrOutputManagerV1 = globals
        .bind(&qh, 1..=4, ())
        .context("Bind zwlr_output_manager_v1")?;

    let mut state = WlrState::new();
    dispatch_until(&mut queue, &mut state, |s| s.serial.is_some())?;

    let mut result = Err(anyhow::anyhow!("Output configuration retries exhausted"));
    for _attempt in 0..4 {
        let serial = state.serial.context("Missing zwlr_output_manager serial")?;
        state.outcome = ApplyOutcome::Pending;

        // Ensure every target head is currently present
        let missing: Vec<&str> = updates
            .iter()
            .filter(|u| {
                !state
                    .heads
                    .values()
                    .any(|h| !h.finished && h.name == u.name)
            })
            .map(|u| u.name.as_str())
            .collect();
        if !missing.is_empty() {
            result = Err(anyhow::anyhow!(
                "Outputs not advertised by zwlr_output_manager_v1: {missing:?}"
            ));
            break;
        }

        let config = mgr.create_configuration(serial, &qh, ());
        let mut head_ids: Vec<ObjectId> = state
            .heads
            .iter()
            .filter(|(_, h)| !h.finished)
            .map(|(id, _)| id.clone())
            .collect();
        head_ids.sort_by(|a, b| state.heads[a].name.cmp(&state.heads[b].name));

        for head_id in head_ids {
            let head = &state.heads[&head_id];
            let update = updates.iter().find(|u| u.name == head.name);
            if !head.enabled && update.is_none() {
                config.disable_head(&head.proxy);
                continue;
            }
            let conf_head = config.enable_head(&head.proxy, &qh, ());
            if let Some(update) = update {
                if let Some((w, h, hz)) = update.mode {
                    let matching_mode = head.modes.iter().find_map(|mid| {
                        let m = state.modes.get(mid)?;
                        if !m.finished
                            && m.width == w as i32
                            && m.height == h as i32
                            && (hz == 0 || (m.refresh_mhz / 1000 - hz as i32).abs() <= 1)
                        {
                            Some(&m.proxy)
                        } else {
                            None
                        }
                    });
                    if let Some(mode_proxy) = matching_mode {
                        conf_head.set_mode(mode_proxy);
                    } else {
                        conf_head.set_custom_mode(w as i32, h as i32, (hz * 1000) as i32);
                    }
                } else if let Some(mid) = &head.current_mode
                    && let Some(mode) = state.modes.get(mid)
                {
                    conf_head.set_mode(&mode.proxy);
                }
                let pos = update.pos.unwrap_or(head.position);
                conf_head.set_position(pos.0, pos.1);
                let scale = update.scale.map(f64::from).unwrap_or(head.scale);
                conf_head.set_scale(scale);
                conf_head.set_transform(head.transform);
            } else {
                // Explicitly re-pin physical monitors so compositor `position = "auto"` stays fixed.
                if let Some(mid) = &head.current_mode
                    && let Some(mode) = state.modes.get(mid)
                {
                    conf_head.set_mode(&mode.proxy);
                }
                conf_head.set_position(head.position.0, head.position.1);
                conf_head.set_scale(head.scale);
                conf_head.set_transform(head.transform);
            }
        }

        config.apply();
        let _ = conn.flush();
        dispatch_until(&mut queue, &mut state, |s| s.outcome != ApplyOutcome::Pending)?;
        config.destroy();

        match state.outcome {
            ApplyOutcome::Succeeded => {
                result = Ok(());
                break;
            }
            ApplyOutcome::Failed => {
                result = Err(anyhow::anyhow!(
                    "Compositor rejected zwlr_output_configuration_v1"
                ));
                break;
            }
            ApplyOutcome::Cancelled => {
                // Pump events to receive updated `Done { serial }` before retrying.
                queue.roundtrip(&mut state)?;
                continue;
            }
            ApplyOutcome::Pending => unreachable!(),
        }
    }

    mgr.stop();
    let _ = conn.roundtrip();
    result
}
