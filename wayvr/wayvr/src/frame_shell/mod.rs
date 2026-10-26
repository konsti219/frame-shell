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

mod abi;
mod desktops;
mod floor;
mod keys;
mod pcs;
mod top_row;
pub use abi::*;
pub use desktops::*;
pub use floor::*;
pub use keys::*;
pub use pcs::*;
pub use top_row::*;

pub fn set_input_capture(paths: &[std::ffi::CString], pose_mask: u32) {
    let pointers: Vec<_> = paths.iter().map(|p| p.as_ptr()).collect();
    if let Some(Host(host)) = host().as_ref() {
        if let Some(set) = host.set_input_capture {
            unsafe { set(host.user, pointers.as_ptr(), pointers.len(), pose_mask) };
        }
    }
}

pub fn runtime_clients(
    source: u32,
    clients: &[wayvr_ipc::frame::RuntimeClient],
    known: bool,
    composition_supported: bool,
) {
    let clients: Vec<_> = clients
        .iter()
        .map(|client| frame_abi::frame_runtime_client {
            id: client.id,
            overlay: client.overlay,
            visible: client.visible,
            primary: client.primary,
        })
        .collect();
    if let Some(Host(host)) = host().as_ref()
        && let Some(send) = host.runtime_clients
    {
        unsafe {
            send(
                host.user,
                source,
                clients.as_ptr(),
                clients.len(),
                known,
                composition_supported,
            )
        };
    }
}

pub static EMBEDDED: AtomicBool = AtomicBool::new(false);
/// The WiVRn client's callbacks, from `run`
static HOST: Mutex<Option<Host>> = Mutex::new(None);
static CONNECTED: AtomicBool = AtomicBool::new(false);
static REMOTE_LAYER_VISIBLE: AtomicBool = AtomicBool::new(false);
static INPUT: Mutex<VecDeque<ShellInput>> = Mutex::new(VecDeque::new());
/// PC monitors announced by the client, keyed by `surface_key`
static SURFACES: Mutex<BTreeMap<u64, Surface>> = Mutex::new(BTreeMap::new());
/// Counts `openxr_run`s, each of which starts with no desktop overlays
static RUN: AtomicU64 = AtomicU64::new(0);
/// Surface the PC pointer was last moved on
static POINTER_SURFACE: AtomicU64 = AtomicU64::new(0);
/// The WiVRn client's lobby (servers, connection) and stream (PC apps) state as JSON, with a change counter
pub static LOBBY_STATE: Mutex<(u64, String)> = Mutex::new((0, String::new()));
static STREAM_STATES: Mutex<BTreeMap<u32, serde_json::Value>> = Mutex::new(BTreeMap::new());
/// Commands for the WiVRn client (`client-shell.h` lists them)
static COMMANDS: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
/// PIN digits typed on the PC panel so far
pub static PIN: Mutex<String> = Mutex::new(String::new());
/// Requests for the dashboard (power menu, pairing) and whether to bring it up for them
static POWER_MENU_REQUEST: AtomicBool = AtomicBool::new(false);
static PAIRING_REQUEST: AtomicBool = AtomicBool::new(false);
static SHOW_DASHBOARD: AtomicBool = AtomicBool::new(false);
/// An application to start on a PC once connected to it: (host, app id, name)
static PENDING_LAUNCH: Mutex<VecDeque<(String, String, String)>> = Mutex::new(VecDeque::new());
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
    connector: String,
    registered: bool,
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

fn c_str(text: *const std::ffi::c_char) -> String {
    if text.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}

/// `RUN` in which the remembered screens were spawned
static SCREENS_SPAWNED_RUN: AtomicU64 = AtomicU64::new(0);

fn remembered_screens() -> Vec<String> {
    crate::frame_hosts::layouts()
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

/// The shell is stopped without a chance to save on exit, so the layout is saved after every move
pub fn layout_changed() {
    LAYOUT_CHANGED_MS.store(START.elapsed().as_millis().max(1) as u64, Ordering::Relaxed);
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
    if remote_connected() && REMOTE_LAYER_VISIBLE.load(Ordering::Relaxed) {
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

