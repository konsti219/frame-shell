use crate::{
    backend::{
        input::{HoverResult, PointerHit},
        task::{OverlayTask, SpawnPos, TaskType},
    },
    gui::panel::{GuiPanel, NewGuiPanelParams},
    overlays::screen::{
        backend::{screen_hover, screen_pointer, screen_scroll},
        capture::Nv12ScreenPipeline,
    },
    state::AppState,
    subsystem::{
        hid::{MODS_TO_KEYS, VirtualKey, WheelDelta, XkbKeymap, provider::HidProvider},
        input::InputFocus,
    },
    windowing::{
        OverlaySelector,
        backend::{
            FrameMeta, OverlayBackend, OverlayEventData, RenderResources, ShouldRender,
            ui_transform,
        },
        window::{OverlayCategory, OverlayWindowConfig},
    },
};
use frame_abi::{
    FRAME_SHELL_ABI_VERSION, frame_shell_api, frame_shell_desktop_frame as DesktopFrame,
    frame_shell_desktop_surface as DesktopSurface, frame_shell_host,
    frame_shell_input as ShellInput,
};
use glam::{Affine2, DVec2};
use std::sync::Arc;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    os::fd::{AsRawFd, BorrowedFd, OwnedFd},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use wgui::gfx::{DmabufImportInfo, DmabufPlaneLayout, Format, ImageView};
use wlx_common::{
    overlays::{BackendAttrib, BackendAttribValue},
    windowing::OverlayWindowState,
};

pub static EMBEDDED: AtomicBool = AtomicBool::new(false);
/// The WiVRn client's callbacks, from `run`
static HOST: Mutex<Option<Host>> = Mutex::new(None);
static CONNECTED: AtomicBool = AtomicBool::new(false);
static INPUT: Mutex<VecDeque<ShellInput>> = Mutex::new(VecDeque::new());
/// PC monitors announced by the client, keyed by `surface_key`
static SURFACES: Mutex<BTreeMap<u64, Surface>> = Mutex::new(BTreeMap::new());
/// Counts `openxr_run`s, each of which starts with no desktop overlays
static RUN: AtomicU64 = AtomicU64::new(0);
/// Surface the PC pointer was last moved on
static POINTER_SURFACE: AtomicU64 = AtomicU64::new(0);
/// The WiVRn client's lobby (servers, connection) and stream (PC apps) state as JSON, with a change counter
pub static LOBBY_STATE: Mutex<(u64, String, String)> =
    Mutex::new((0, String::new(), String::new()));
/// PCs connected besides the one presenting VR (the client's hosts_state), as JSON
static HOSTS: Mutex<String> = Mutex::new(String::new());
/// Commands for the WiVRn client (`client-shell.h` lists them)
static COMMANDS: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
/// PIN digits typed on the PC panel so far
pub static PIN: Mutex<String> = Mutex::new(String::new());
/// Requests for the dashboard (power menu, pairing) and whether to bring it up for them
static POWER_MENU_REQUEST: AtomicBool = AtomicBool::new(false);
static PAIRING_REQUEST: AtomicBool = AtomicBool::new(false);
static SHOW_DASHBOARD: AtomicBool = AtomicBool::new(false);
/// An application to start on a PC once connected to it: (host, app id, name)
static PENDING_LAUNCH: Mutex<Option<(String, String, String)>> = Mutex::new(None);
/// Power key: milliseconds since `START` when pressed (0 = up), and whether the hold was handled
static POWER_KEY_DOWN_MS: AtomicU64 = AtomicU64::new(0);
static POWER_KEY_HELD: AtomicBool = AtomicBool::new(false);
/// Volume (percent + 1, 0 = none) to show after a volume key press
static VOLUME_TOAST: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static START: std::sync::LazyLock<std::time::Instant> =
    std::sync::LazyLock::new(std::time::Instant::now);
/// Floor grid: pinned from the watch, or shown until this time (ms since START) after a playspace change
/// When an overlay was last let go of (ms since START, 0 = saved), to save the layout shortly after
static LAYOUT_CHANGED_MS: AtomicU64 = AtomicU64::new(0);
static FLOOR_PINNED: AtomicBool = AtomicBool::new(false);
static FLOOR_UNTIL_MS: AtomicU64 = AtomicU64::new(0);
static FLOOR_SPAWNED: AtomicBool = AtomicBool::new(false);
static FLOOR_SAMPLES_SPAWNED: AtomicBool = AtomicBool::new(false);
static FLOOR_SAMPLES_NAME: std::sync::LazyLock<Arc<str>> =
    std::sync::LazyLock::new(|| "frame-floor-samples".into());
static FLOOR_NAME: std::sync::LazyLock<Arc<str>> =
    std::sync::LazyLock::new(|| "frame-floor".into());

static DRAG_OFFSET: Mutex<[f32; 7]> = Mutex::new([0., 0., 0., 1., 0., 0., 0.]);

/// The space drag (floor-from-stage) the WiVRn client takes out of its poses and sends to the server
pub fn update_drag_offset(transform: glam::Affine3A) {
    let q = glam::Quat::from_affine3(&transform).normalize();
    let p = transform.translation;
    *DRAG_OFFSET
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = [q.x, q.y, q.z, q.w, p.x, p.y, p.z];
}

extern "C" fn drag_offset(output: *mut f32) {
    if output.is_null() {
        return;
    }
    let offset = *DRAG_OFFSET
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    unsafe { std::ptr::copy_nonoverlapping(offset.as_ptr(), output, offset.len()) };
}

struct Host(frame_shell_host);
// The client's callbacks may be called from any thread
unsafe impl Send for Host {}

fn host() -> std::sync::MutexGuard<'static, Option<Host>> {
    HOST.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Gives a desktop frame's decoder buffer back to the client
fn release_frame(id: u64) {
    if let Some(Host(host)) = host().as_ref()
        && let Some(release) = host.release_desktop_frame
    {
        unsafe { release(host.user, id) };
    }
}

/// A wayvr_ipc::frame::ShellMessage for a connected PC (use frame_host::send)
pub fn send_host_message(source: u32, data: &[u8]) -> bool {
    if let Some(Host(host)) = host().as_ref()
        && let Some(send) = host.send_host_message
    {
        unsafe { send(host.user, source, data.as_ptr(), data.len()) };
        return true;
    }
    false
}

