//! The frame shell's PC host services, linked into WiVRn's server through frame_abi.h: monitor capture and
//! encoding, browser media and wayvr-ipc clients, Music volume, running games, runtime metrics, system stats and
//! notifications, for one headset connection at a time.

mod capture;
mod games;
mod ipc_proxy;
mod metrics;
mod metrics_proto;
mod music;
mod notifications;
mod notify;
mod system;

use frame_abi::{frame_host, frame_host_callbacks, frame_host_surface};
use std::{
    collections::{HashMap, HashSet},
    ffi::CString,
    sync::{
        Arc, Mutex, MutexGuard, RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use wayvr_ipc::frame::{DesktopSettings, HostMessage, ShellMessage};

/// The server's callbacks, callable from any thread until frame_host_stop returns
struct Callbacks(frame_host_callbacks);
unsafe impl Send for Callbacks {}
unsafe impl Sync for Callbacks {}

/// What the workers send to the server; nothing goes out once stopping
#[derive(Clone)]
pub struct Output {
    callbacks: Arc<Callbacks>,
    stop: Arc<AtomicBool>,
    /// Held while calling back; stopping takes it to close it, so no callback runs after frame_host_stop
    open: Arc<RwLock<bool>>,
}

impl Output {
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Sleeps for `duration` unless stopped first; false once stopped
    pub fn sleep(&self, duration: Duration) -> bool {
        let end = Instant::now() + duration;
        while !self.stopped() {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            std::thread::sleep(left.min(Duration::from_millis(50)));
        }
        false
    }

    /// Calls back into the server unless stopping
    fn call(&self, call: impl FnOnce(&frame_host_callbacks)) {
        let open = self
            .open
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *open {
            call(&self.callbacks.0);
        }
    }

    pub fn send(&self, message: &HostMessage) {
        let data = message.encode();
        self.call(|c| {
            if let Some(callback) = c.message {
                unsafe { callback(c.user, data.as_ptr(), data.len()) };
            }
        });
    }

    pub fn surface(&self, id: u32, size: (u32, u32), region: [f32; 4], name: &str, hevc: bool) {
        let name = CString::new(name.replace('\0', "")).unwrap_or_default();
        let surface = frame_host_surface {
            id,
            width: size.0,
            height: size.1,
            region,
            name: name.as_ptr(),
            codec: u32::from(hevc),
        };
        self.call(|c| {
            if let Some(callback) = c.surface {
                unsafe { callback(c.user, &surface) };
            }
        });
    }

    pub fn removed(&self, id: u32) {
        self.call(|c| {
            if let Some(callback) = c.surface_removed {
                unsafe { callback(c.user, id) };
            }
        });
    }

    pub fn video(&self, id: u32, data: &[u8]) {
        self.call(|c| {
            if let Some(callback) = c.video {
                unsafe { callback(c.user, id, data.as_ptr(), data.len()) };
            }
        });
    }
}

/// State the shell sets and the capture workers read
#[derive(Default)]
pub struct Shared {
    pub settings: Mutex<Option<DesktopSettings>>,
    pub visibility: Mutex<HashMap<u32, bool>>,
    /// Monitors whose next frame must be a keyframe
    pub keyframes: Mutex<HashSet<u32>>,
}

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Shared {
    pub fn settings(&self) -> DesktopSettings {
        lock(&self.settings).unwrap_or(DesktopSettings {
            bitrate_per_1080p: 16000,
            hevc: false,
        })
    }
}

struct Host {
    output: Output,
    shared: Arc<Shared>,
    power: Sender<u8>,
    games: Sender<(String, bool)>,
    music: Sender<u32>,
    proxy: Arc<ipc_proxy::Proxy>,
    threads: Vec<JoinHandle<()>>,
}

impl Host {
    fn start(callbacks: frame_host_callbacks) -> anyhow::Result<Self> {
        gstreamer::init()?;
        let output = Output {
            callbacks: Arc::new(Callbacks(callbacks)),
            stop: Arc::default(),
            open: Arc::new(RwLock::new(true)),
        };
        let shared = Arc::new(Shared::default());
        let (power, power_rx) = mpsc::channel();
        let (games, games_rx) = mpsc::channel();
        let (music, music_rx) = mpsc::channel();
        let proxy = Arc::new(ipc_proxy::Proxy::default());
        let mut threads = Vec::new();
        let mut spawn = |name: &str, body: Box<dyn FnOnce() + Send>| {
            threads.push(
                std::thread::Builder::new()
                    .name(format!("frame-{name}"))
                    .spawn(body)
                    .expect("spawning a host thread"),
            );
        };
        let worker = output.clone();
        spawn("system", Box::new(move || system::run(&worker, &power_rx)));
        let worker = output.clone();
        spawn("games", Box::new(move || games::run(&worker, &games_rx)));
        let worker = output.clone();
        spawn("music", Box::new(move || music::run(&worker, &music_rx)));
        let worker = output.clone();
        spawn("metrics", Box::new(move || metrics::run(&worker)));
        let worker = output.clone();
        spawn(
            "notify-udp",
            Box::new(move || notifications::run_udp(&worker)),
        );
        let worker = output.clone();
        spawn(
            "notify-dbus",
            Box::new(move || notifications::run_dbus(&worker)),
        );
        let (worker, connections) = (output.clone(), proxy.clone());
        spawn(
            "ipc",
            Box::new(move || ipc_proxy::run(&worker, &connections)),
        );
        let (worker, state) = (output.clone(), shared.clone());
        spawn("capture", Box::new(move || capture::run(&worker, &state)));
        Ok(Self {
            output,
            shared,
            power,
            games,
            music,
            proxy,
            threads,
        })
    }

    fn receive(&self, message: ShellMessage) {
        match message {
            ShellMessage::DesktopSettings(settings) => {
                *lock(&self.shared.settings) = Some(DesktopSettings {
                    bitrate_per_1080p: settings.bitrate_per_1080p.clamp(1000, 150000),
                    ..settings
                });
            }
            ShellMessage::DesktopVisibility { surface, visible } => {
                lock(&self.shared.visibility).insert(surface, visible);
            }
            ShellMessage::StopGame { app_id, force } => {
                if app_id.parse::<u64>().is_ok() {
                    let _ = self.games.send((app_id, force));
                }
            }
            ShellMessage::PowerProfile(profile) => {
                if profile <= 2 {
                    let _ = self.power.send(profile);
                }
            }
            ShellMessage::MusicVolume(percent) => {
                let _ = self.music.send(percent.min(100));
            }
            ShellMessage::Ipc(connection, packet) => self.proxy.send(connection, &packet),
            ShellMessage::IpcClose(connection) => self.proxy.close(connection),
        }
    }

    fn stop(self) {
        self.output.stop.store(true, Ordering::Release);
        *self
            .output
            .open
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
        self.proxy.close_all();
        for thread in self.threads {
            thread.join().expect("host worker panicked");
        }
    }
}

/// Checks the definitions below against frame_abi.h's declarations
const _: () = {
    let _: unsafe extern "C" fn(*const frame_host_callbacks) -> *mut frame_host =
        frame_abi::frame_host_start;
    let _: unsafe extern "C" fn(*mut frame_host, *const u8, usize) = frame_abi::frame_host_receive;
    let _: unsafe extern "C" fn(*mut frame_host, u32) = frame_abi::frame_host_request_keyframe;
    let _: unsafe extern "C" fn(*mut frame_host) = frame_abi::frame_host_stop;
};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn frame_host_start(
    callbacks: *const frame_host_callbacks,
) -> *mut frame_host {
    let Some(callbacks) = (unsafe { callbacks.as_ref() }).copied() else {
        return std::ptr::null_mut();
    };
    match std::panic::catch_unwind(|| Host::start(callbacks)) {
        Ok(Ok(host)) => Box::into_raw(Box::new(host)).cast(),
        Ok(Err(error)) => {
            eprintln!("Frame host: {error:#}");
            std::ptr::null_mut()
        }
        Err(_) => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn frame_host_receive(host: *mut frame_host, data: *const u8, size: usize) {
    let Some(host) = (unsafe { host.cast::<Host>().as_ref() }) else {
        return;
    };
    if data.is_null() {
        return;
    }
    let data = unsafe { std::slice::from_raw_parts(data, size) };
    match ShellMessage::decode(data) {
        Ok(message) => {
            let _ =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host.receive(message)));
        }
        Err(error) => eprintln!("Frame host: invalid shell message: {error:#}"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn frame_host_request_keyframe(host: *mut frame_host, surface: u32) {
    if let Some(host) = unsafe { host.cast::<Host>().as_ref() } {
        lock(&host.shared.keyframes).insert(surface);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn frame_host_stop(host: *mut frame_host) {
    if !host.is_null() {
        let host = unsafe { Box::from_raw(host.cast::<Host>()) };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host.stop()));
    }
}