struct Surface {
    visible: Option<bool>,
    name: Arc<str>,
    /// Latest decoded frame from the client's V4L2 decoder, with its own reference to the DMA-buf
    frame: Option<(DesktopFrame, OwnedFd)>,
    /// `RUN` the overlay was spawned in
    spawned: u64,
}

const fn surface_key(source: u32, surface: u32) -> u64 {
    ((source as u64) << 32) | surface as u64
}

fn surfaces() -> std::sync::MutexGuard<'static, BTreeMap<u64, Surface>> {
    SURFACES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn enqueue(event: ShellInput) {
    if !crate::frame_host::is_connected(event.source) {
        return;
    }
    let mut queue = INPUT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if event.kind == 2 && queue.back().is_some_and(|last| last.kind == 2) {
        queue.pop_back();
    }
    if queue.len() < 4096 {
        queue.push_back(event);
    }
}

extern "C" fn input_poll(event: *mut ShellInput) -> bool {
    if event.is_null() {
        return false;
    }
    if let Some(next) = INPUT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .pop_front()
    {
        unsafe {
            event.write(next);
        }
        true
    } else {
        false
    }
}

fn c_str(text: *const std::ffi::c_char) -> String {
    if text.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}

extern "C" fn lobby_state(json: *const std::ffi::c_char) {
    let json = c_str(json);
    // A PC asking for its PIN opens the dashboard on the WiVRn tab
    let pairing = json.contains("\"state\":\"pin\"");
    let mut state = LOBBY_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if pairing && !state.1.contains("\"state\":\"pin\"") {
        PAIRING_REQUEST.store(true, Ordering::Relaxed);
        SHOW_DASHBOARD.store(true, Ordering::Relaxed);
    }
    state.0 += 1;
    state.1 = json;
}

extern "C" fn stream_state(json: *const std::ffi::c_char) {
    let json = c_str(json);
    // The PC encodes its desktops with the headset's settings
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&json)
        && let Some(settings) = value.get("settings")
        && let Some(bitrate) = settings
            .get("desktop_bitrate")
            .and_then(serde_json::Value::as_u64)
    {
        crate::frame_host::sync_desktop_settings(wayvr_ipc::frame::DesktopSettings {
            bitrate_per_1080p: bitrate as u32,
            hevc: settings
                .get("desktop_codec")
                .and_then(serde_json::Value::as_str)
                == Some("h265"),
        });
    }
    let mut state = LOBBY_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.0 += 1;
    state.2 = json;
}

extern "C" fn command_poll(buffer: *mut std::ffi::c_char, size: usize) -> bool {
    if buffer.is_null() || size == 0 {
        return false;
    }
    let Some(command) = COMMANDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .pop_front()
    else {
        return false;
    };
    let bytes = &command.as_bytes()[..command.len().min(size - 1)];
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr().cast(), buffer, bytes.len());
        *buffer.add(bytes.len()) = 0;
    }
    true
}

/// `RUN` in which the remembered screens were spawned
static SCREENS_SPAWNED_RUN: AtomicU64 = AtomicU64::new(0);

fn screens_file() -> std::path::PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config"))
        })
        .unwrap_or_default();
    config.join("wayvr/frame-shell-screens")
}

/// PC screens seen in earlier sessions, one overlay name per line, sorted like the PC lists its outputs
fn remembered_screens() -> Vec<String> {
    let mut screens: Vec<String> = std::fs::read_to_string(screens_file())
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    screens.sort_by_key(|name| {
        let digits = name.len() - name.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        let (prefix, number) = name.split_at(name.len() - digits);
        (prefix.to_owned(), number.parse::<u64>().unwrap_or(0))
    });
    screens
}

fn remember_screen(name: &str) {
    let mut screens = remembered_screens();
    if screens.iter().any(|screen| screen == name) {
        return;
    }
    screens.push(name.to_owned());
    let path = screens_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(error) = std::fs::write(&path, screens.join("\n") + "\n") {
        log::warn!("{}: {error}", path.display());
    }
}

/// A short text toast, the shell's feedback for quick actions
pub fn toast(app: &mut AppState, text: String) {
    crate::overlays::toast::Toast::new(
        wlx_common::overlays::ToastTopic::System,
        None,
        wgui::i18n::Translation::from_raw_text_string(text),
    )
    .with_timeout(1.5)
    .submit(app);
}

fn lobby_json() -> (serde_json::Value, serde_json::Value) {
    let state = LOBBY_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    (
        serde_json::from_str(&state.1).unwrap_or(serde_json::Value::Null),
        serde_json::from_str(&state.2).unwrap_or(serde_json::Value::Null),
    )
}

/// The WiVRn state for the dashboard, with the PIN pad's progress added
pub fn dashboard_state() -> Option<(u64, String, String)> {
    if !EMBEDDED.load(Ordering::Relaxed) {
        return None;
    }
    let typed = PIN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .len();
    let (version, lobby, stream) = LOBBY_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let mut lobby: serde_json::Value =
        serde_json::from_str(&lobby).unwrap_or_else(|_| serde_json::json!({}));
    lobby["pin_typed"] = typed.into();
    lobby["hosts"] = hosts();
    let mut stream: serde_json::Value =
        serde_json::from_str(&stream).unwrap_or_else(|_| serde_json::json!({}));
    stream["steam_running"] = crate::frame_host::vr_source()
        .and_then(crate::frame_host::games)
        .unwrap_or_default()
        .into();
    Some((version, lobby.to_string(), stream.to_string()))
}

/// Which PC (WiVRn server name) the stream is up with, if any
fn connected_host() -> Option<String> {
    let (lobby, stream) = lobby_json();
    (lobby.get("state").and_then(serde_json::Value::as_str) == Some("connected"))
        .then(|| {
            stream
                .get("server")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .flatten()
}

/// Starts an application on a PC; connects first (and starts it once connected) when needed
/// Starts an application on a connected PC; one on standby then presents VR
fn start_app_on(source: u32, app_id: &str) {
    send_command(&format!("on {source} start_app {app_id}"));
}

pub fn launch_remote(app: &mut AppState, host: &str, app_id: &str, name: &str) {
    if let Some(source) = source_for_host(host) {
        start_app_on(source, app_id);
        toast(app, format!("Starting {name} on {host}"));
        return;
    }
    let (lobby, _) = lobby_json();
    let server = lobby
        .get("servers")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .find(|server| server.get("name").and_then(serde_json::Value::as_str) == Some(host));
    let Some(cookie) = server
        .and_then(|server| server.get("cookie"))
        .and_then(serde_json::Value::as_str)
    else {
        toast(app, format!("{host} is not a known PC"));
        return;
    };
    // Connected besides a PC already presenting VR, it presents once the application runs
    send_command(&format!("connect {cookie}"));
    *PENDING_LAUNCH
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some((host.to_owned(), app_id.to_owned(), name.to_owned()));
    toast(app, format!("Connecting to {host} to start {name}"));
}

/// A known WiVRn server by its service name or the first label of its host name: (name, cookie)
fn find_server(lobby: &serde_json::Value, host: &str) -> Option<(String, String)> {
    let field = |server: &serde_json::Value, key: &str| {
        server
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let servers = lobby.get("servers").and_then(serde_json::Value::as_array)?;
    let server = servers
        .iter()
        .find(|server| field(server, "name") == host)
        .or_else(|| {
            servers.iter().find(|server| {
                field(server, "hostname")
                    .split('.')
                    .next()
                    .is_some_and(|short| short.eq_ignore_ascii_case(host))
            })
        })?;
    Some((field(server, "name"), field(server, "cookie")))
}

/// Connects to the PC a screen belongs to unless it's already connecting or connected
pub fn connect_screen_host(app: &mut AppState, screen: &str) {
    let host = screen.rsplit_once(' ').map_or(screen, |(host, _)| host);
    let (lobby, _) = lobby_json();
    let Some((name, _)) = find_server(&lobby, host) else {
        toast(app, format!("{host} is not a known PC"));
        return;
    };
    let state = lobby
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let server = lobby
        .get("server")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if server == name && matches!(state, "connected" | "connecting" | "pin") {
        toast(app, format!("{screen}: waiting for its picture"));
        return;
    }
    connect_host(app, host, true);
}

/// Connects to the PC a screen ("<PC host name> <output>") belongs to, or disconnects if it's the one streaming
pub fn toggle_screen_host(app: &mut AppState, screen: &str) {
    let host = screen.rsplit_once(' ').map_or(screen, |(host, _)| host);
    let (lobby, _) = lobby_json();
    let Some((name, _)) = find_server(&lobby, host) else {
        toast(app, format!("{host} is not a known PC"));
        return;
    };
    let active = lobby.get("server").and_then(serde_json::Value::as_str) == Some(name.as_str())
        && matches!(
            lobby.get("state").and_then(serde_json::Value::as_str),
            Some("connected" | "connecting" | "pin")
        );
    connect_host(app, host, !active);
}

/// Connects to (or disconnects from) a PC by service or host name
pub fn connect_host(app: &mut AppState, host: &str, connect: bool) {
    if !connect {
        send_command("disconnect");
        toast(app, format!("Disconnecting from {host}"));
        return;
    }
    let (lobby, _) = lobby_json();
    match find_server(&lobby, host) {
        Some((name, cookie)) => {
            send_command(&format!("connect {cookie}"));
            toast(app, format!("Connecting to {name}"));
        }
        None => toast(app, format!("{host} is not a known PC")),
    }
}

/// The shell is stopped without a chance to save on exit, so the layout is saved after every move
pub fn layout_changed() {
    LAYOUT_CHANGED_MS.store(START.elapsed().as_millis().max(1) as u64, Ordering::Relaxed);
}

pub fn floor_visible() -> bool {
    FLOOR_PINNED.load(Ordering::Relaxed)
}

pub fn set_floor_visible(visible: bool) {
    FLOOR_PINNED.store(visible, Ordering::Relaxed);
}

pub fn floor_debug_visible(app: &AppState) -> bool {
    floor_visible() || app.floor_calibration.remaining_seconds().is_some()
}

/// Shows the grid without enabling debug dots.
pub fn show_floor() {
    FLOOR_UNTIL_MS.store(START.elapsed().as_millis() as u64 + 5000, Ordering::Relaxed);
}

fn floor_overlay(_: &mut AppState) -> Option<OverlayWindowConfig> {
    floor_layer(false)
}
fn floor_samples_overlay(_: &mut AppState) -> Option<OverlayWindowConfig> {
    floor_layer(true)
}

fn floor_layer(samples: bool) -> Option<OverlayWindowConfig> {
    Some(OverlayWindowConfig {
        name: if samples {
            FLOOR_SAMPLES_NAME.clone()
        } else {
            FLOOR_NAME.clone()
        },
        default_state: OverlayWindowState {
            interactable: false,
            grabbable: false,
            positioning: wlx_common::windowing::Positioning::Static,
            ..OverlayWindowState::default()
        },
        global: true,
        ..OverlayWindowConfig::from_backend(Box::new(if samples {
            crate::overlays::floor::FloorGrid::samples()
        } else {
            crate::overlays::floor::FloorGrid::default()
        }))
    })
}

/// Keeps the static grid and small dynamic sample texture aligned on the calibrated floor.
fn update_floor(app: &mut AppState) {
    let debug = floor_debug_visible(app);
    let grid =
        debug || (START.elapsed().as_millis() as u64) < FLOOR_UNTIL_MS.load(Ordering::Relaxed);
    let head = app
        .floor_from_stage
        .transform_point3a(app.input_state.hmd.translation);
    let transform = app.floor_from_stage.inverse()
        * glam::Affine3A::from_scale_rotation_translation(
            glam::Vec3::splat(8.0),
            glam::Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
            glam::vec3(
                (head.x / 0.5).round() * 0.5,
                0.0,
                (head.z / 0.5).round() * 0.5,
            ),
        );
    type Factory = fn(&mut AppState) -> Option<OverlayWindowConfig>;
    for (name, spawned, want, factory) in [
        (
            FLOOR_NAME.clone(),
            &FLOOR_SPAWNED,
            grid,
            floor_overlay as Factory,
        ),
        (
            FLOOR_SAMPLES_NAME.clone(),
            &FLOOR_SAMPLES_SPAWNED,
            debug,
            floor_samples_overlay as Factory,
        ),
    ] {
        if !want {
            if spawned.swap(false, Ordering::Relaxed) {
                app.tasks
                    .enqueue(TaskType::Overlay(OverlayTask::Drop(OverlaySelector::Name(
                        name,
                    ))));
            }
            continue;
        }
        if !spawned.swap(true, Ordering::Relaxed) {
            app.tasks.enqueue(TaskType::Overlay(OverlayTask::Spawn(
                OverlaySelector::Name(name.clone()),
                SpawnPos::FixedNoRealign,
                Box::new(factory),
            )));
        }
        app.tasks.enqueue(TaskType::Overlay(OverlayTask::Modify(
            OverlaySelector::Name(name),
            Box::new(move |app, config| {
                if config.active_state.is_none() {
                    config.activate(app, false);
                }
                if let Some(state) = config.active_state.as_mut() {
                    state.transform = transform;
                    config.dirty = true;
                }
            }),
        )));
    }
}

pub fn power_action(action: &str) {
    // frame-sleep stops the VR stack around the suspend (and so this process): run it as its own unit
    let (program, args): (&str, &[&str]) = match action {
        "sleep" => (
            "systemd-run",
            &[
                "--user",
                "--collect",
                "--unit=frame-sleep",
                "--",
                "/bin/sh",
                "-c",
                "exec \"$HOME/.local/bin/frame-sleep\"",
            ],
        ),
        "restart_stack" => (
            "systemctl",
            &[
                "--user",
                "--no-block",
                "restart",
                "monado.service",
                "frame-shell.service",
            ],
        ),
        "restart" => ("systemctl", &["reboot"]),
        "poweroff" => ("systemctl", &["poweroff"]),
        "steamvr" => ("steamosctl", &["switch-to-game-mode"]),
        _ => return,
    };
    if let Err(error) = std::process::Command::new(program).args(args).spawn() {
        log::error!("{program} {args:?}: {error}");
    }
}

/// The next thing the dashboard should show for the shell
pub fn take_shell_request() -> Option<wlx_common::dash_interface::ShellRequest> {
    use wlx_common::dash_interface::ShellRequest;
    if POWER_MENU_REQUEST.swap(false, Ordering::Relaxed) {
        Some(ShellRequest::PowerMenu)
    } else if PAIRING_REQUEST.swap(false, Ordering::Relaxed) {
        Some(ShellRequest::Pairing)
    } else {
        None
    }
}

/// Steps the headset output volume and queues the toast showing it
fn step_volume(up: bool) {
    crate::headset_audio::step_output(up);
}

pub(crate) fn volume_feedback(percent: u32) {
    VOLUME_TOAST.store(percent + 1, Ordering::Relaxed);
}

/// Reads the headset's buttons: power (`pmic_pwrkey`; short press sleeps, held 1 s opens the power menu like
/// SteamOS) and volume (`pmic_resin` down, `gpio-keys` up; repeats while held)
fn watch_keys() {
    let Ok(entries) = std::fs::read_dir("/sys/class/input") else {
        return;
    };
    for entry in entries.flatten() {
        let file = entry.file_name().to_string_lossy().into_owned();
        let Ok(name) = std::fs::read_to_string(entry.path().join("device/name")) else {
            continue;
        };
        if file.starts_with("event")
            && matches!(name.trim(), "pmic_pwrkey" | "pmic_resin" | "gpio-keys")
        {
            watch_key_device(format!("/dev/input/{file}"));
        }
    }
}

fn watch_key_device(path: String) {
    std::thread::spawn(move || {
        use std::io::Read;
        let mut device = match std::fs::File::open(&path) {
            Ok(device) => device,
            Err(error) => {
                log::warn!("Power key {path}: {error}");
                return;
            }
        };
        // struct input_event on 64-bit: timeval (16), type (2), code (2), value (4)
        let mut event = [0u8; 24];
        while device.read_exact(&mut event).is_ok() {
            let kind = u16::from_ne_bytes([event[16], event[17]]);
            let code = u16::from_ne_bytes([event[18], event[19]]);
            let value = i32::from_ne_bytes([event[20], event[21], event[22], event[23]]);
            const EV_KEY: u16 = 1;
            const KEY_VOLUMEDOWN: u16 = 114;
            const KEY_VOLUMEUP: u16 = 115;
            const KEY_POWER: u16 = 116;
            if kind != EV_KEY {
                continue;
            }
            if matches!(code, KEY_VOLUMEDOWN | KEY_VOLUMEUP) {
                // Press and auto-repeat
                if value != 0 {
                    step_volume(code == KEY_VOLUMEUP);
                }
                continue;
            }
            if code != KEY_POWER {
                continue;
            }
            if value == 1 {
                POWER_KEY_HELD.store(false, Ordering::Relaxed);
                POWER_KEY_DOWN_MS
                    .store(START.elapsed().as_millis().max(1) as u64, Ordering::Relaxed);
            } else if value == 0 {
                // A release without a press seen here (the shell started while the key was down) does nothing
                let pressed = POWER_KEY_DOWN_MS.swap(0, Ordering::Relaxed) != 0;
                if !POWER_KEY_HELD.swap(false, Ordering::Relaxed) && pressed {
                    power_action("sleep");
                }
            }
        }
    });
}

/// Queues a command line for the WiVRn client; PIN pad keys are collected here into one `pin` command.
pub fn send_command(line: &str) {
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

extern "C" fn connected(source: u32, connected: bool, vr: bool) {
    if connected {
        crate::frame_host::connected(source, vr);
        CONNECTED.store(crate::frame_host::vr_source().is_some(), Ordering::Relaxed);
        crate::frame_host::resync_desktop_settings();
        bump_lobby_state();
        return;
    }
    INPUT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|event| event.source != source);
    let mut released = Vec::new();
    surfaces().retain(|&key, surface| {
        if (key >> 32) as u32 != source {
            return true;
        }
        if let Some((frame, _)) = surface.frame.take() {
            released.push(frame.id);
        }
        false
    });
    for id in released {
        release_frame(id);
    }
    crate::frame_host::disconnected(source);
    CONNECTED.store(crate::frame_host::vr_source().is_some(), Ordering::Relaxed);
    let mut state = LOBBY_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.0 += 1;
    if vr {
        COMMANDS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|command| {
                !(command.starts_with("stop_app ") || command.starts_with("set_active_app "))
            });
        state.2.clear();
    }
}

extern "C" fn desktop_surface(info: *const DesktopSurface) {
    let Some(info) = (unsafe { info.as_ref() }) else {
        return;
    };
    let key = surface_key(info.source, info.surface);
    if info.removed {
        if let Some((frame, _)) = surfaces().remove(&key).and_then(|surface| surface.frame) {
            release_frame(frame.id);
        }
        return;
    }
    // Overlay names carry their saved layout, so name them after the PC and its monitor
    let text = frame_abi::c_string::<64>;
    let name: Arc<str> = format!("{} {}", text(&info.source_name), text(&info.name))
        .trim()
        .into();
    log::info!("Desktop {name}: {}x{}", info.width, info.height);
    remember_screen(&name);
    surfaces().entry(key).or_insert(Surface {
        visible: None,
        name,
        frame: None,
        spawned: 0,
    });
}

extern "C" fn desktop_frame(frame: *const DesktopFrame) {
    let Some(frame) = (unsafe { frame.as_ref() }) else {
        return;
    };
    // The fd is borrowed for this call: keep our own
    let Ok(fd) = unsafe { BorrowedFd::borrow_raw(frame.fd) }.try_clone_to_owned() else {
        release_frame(frame.id);
        return;
    };
    let key = surface_key(frame.source, frame.surface);
    // A newer frame replaces one that wasn't shown yet
    let replaced = match surfaces().get_mut(&key) {
        Some(surface) => surface.frame.replace((*frame, fd)).map(|(old, _)| old.id),
        None => Some(frame.id),
    };
    if let Some(id) = replaced {
        release_frame(id);
    }
}

/// Spawns an overlay for each announced monitor; vanished monitors keep theirs (blank) to keep its layout.
pub fn poll_desktop_visibility<T: Default>(
    overlays: &crate::windowing::manager::OverlayWindowManager<T>,
) {
    if !EMBEDDED.load(Ordering::Relaxed) {
        return;
    }
    for (&key, surface) in surfaces().iter_mut() {
        let visible = overlays.iter().any(|(_, overlay)| {
            overlay.config.name == surface.name
                && overlay
                    .config
                    .active_state
                    .as_ref()
                    .is_some_and(|state| state.alpha > 0.05)
        });
        if surface.visible != Some(visible) {
            surface.visible = Some(visible);
            crate::frame_host::send(
                (key >> 32) as u32,
                &wayvr_ipc::frame::ShellMessage::DesktopVisibility {
                    surface: key as u32,
                    visible,
                },
            );
        }
    }
}

pub fn poll_surfaces(app: &mut AppState) {
    if !EMBEDDED.load(Ordering::Relaxed) {
        return;
    }
    #[cfg(feature = "feat-monado-metrics")]
    {
        let frames = crate::frame_host::take_frames();
        if !CONNECTED.load(Ordering::Relaxed) {
            app.remote_metrics = None;
        } else if !frames.is_empty() {
            let (updated, metrics) = app
                .remote_metrics
                .get_or_insert_with(|| (std::time::Instant::now(), Default::default()));
            *updated = std::time::Instant::now();
            for frame in frames {
                metrics.ingest_remote(frame.into());
            }
        }
    }
    for error in crate::frame_host::take_errors() {
        toast(app, error);
    }
    for notification in crate::frame_host::take_notifications() {
        use wayvr_ipc::frame::NotificationSource;
        let topic = match notification.source {
            NotificationSource::Desktop => wlx_common::overlays::ToastTopic::DesktopNotification,
            NotificationSource::Overlay => wlx_common::overlays::ToastTopic::XSNotification,
        };
        let toast = crate::overlays::toast::Toast::new(
            topic,
            Some(wgui::i18n::Translation::from_raw_text_string(
                notification.title,
            )),
            wgui::i18n::Translation::from_raw_text_string(notification.body),
        )
        .with_timeout(notification.timeout_seconds)
        .with_sound(notification.sound);
        app.notifications.push(toast.build_raw());
    }
    if crate::frame_host::games_expired() {
        bump_lobby_state();
    }
    // Power key held for a second: power menu
    let down = POWER_KEY_DOWN_MS.load(Ordering::Relaxed);
    if down != 0
        && START.elapsed().as_millis() as u64 >= down + 1000
        && !POWER_KEY_HELD.swap(true, Ordering::Relaxed)
    {
        POWER_MENU_REQUEST.store(true, Ordering::Relaxed);
        SHOW_DASHBOARD.store(true, Ordering::Relaxed);
    }
    if let Some(percent) = VOLUME_TOAST.swap(0, Ordering::Relaxed).checked_sub(1) {
        toast(app, format!("Volume {percent} %"));
    }
    update_floor(app);
    let changed = LAYOUT_CHANGED_MS.load(Ordering::Relaxed);
    if changed != 0 && START.elapsed().as_millis() as u64 >= changed + 1000 {
        LAYOUT_CHANGED_MS.store(0, Ordering::Relaxed);
        app.tasks
            .enqueue(TaskType::Overlay(OverlayTask::SaveLayout));
    }
    if SHOW_DASHBOARD.swap(false, Ordering::Relaxed) {
        app.tasks
            .enqueue(TaskType::Overlay(OverlayTask::ShowDashboard));
    }
    // Start what was waiting for a connection
    let waiting = PENDING_LAUNCH
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .map(|(host, _, _)| host.clone());
    if let Some(waiting) = waiting
        && let Some(source) = source_for_host(&waiting)
        && let Some((host, app_id, name)) = PENDING_LAUNCH
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    {
        start_app_on(source, &app_id);
        toast(app, format!("Starting {name} on {host}"));
    }
    let run = RUN.load(Ordering::Relaxed);
    // Screens of earlier sessions come back as placeholders until their PC streams them again
    if SCREENS_SPAWNED_RUN.swap(run, Ordering::Relaxed) != run {
        for name in remembered_screens() {
            let name: Arc<str> = name.into();
            app.tasks.enqueue(TaskType::Overlay(OverlayTask::Spawn(
                OverlaySelector::Name(name.clone()),
                SpawnPos::Spread,
                Box::new(move |app| Some(desktop_overlay(name, 0, app))),
            )));
        }
    }
    for (&key, surface) in surfaces()
        .iter_mut()
        .filter(|(_, surface)| surface.spawned != run)
    {
        surface.spawned = run;
        let name = surface.name.clone();
        app.tasks.enqueue(TaskType::Overlay(OverlayTask::Spawn(
            OverlaySelector::Name(name.clone()),
            SpawnPos::Spread,
            Box::new(move |app| Some(desktop_overlay(name, key, app))),
        )));
    }
}

extern "C" fn stop() {
    crate::RUNNING.store(false, Ordering::Relaxed);
    disconnect_all();
}

/// The shell stops: every PC is gone for it
fn disconnect_all() {
    for (source, vr) in crate::frame_host::sources() {
        connected(source, false, vr);
    }
}

extern "C" fn run(callbacks: *const frame_shell_host) -> i32 {
    let Some(callbacks) = (unsafe { callbacks.as_ref() }) else {
        return 1;
    };
    *host() = Some(Host(*callbacks));
    match std::panic::catch_unwind(run_embedded) {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            eprintln!("Frame shell: {error:#}");
            1
        }
        Err(_) => {
            eprintln!("Frame shell panicked");
            2
        }
    }
}

static API: frame_shell_api = frame_shell_api {
    version: FRAME_SHELL_ABI_VERSION,
    run: Some(run),
    stop: Some(stop),
    input_poll: Some(input_poll),
    desktop_surface: Some(desktop_surface),
    desktop_frame: Some(desktop_frame),
    connected: Some(connected),
    lobby_state: Some(lobby_state),
    stream_state: Some(stream_state),
    host_message: Some(host_message),
    hosts_state: Some(hosts_state),
    command_poll: Some(command_poll),
    drag_offset: Some(drag_offset),
};

/// The shell's entry points for the WiVRn client (frame_abi.h)
#[unsafe(no_mangle)]
pub extern "C" fn frame_shell_api() -> *const frame_shell_api {
    &API
}
const _: frame_abi::frame_shell_api_fn = Some(frame_shell_api);

extern "C" fn host_message(source: u32, data: *const u8, size: usize) {
    if !data.is_null() {
        crate::frame_host::receive(source, unsafe { std::slice::from_raw_parts(data, size) });
    }
}

extern "C" fn hosts_state(json: *const std::ffi::c_char) {
    *HOSTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = c_str(json);
    bump_lobby_state();
}

fn run_embedded() -> anyhow::Result<()> {
    EMBEDDED.store(true, Ordering::Relaxed);
    watch_keys();
    let mut args = crate::Args {
        openxr: true,
        headless: true,
        show: true,
        no_audio: true,
        ..Default::default()
    };
    crate::logging_init(&mut args);
    while crate::RUNNING.load(Ordering::Relaxed) {
        crate::RESTART.store(false, Ordering::Relaxed);
        RUN.fetch_add(1, Ordering::Relaxed);
        use crate::backend::BackendError;
        match crate::backend::openxr::openxr_run(&args, crate::init_run_params(true)) {
            Ok(()) | Err(BackendError::Shutdown) => break,
            Err(BackendError::Restart)
            | Err(BackendError::OpenXrError(
                openxr::sys::Result::ERROR_INSTANCE_LOST | openxr::sys::Result::ERROR_SESSION_LOST,
            )) => (),
            Err(error) => return Err(error.into()),
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    disconnect_all();
    Ok(())
}

/// Forwards desktop input to the PC; pointer positions are normalized (0..1) on `POINTER_SURFACE`.
#[derive(Default)]
pub struct PcInput {
    modifiers: u8,
    held: BTreeSet<u16>,
    /// PC the pointer is on: buttons and keys go there
    source: u32,
}

impl PcInput {
    pub fn release(&mut self) {
        for code in std::mem::take(&mut self.held) {
            enqueue(ShellInput {
                kind: 0,
                code: code.into(),
                source: self.source,
                ..Default::default()
            });
        }
        self.modifiers = 0;
        enqueue(ShellInput {
            kind: 4,
            source: self.source,
            ..Default::default()
        });
    }
}

impl HidProvider for PcInput {
    fn mouse_move(&mut self, pos: DVec2) {
        let key = POINTER_SURFACE.load(Ordering::Relaxed);
        // Moving to another PC lets go of what is held on the previous one
        if (key >> 32) as u32 != self.source {
            PcInput::release(self);
            self.source = (key >> 32) as u32;
        }
        enqueue(ShellInput {
            kind: 2,
            x: pos.x.clamp(0., 1.) as f32,
            y: pos.y.clamp(0., 1.) as f32,
            source: (key >> 32) as u32,
            surface: key as u32,
            ..Default::default()
        });
    }
    fn mouse_move_rel(&mut self, _: DVec2) {}
    fn send_button(&mut self, button: u16, down: bool) {
        enqueue(ShellInput {
            kind: 1,
            code: button.saturating_sub(0x110).into(),
            x: f32::from(down),
            source: self.source,
            ..Default::default()
        });
    }
    fn wheel(&mut self, delta: WheelDelta) {
        enqueue(ShellInput {
            kind: 3,
            x: delta.x,
            y: delta.y,
            source: self.source,
            ..Default::default()
        });
    }
    fn set_desktop_extent(&mut self, _: DVec2) {}
    fn set_desktop_origin(&mut self, _: DVec2) {}
    fn set_modifiers(&mut self, mods: u8) {
        let changed = self.modifiers ^ mods;
        for bit in 0..8 {
            let mask = 1 << bit;
            if changed & mask != 0 {
                if let Some(key) = MODS_TO_KEYS.get(mask).and_then(|keys| keys.first()) {
                    self.send_key(*key, mods & mask != 0);
                }
            }
        }
        self.modifiers = mods;
    }
    fn send_key(&mut self, key: VirtualKey, down: bool) {
        let code = (key as u16).saturating_sub(8);
        if down {
            self.held.insert(code);
        } else {
            self.held.remove(&code);
        }
        enqueue(ShellInput {
            kind: 0,
            code: code.into(),
            x: f32::from(down),
            source: self.source,
            ..Default::default()
        });
    }
    fn set_keymap(&mut self, _: &XkbKeymap) {}
    fn commit(&mut self) {}
    fn release(&mut self) {
        PcInput::release(self);
    }
}

fn desktop_overlay(name: Arc<str>, key: u64, app: &AppState) -> OverlayWindowConfig {
    OverlayWindowConfig {
        name: name.clone(),
        category: OverlayCategory::Screen,
        show_on_spawn: true,
        input_focus: Some(InputFocus::PhysicalScreen),
        default_state: OverlayWindowState {
            positioning: app.session.config.default_positioning.into(),
            grabbable: true,
            interactable: true,
            transform: glam::Affine3A::from_scale_rotation_translation(
                glam::Vec3::splat(1.5),
                glam::Quat::IDENTITY,
                glam::vec3(0., 0., -1.),
            ),
            ..Default::default()
        },
        ..OverlayWindowConfig::from_backend(Box::new(DesktopOverlay {
            name: name.clone(),
            key,
            offline: None,
            meta: None,
            planes: None,
            shown: None,
            imports: HashMap::new(),
            renderer: None,
        }))
    }
}

struct DesktopOverlay {
    name: Arc<str>,
    key: u64,
    /// Shown while the PC isn't streaming this screen, in the monitor's aspect; a click connects
    offline: Option<GuiPanel<()>>,
    meta: Option<FrameMeta>,
    /// Luma and chroma views of the frame being shown
    planes: Option<(Arc<ImageView>, Arc<ImageView>)>,
    /// Decoder buffer of the frame being shown, released once a newer one is: WayVR waits for the previous
    /// frame's GPU work before rendering the next
    shown: Option<u64>,
    /// The decoder cycles through a fixed pool of DMA-bufs: import each once
    imports: HashMap<u64, (Arc<ImageView>, Arc<ImageView>)>,
    renderer: Option<Nv12ScreenPipeline>,
}

impl DesktopOverlay {
    fn offline_panel(&self, app: &mut AppState) -> anyhow::Result<GuiPanel<()>> {
        // Keep the overlay's shape: the monitor's last aspect, 16:9 before any frame
        let [width, height] = self.meta.map_or([1920, 1080], |meta| meta.extent);
        let panel_w = 1280u32;
        let panel_h = (u64::from(panel_w) * u64::from(height) / u64::from(width.max(1))) as u32;
        let mut panel = GuiPanel::new_from_template(
            app,
            "gui/frame-screen-offline.xml",
            (),
            NewGuiPanelParams {
                extra_vars: [
                    ("name".into(), self.name.as_ref().into()),
                    ("panel_w".into(), panel_w.to_string().into()),
                    ("panel_h".into(), panel_h.to_string().into()),
                ]
                .into(),
                ..Default::default()
            },
        )?;
        panel.update_layout(app)?;
        panel.init(app)?;
        Ok(panel)
    }
}

impl Drop for DesktopOverlay {
    fn drop(&mut self) {
        if let Some(id) = self.shown.take() {
            release_frame(id);
        }
    }
}

impl OverlayBackend for DesktopOverlay {
    fn init(&mut self, _: &mut AppState) -> anyhow::Result<()> {
        Ok(())
    }
    fn pause(&mut self, _: &mut AppState) -> anyhow::Result<()> {
        Ok(())
    }
    fn resume(&mut self, _: &mut AppState) -> anyhow::Result<()> {
        Ok(())
    }
    fn should_render(&mut self, app: &mut AppState) -> anyhow::Result<ShouldRender> {
        // Bound by name: the key of a monitor can differ between sessions, and remembered screens start without one
        let latest = match surfaces()
            .iter_mut()
            .find(|(_, surface)| surface.name == self.name)
        {
            Some((&key, surface)) => {
                self.key = key;
                surface.frame.take()
            }
            None => {
                self.planes = None;
                if let Some(id) = self.shown.take() {
                    release_frame(id);
                }
                self.imports.clear();
                self.renderer = None;
                if self.offline.is_none() {
                    self.offline = Some(self.offline_panel(app)?);
                }
                return self.offline.as_mut().unwrap().should_render(app);
            }
        };
        if latest.is_some() {
            self.offline = None;
        } else if self.offline.is_some() || self.planes.is_none() {
            // Connected, first frame still on its way: keep the placeholder (a fresh screen has nothing else)
            if self.offline.is_none() {
                self.offline = Some(self.offline_panel(app)?);
            }
            return self.offline.as_mut().unwrap().should_render(app);
        }
        if let Some((frame, fd)) = latest {
            let extent = [frame.width, frame.height];
            if self.meta.is_some_and(|old| old.extent != extent) || self.imports.len() > 32 {
                self.imports.clear();
            }
            if !self.imports.contains_key(&frame.id) {
                let plane =
                    |format, size: [u32; 2], offset: u64| -> anyhow::Result<Arc<ImageView>> {
                        let image = app.gfx.import_dmabuf(&DmabufImportInfo {
                            extent: [size[0], size[1], 1],
                            format,
                            fd: fd.as_raw_fd(),
                            modifier: Some(0),
                            plane_layouts: vec![DmabufPlaneLayout {
                                offset,
                                row_pitch: frame.stride.into(),
                            }],
                        })?;
                        app.gfx.create_image_view(image)
                    };
                let luma = plane(Format::R8_UNORM, extent, 0)?;
                let chroma = plane(
                    Format::R8G8_UNORM,
                    [frame.width.div_ceil(2), frame.height.div_ceil(2)],
                    frame.chroma_offset,
                )?;
                self.imports.insert(frame.id, (luma, chroma));
            }
            self.planes = self.imports.get(&frame.id).cloned();
            if let Some(old) = self.shown.replace(frame.id)
                && old != frame.id
            {
                release_frame(old);
            }
            let meta = FrameMeta {
                extent,
                ..Default::default()
            };
            // Also after a disconnect, which drops the renderer but keeps the size for the placeholder
            if self.renderer.is_none() || self.meta.is_none_or(|old| old.extent != extent) {
                self.renderer = Some(Nv12ScreenPipeline::new(&meta, app)?);
            }
            self.meta = Some(meta);
            Ok(ShouldRender::Should)
        } else {
            Ok(if self.planes.is_some() {
                ShouldRender::Can
            } else {
                ShouldRender::Unable
            })
        }
    }
    fn render(
        &mut self,
        app: &mut AppState,
        resources: &mut RenderResources,
    ) -> anyhow::Result<()> {
        if let Some(offline) = &mut self.offline {
            return offline.render(app, resources);
        }
        let (luma, chroma) = self.planes.clone().unwrap();
        self.renderer
            .as_mut()
            .unwrap()
            .render(luma, chroma, app, resources)
    }
    fn frame_meta(&mut self) -> Option<FrameMeta> {
        match &mut self.offline {
            Some(offline) => offline.frame_meta(),
            None => self.meta,
        }
    }
    fn notify(&mut self, _: &mut AppState, _: OverlayEventData) -> anyhow::Result<()> {
        Ok(())
    }
    // WayVR's own screen input (hand priority, move throttle, click freeze, left-handed mode, scrolling) on
    // this surface's normalized coordinates
    fn on_hover(&mut self, app: &mut AppState, hit: &PointerHit) -> HoverResult {
        if let Some(offline) = &mut self.offline {
            return offline.on_hover(app, hit);
        }
        POINTER_SURFACE.store(self.key, Ordering::Relaxed);
        screen_hover(app, hit, hit.uv)
    }
    fn on_left(&mut self, app: &mut AppState, pointer: usize) {
        if let Some(offline) = &mut self.offline {
            offline.on_left(app, pointer);
        }
    }
    fn on_pointer(&mut self, app: &mut AppState, hit: &PointerHit, pressed: bool) {
        if self.offline.is_some() {
            // Only ever connects: another screen of the same PC may already be streaming
            if pressed {
                connect_screen_host(app, &self.name);
            }
            return;
        }
        POINTER_SURFACE.store(self.key, Ordering::Relaxed);
        screen_pointer(app, hit, pressed, hit.uv);
    }
    fn on_scroll(&mut self, app: &mut AppState, _: &PointerHit, delta: WheelDelta) {
        if self.offline.is_none() {
            screen_scroll(app, delta);
        }
    }
    fn get_interaction_transform(&mut self) -> Option<Affine2> {
        match &mut self.offline {
            Some(offline) => offline.get_interaction_transform(),
            None => self.meta.map(|meta| ui_transform(meta.extent)),
        }
    }
    fn get_attrib(&self, _: BackendAttrib) -> Option<BackendAttribValue> {
        None
    }
    fn set_attrib(&mut self, _: &mut AppState, _: BackendAttribValue) -> bool {
        false
    }
}

pub fn openxr_entry() -> anyhow::Result<openxr::Entry> {
    if !EMBEDDED.load(Ordering::Relaxed) {
        return Ok(openxr::Entry::linked());
    }
    let path = std::ffi::CString::new(std::env::var("FRAME_SHELL_OPENXR_LOADER")?)?;
    unsafe {
        let library = libc::dlopen(
            path.as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL | libc::RTLD_DEEPBIND,
        );
        if library.is_null() {
            anyhow::bail!(
                "Shell OpenXR loader: {}",
                std::ffi::CStr::from_ptr(libc::dlerror()).to_string_lossy()
            );
        }
        let symbol = libc::dlsym(library, c"xrGetInstanceProcAddr".as_ptr());
        if symbol.is_null() {
            anyhow::bail!("Shell loader has no xrGetInstanceProcAddr");
        }
        let get_proc =
            std::mem::transmute::<*mut libc::c_void, openxr::sys::pfn::GetInstanceProcAddr>(symbol);
        Ok(openxr::Entry::from_get_instance_proc_addr(get_proc)?)
    }
}


#[cfg(feature = "feat-monado-metrics")]
pub fn remote_connected() -> bool {
    EMBEDDED.load(Ordering::Relaxed) && CONNECTED.load(Ordering::Relaxed)
}

#[cfg(feature = "feat-monado-metrics")]
pub fn watch_metrics(
    app: &AppState,
) -> Option<&crate::backend::openxr::monado_state::MonadoWatchMetrics> {
    if remote_connected() {
        app.remote_metrics
            .as_ref()
            .filter(|(updated, _)| updated.elapsed() < std::time::Duration::from_secs(2))
            .map(|(_, metrics)| metrics)
    } else {
        app.monado_state
            .as_ref()
            .map(|monado| &monado.watch_metrics)
    }
}

pub fn host_system() -> Option<wayvr_ipc::frame::SystemStats> {
    crate::frame_host::system()
}


pub fn remote_game_running(host: &str, app_id: &str) -> bool {
    source_for_host(host)
        .and_then(crate::frame_host::games)
        .is_some_and(|games| games.iter().any(|game| game == app_id))
}

pub fn stop_remote_game(host: &str, app_id: &str, force: bool) {
    if let Some(source) = source_for_host(host) {
        crate::frame_host::send(
            source,
            &wayvr_ipc::frame::ShellMessage::StopGame {
                app_id: app_id.to_owned(),
                force,
            },
        );
    }
}

/// The PCs connected besides the one presenting VR: [{"cookie", "name", "state", "detail", "source"}]
fn hosts() -> serde_json::Value {
    let json = HOSTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    serde_json::from_str(&json).unwrap_or_else(|_| serde_json::json!([]))
}

/// A connected PC's source number, by WiVRn server name
fn source_for_host(host: &str) -> Option<u32> {
    if connected_host().as_deref() == Some(host) {
        return crate::frame_host::vr_source();
    }
    hosts()
        .as_array()?
        .iter()
        .find(|pc| {
            pc.get("name").and_then(serde_json::Value::as_str) == Some(host)
                && pc.get("state").and_then(serde_json::Value::as_str) == Some("connected")
        })
        .and_then(|pc| pc.get("source"))
        .and_then(serde_json::Value::as_u64)
        .map(|source| source as u32)
}

pub fn pinned_stop_key(host: Option<&str>, app_id: &str) -> String {
    host.map_or_else(|| app_id.to_owned(), |host| format!("{host}:{app_id}"))
}
