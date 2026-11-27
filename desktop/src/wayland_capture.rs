//! Direct Wayland output capture via `ext-image-copy-capture-v1` feeding GStreamer hardware encoders.

use crate::{Output, Shared, capture, lock};
use anyhow::{Context, Result, bail};
use gstreamer::{self as gst, prelude::*};
use gstreamer_app::{AppSink, AppSrc};
use std::{
    ffi::CString,
    os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd},
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
#[cfg(test)]
use std::sync::Arc;
use wayvr_ipc::{frame::DesktopSettings, metrics};
use wlx_capture::libc;
use wlx_capture::wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
    backend::WaylandError,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_buffer::WlBuffer,
        wl_output::{self, WlOutput},
        wl_registry::{self, WlRegistry},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
    },
};
use wlx_capture::wayland_protocols::ext::{
    image_capture_source::v1::client::{
        ext_image_capture_source_v1::ExtImageCaptureSourceV1,
        ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
    },
    image_copy_capture::v1::client::{
        ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1, FailureReason},
        ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
        ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
    },
};

/// Returns true when the compositor advertises `ext_image_copy_capture_manager_v1` and
/// `ext_output_image_capture_source_manager_v1`.
pub fn supports_ext_image_copy_capture() -> bool {
    let Ok(conn) = Connection::connect_to_env() else {
        return false;
    };
    let Ok((globals, _queue)) = registry_queue_init::<ProbeState>(&conn) else {
        return false;
    };
    let list = globals.contents().clone_list();
    let has_capture = list
        .iter()
        .any(|g| g.interface == ExtImageCopyCaptureManagerV1::interface().name);
    let has_source = list
        .iter()
        .any(|g| g.interface == ExtOutputImageCaptureSourceManagerV1::interface().name);
    has_capture && has_source
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameStatus {
    Idle,
    Pending,
    Ready,
    Failed(Option<FailureReason>),
}

struct OutputEntry {
    registry_name: u32,
    proxy: WlOutput,
    name: Option<String>,
}

struct CaptureState {
    target_name: String,
    outputs: Vec<OutputEntry>,
    output_removed: bool,

    pending_size: (u32, u32),
    pending_formats: Vec<wl_shm::Format>,

    active_size: (u32, u32),
    active_formats: Vec<wl_shm::Format>,
    constraints_gen: u64,
    session_stopped: bool,
    frame_status: FrameStatus,
}

impl CaptureState {
    fn new(target_name: String) -> Self {
        Self {
            target_name,
            outputs: Vec::new(),
            output_removed: false,
            pending_size: (0, 0),
            pending_formats: Vec::new(),
            active_size: (0, 0),
            active_formats: Vec::new(),
            constraints_gen: 0,
            session_stopped: false,
            frame_status: FrameStatus::Idle,
        }
    }

    fn target_output(&self) -> Option<&WlOutput> {
        self.outputs
            .iter()
            .find(|entry| entry.name.as_deref() == Some(self.target_name.as_str()))
            .map(|entry| &entry.proxy)
    }

    fn reset_session_state(&mut self) {
        self.pending_size = (0, 0);
        self.pending_formats.clear();
        self.active_size = (0, 0);
        self.active_formats.clear();
        self.session_stopped = false;
        self.frame_status = FrameStatus::Idle;
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for CaptureState {
    fn event(
        state: &mut Self,
        proxy: &WlRegistry,
        event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        qhandle: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == WlOutput::interface().name => {
                let wl_output: WlOutput = proxy.bind(name, version.min(4), qhandle, name);
                state.outputs.push(OutputEntry {
                    registry_name: name,
                    proxy: wl_output,
                    name: None,
                });
            }
            wl_registry::Event::GlobalRemove { name } => {
                if let Some(pos) = state.outputs.iter().position(|o| o.registry_name == name) {
                    let removed = state.outputs.remove(pos);
                    if removed.name.as_deref() == Some(state.target_name.as_str()) {
                        state.output_removed = true;
                    }
                    removed.proxy.release();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, u32> for CaptureState {
    fn event(
        state: &mut Self,
        _proxy: &WlOutput,
        event: wl_output::Event,
        registry_name: &u32,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event
            && let Some(entry) = state
                .outputs
                .iter_mut()
                .find(|entry| entry.registry_name == *registry_name)
        {
            if name == state.target_name {
                state.output_removed = false;
            }
            entry.name = Some(name);
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for CaptureState {
    fn event(
        state: &mut Self,
        _proxy: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                state.pending_size = (width, height);
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat {
                format: WEnum::Value(fmt),
            } => {
                state.pending_formats.push(fmt);
            }
            ext_image_copy_capture_session_v1::Event::Done => {
                state.active_size = state.pending_size;
                if !state.pending_formats.is_empty() {
                    state.active_formats = std::mem::take(&mut state.pending_formats);
                }
                state.constraints_gen = state.constraints_gen.wrapping_add(1);
            }
            ext_image_copy_capture_session_v1::Event::Stopped => {
                state.session_stopped = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for CaptureState {
    fn event(
        state: &mut Self,
        proxy: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => {
                state.frame_status = FrameStatus::Ready;
                proxy.destroy();
            }
            ext_image_copy_capture_frame_v1::Event::Failed { reason } => {
                state.frame_status = FrameStatus::Failed(reason.into_result().ok());
                proxy.destroy();
            }
            _ => {}
        }
    }
}

macro_rules! noop_dispatch {
    ($($ty:ty),* $(,)?) => {
        $(
            impl Dispatch<$ty, ()> for CaptureState {
                fn event(
                    _state: &mut Self,
                    _proxy: &$ty,
                    _event: <$ty as Proxy>::Event,
                    _data: &(),
                    _conn: &Connection,
                    _qhandle: &QueueHandle<Self>,
                ) {
                }
            }
        )*
    };
}

noop_dispatch!(
    WlShm,
    WlShmPool,
    WlBuffer,
    ExtOutputImageCaptureSourceManagerV1,
    ExtImageCaptureSourceV1,
    ExtImageCopyCaptureManagerV1,
);

struct ShmFrameBuffer {
    width: u32,
    height: u32,
    format: wl_shm::Format,
    gst_format: &'static str,
    wl_buffer: WlBuffer,
    wl_pool: WlShmPool,
    ptr: NonNull<u8>,
    byte_len: usize,
    _fd: OwnedFd,
}

impl ShmFrameBuffer {
    fn pick_format(formats: &[wl_shm::Format]) -> Option<(wl_shm::Format, &'static str)> {
        const PREFERENCE: &[(wl_shm::Format, &str)] = &[
            (wl_shm::Format::Xrgb8888, "BGRx"),
            (wl_shm::Format::Argb8888, "BGRA"),
            (wl_shm::Format::Xbgr8888, "RGBx"),
            (wl_shm::Format::Abgr8888, "RGBA"),
        ];
        PREFERENCE
            .iter()
            .find(|(fmt, _)| formats.contains(fmt))
            .copied()
    }

    fn allocate(
        wl_shm: &WlShm,
        qh: &QueueHandle<CaptureState>,
        width: u32,
        height: u32,
        formats: &[wl_shm::Format],
    ) -> Result<Self> {
        let (format, gst_format) = Self::pick_format(formats)
            .with_context(|| format!("No supported 32-bit wl_shm format in {formats:?}"))?;
        let stride = width
            .checked_mul(4)
            .context("Frame width overflows stride")?;
        let byte_len = usize::try_from(stride)
            .ok()
            .and_then(|s| s.checked_mul(height as usize))
            .filter(|&n| n > 0 && n <= i32::MAX as usize)
            .context("Invalid SHM buffer dimensions")?;

        let fd = create_shm_fd(byte_len)?;
        let raw_ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                byte_len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if raw_ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error()).context("mmap SHM capture buffer");
        }
        let ptr = NonNull::new(raw_ptr.cast::<u8>()).context("mmap returned null")?;
        let wl_pool = wl_shm.create_pool(fd.as_fd(), byte_len as i32, qh, ());
        let wl_buffer = wl_pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            format,
            qh,
            (),
        );
        Ok(Self {
            width,
            height,
            format,
            gst_format,
            wl_buffer,
            wl_pool,
            ptr,
            byte_len,
            _fd: fd,
        })
    }

    fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.byte_len) }
    }
}

impl Drop for ShmFrameBuffer {
    fn drop(&mut self) {
        self.wl_buffer.destroy();
        self.wl_pool.destroy();
        unsafe {
            libc::munmap(self.ptr.as_ptr().cast(), self.byte_len);
        }
    }
}

fn create_shm_fd(byte_len: usize) -> Result<OwnedFd> {
    let raw_fd = unsafe {
        let fd = libc::memfd_create(c"frame-virtual-shm".as_ptr(), libc::MFD_CLOEXEC);
        if fd >= 0 {
            fd
        } else {
            let name = CString::new(format!("/frame-shm-{}", std::process::id()))?;
            let shm_fd = libc::shm_open(
                name.as_ptr(),
                libc::O_CREAT | libc::O_RDWR | libc::O_EXCL,
                libc::S_IRUSR | libc::S_IWUSR,
            );
            let _ = libc::shm_unlink(name.as_ptr());
            if shm_fd < 0 {
                return Err(std::io::Error::last_os_error()).context("memfd_create / shm_open");
            }
            shm_fd
        }
    };
    let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    if unsafe { libc::ftruncate(fd.as_raw_fd(), byte_len as libc::off_t) } != 0 {
        return Err(std::io::Error::last_os_error()).context("ftruncate SHM capture buffer");
    }
    Ok(fd)
}

struct EncoderPipeline {
    pipeline: gst::Pipeline,
    appsrc: AppSrc,
    appsink: AppSink,
    hevc: bool,
    applied_bitrate: u32,
    width: u32,
    height: u32,
    gst_format: &'static str,
    started: Instant,
    seq: u64,
}

impl EncoderPipeline {
    fn new(
        width: u32,
        height: u32,
        gst_format: &'static str,
        settings: DesktopSettings,
    ) -> Result<Self> {
        let bitrate = capture::scaled_bitrate(settings, width, height);
        let (_codec, tail) = capture::encoder_tail(settings.hevc, bitrate)?;
        let pipeline_desc = format!(
            "appsrc name=source is-live=true format=time do-timestamp=true block=false max-buffers=2 leaky-type=downstream ! videorate name=capture_rate drop-only=true ! video/x-raw,framerate=60/1 ! {tail} ! appsink name=encoded sync=false max-buffers=2 drop=false"
        );
        let pipeline = gst::parse::launch(&pipeline_desc)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow::anyhow!("Invalid virtual panel capture pipeline"))?;
        pipeline.use_clock(None::<&gst::Clock>);
        capture::install_live_latency_probe(&pipeline);

        let appsrc = pipeline
            .by_name("source")
            .context("Missing appsrc")?
            .downcast::<AppSrc>()
            .map_err(|_| anyhow::anyhow!("Invalid appsrc element"))?;
        let src_caps = gst::Caps::builder("video/x-raw")
            .field("format", gst_format)
            .field("width", width as i32)
            .field("height", height as i32)
            .field("framerate", gst::Fraction::new(0, 1))
            .build();
        appsrc.set_caps(Some(&src_caps));
        let appsink = pipeline
            .by_name("encoded")
            .context("Missing appsink")?
            .downcast::<AppSink>()
            .map_err(|_| anyhow::anyhow!("Invalid appsink element"))?;

        pipeline.set_state(gst::State::Playing)?;
        Ok(Self {
            pipeline,
            appsrc,
            appsink,
            hevc: settings.hevc,
            applied_bitrate: bitrate,
            width,
            height,
            gst_format,
            started: Instant::now(),
            seq: 0,
        })
    }

    fn push_bytes(&mut self, output_name: &str, bytes: &[u8]) -> Result<()> {
        let mut buffer = gst::Buffer::with_size(bytes.len())?;
        {
            let buf_ref = buffer.get_mut().context("Buffer not mutable")?;
            let now = gst::ClockTime::from_nseconds(self.started.elapsed().as_nanos() as u64);
            buf_ref.set_pts(now);
            buf_ref.set_dts(now);
            buf_ref.set_offset(self.seq);
            self.seq = self.seq.wrapping_add(1);
            let mut map = buf_ref.map_writable()?;
            map.as_mut_slice().copy_from_slice(bytes);
        }
        metrics::emit(output_name, "captured", self.seq, bytes.len() as u64);
        self.appsrc
            .push_buffer(buffer)
            .map(|_| ())
            .map_err(|err| anyhow::anyhow!("appsrc push_buffer failed: {err:?}"))
    }

    fn update_bitrate(&mut self, id: u32, settings: DesktopSettings) -> Result<()> {
        let desired = capture::scaled_bitrate(settings, self.width, self.height);
        if desired != self.applied_bitrate {
            self.pipeline
                .by_name("encoder")
                .context("Missing encoder")?
                .set_property("bitrate", desired);
            self.applied_bitrate = desired;
            eprintln!(
                "Virtual surface {id} ({}x{}): {} Mbit/s",
                self.width,
                self.height,
                desired / 1000
            );
        }
        Ok(())
    }
}

impl Drop for EncoderPipeline {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

fn poll_wayland(
    conn: &Connection,
    queue: &mut EventQueue<CaptureState>,
    state: &mut CaptureState,
    timeout_ms: i32,
) -> Result<()> {
    let _ = conn.flush();
    if queue.dispatch_pending(state)? > 0 {
        return Ok(());
    }
    if let Some(guard) = queue.prepare_read() {
        let mut pfd = libc::pollfd {
            fd: conn.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if rc > 0 {
            match guard.read() {
                Ok(_) => {
                    queue.dispatch_pending(state)?;
                }
                Err(WaylandError::Io(ref err)) if err.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(err) => return Err(err.into()),
            }
        } else {
            drop(guard);
        }
    } else {
        queue.dispatch_pending(state)?;
    }
    Ok(())
}

/// Captures `output_name` over `ext-image-copy-capture-v1` and encodes into `Output::video` until
/// `cancel`, `output.stopped()`, or the compositor removes `output_name`.
pub fn stream_virtual_output(
    id: u32,
    output_name: &str,
    cancel: &AtomicBool,
    output: &Output,
    shared: &Shared,
) -> Result<()> {
    let conn = Connection::connect_to_env().context("Connect to Wayland for virtual capture")?;
    let (globals, mut queue) = registry_queue_init::<CaptureState>(&conn)?;
    let qh = queue.handle();

    let wl_shm: WlShm = globals.bind(&qh, 1..=1, ()).context("Bind wl_shm")?;
    let source_mgr: ExtOutputImageCaptureSourceManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .context("Bind ext_output_image_capture_source_manager_v1")?;
    let capture_mgr: ExtImageCopyCaptureManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .context("Bind ext_image_copy_capture_manager_v1")?;

    let mut state = CaptureState::new(output_name.to_owned());
    for g in globals.contents().clone_list() {
        if g.interface == WlOutput::interface().name {
            let wl_output: WlOutput = globals
                .registry()
                .bind(g.name, g.version.min(4), &qh, g.name);
            state.outputs.push(OutputEntry {
                registry_name: g.name,
                proxy: wl_output,
                name: None,
            });
        }
    }
    queue.roundtrip(&mut state)?;

    let wait_deadline = Instant::now() + Duration::from_secs(3);
    while state.target_output().is_none() {
        if cancel.load(Ordering::Acquire) || output.stopped() {
            return Ok(());
        }
        if Instant::now() >= wait_deadline {
            bail!("Timed out waiting for Wayland output {output_name}");
        }
        poll_wayland(&conn, &mut queue, &mut state, 25)?;
    }

    let mut encoder: Option<EncoderPipeline> = None;
    let mut announced: Option<(u32, u32)> = None;
    let mut playing = lock(&shared.visibility).get(&id).copied().unwrap_or(true);
    let mut last_frame: Option<Vec<u8>> = None;

    'session_loop: while !cancel.load(Ordering::Acquire) && !output.stopped() && !state.output_removed
    {
        let Some(wl_output) = state.target_output().cloned() else {
            break;
        };
        state.reset_session_state();
        let source = source_mgr.create_source(&wl_output, &qh, ());
        let session = capture_mgr.create_session(&source, Options::PaintCursors, &qh, ());
        let _ = conn.flush();

        let mut active_gen = state.constraints_gen;
        let mut shm: Option<ShmFrameBuffer> = None;
        let mut in_flight_frame: Option<ExtImageCopyCaptureFrameV1> = None;
        let mut last_capture_at = Instant::now() - Duration::from_secs(1);
        let mut last_push_at = Instant::now() - Duration::from_secs(1);
        let frame_interval = Duration::from_nanos(1_000_000_000 / 60);

        while !cancel.load(Ordering::Acquire)
            && !output.stopped()
            && !state.output_removed
            && !state.session_stopped
        {
            poll_wayland(&conn, &mut queue, &mut state, 12)?;
            if state.session_stopped || state.output_removed {
                break;
            }

            let settings = shared.settings();
            if state.constraints_gen != active_gen || shm.is_none() {
                if state.active_size.0 > 0
                    && state.active_size.1 > 0
                    && !state.active_formats.is_empty()
                {
                    let (w, h) = state.active_size;
                    let needs_rebuf = shm.as_ref().is_none_or(|b| {
                        b.width != w
                            || b.height != h
                            || !state.active_formats.contains(&b.format)
                    });
                    if needs_rebuf {
                        shm = Some(ShmFrameBuffer::allocate(
                            &wl_shm,
                            &qh,
                            w,
                            h,
                            &state.active_formats,
                        )?);
                    }
                    active_gen = state.constraints_gen;
                }
            }

            let Some(buf) = shm.as_ref() else {
                continue;
            };

            let rebuild_encoder = encoder.as_ref().is_none_or(|enc| {
                enc.width != buf.width
                    || enc.height != buf.height
                    || enc.gst_format != buf.gst_format
                    || enc.hevc != settings.hevc
            });
            if rebuild_encoder {
                drop(encoder.take());
                encoder = Some(EncoderPipeline::new(
                    buf.width,
                    buf.height,
                    buf.gst_format,
                    settings,
                )?);
                announced = None;
                last_frame = None;
            }
            let enc = encoder.as_mut().unwrap();
            enc.update_bitrate(id, settings)?;

            let visible = lock(&shared.visibility).get(&id).copied().unwrap_or(true);
            if visible != playing {
                playing = visible;
                if playing {
                    while enc.appsink.try_pull_sample(gst::ClockTime::ZERO).is_some() {}
                    capture::force_keyframe(&enc.pipeline);
                    if let Some(frame_bytes) = last_frame.as_deref() {
                        let _ = enc.push_bytes(output_name, frame_bytes);
                        last_push_at = Instant::now();
                    }
                    metrics::event(output_name, "encoding");
                    eprintln!("Virtual surface {id} ({output_name}): streaming");
                } else {
                    metrics::event(output_name, "encoding stopped");
                    eprintln!("Virtual surface {id} ({output_name}): hidden; encoding stopped");
                }
            }

            match state.frame_status {
                FrameStatus::Ready => {
                    in_flight_frame = None;
                    state.frame_status = FrameStatus::Idle;
                    let bytes = buf.as_slice();
                    let store = last_frame.get_or_insert_with(|| vec![0u8; bytes.len()]);
                    if store.len() != bytes.len() {
                        store.resize(bytes.len(), 0);
                    }
                    store.copy_from_slice(bytes);
                    if playing {
                        enc.push_bytes(output_name, store)?;
                        last_push_at = Instant::now();
                    }
                }
                FrameStatus::Failed(reason) => {
                    in_flight_frame = None;
                    state.frame_status = FrameStatus::Idle;
                    match reason {
                        Some(FailureReason::BufferConstraints) => {
                            shm = None;
                            continue;
                        }
                        Some(FailureReason::Stopped) => break,
                        _ => {}
                    }
                }
                FrameStatus::Pending | FrameStatus::Idle => {}
            }

            if playing && lock(&shared.keyframes).remove(&id) {
                metrics::event(output_name, "keyframe requested");
                capture::force_keyframe(&enc.pipeline);
                if let Some(frame_bytes) = last_frame.as_deref() {
                    let _ = enc.push_bytes(output_name, frame_bytes);
                    last_push_at = Instant::now();
                }
            }

            // Static virtual monitors produce an initial frame then wait for damage on subsequent captures;
            // re-push the last frame every 100 ms until the encoder outputs its first frame.
            if playing
                && announced.is_none()
                && let Some(frame_bytes) = last_frame.as_deref()
                && last_push_at.elapsed() >= Duration::from_millis(100)
            {
                let _ = enc.push_bytes(output_name, frame_bytes);
                last_push_at = Instant::now();
            }

            if playing
                && in_flight_frame.is_none()
                && state.frame_status == FrameStatus::Idle
                && last_capture_at.elapsed() >= frame_interval
            {
                let frame = session.create_frame(&qh, ());
                frame.attach_buffer(&buf.wl_buffer);
                frame.damage_buffer(0, 0, buf.width as i32, buf.height as i32);
                frame.capture();
                let _ = conn.flush();
                in_flight_frame = Some(frame);
                state.frame_status = FrameStatus::Pending;
                last_capture_at = Instant::now();
            }

            while let Some(sample) = enc.appsink.try_pull_sample(gst::ClockTime::ZERO) {
                let caps = sample.caps().context("Encoded virtual frame without caps")?;
                let structure = caps.structure(0).context("Empty virtual caps")?;
                let size = (
                    u32::try_from(structure.get::<i32>("width")?)?,
                    u32::try_from(structure.get::<i32>("height")?)?,
                );
                if announced != Some(size) {
                    let region = shared
                        .region_for_output(output_name)
                        .unwrap_or([0.0, 0.0, 1.0, 1.0]);
                    output.surface(id, size, region, output_name, settings.hevc);
                    shared.sync_surface_region(output, id);
                    announced = Some(size);
                }
                let buffer = sample.buffer().context("Empty virtual encoded buffer")?;
                let map = buffer.map_readable()?;
                let bytes = map.len() as u64;
                if !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT) {
                    metrics::emit(output_name, "keyframe", buffer.offset(), bytes);
                }
                metrics::emit(output_name, "encoded", buffer.offset(), bytes);
                if map.len() > 4 * 1024 * 1024 {
                    bail!("Virtual panel frame exceeds 4 MiB");
                }
                output.video(id, &map);
                metrics::emit(output_name, "sent", buffer.offset(), bytes);
            }

            if let Some(bus) = enc.pipeline.bus()
                && let Some(msg) = bus.pop_filtered(&[
                    gst::MessageType::Error,
                    gst::MessageType::Eos,
                    gst::MessageType::Warning,
                ])
            {
                if msg.type_() != gst::MessageType::Warning {
                    bail!("Virtual capture GStreamer error: {msg:?}");
                }
                metrics::event(output_name, &format!("{msg:?}"));
            }

            if !playing {
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        if let Some(frame) = in_flight_frame.take() {
            frame.destroy();
        }
        session.destroy();
        source.destroy();
        let _ = conn.flush();

        if state.output_removed || cancel.load(Ordering::Acquire) || output.stopped() {
            break 'session_loop;
        }
        std::thread::sleep(Duration::from_millis(50));
        let _ = queue.roundtrip(&mut state);
        if state.target_output().is_none() {
            break;
        }
    }

    drop(encoder);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::wlr_output::{OutputConfigUpdate, apply_output_layout};
    use std::ffi::c_void;
    use std::process::Command;
    use std::sync::Mutex;

    #[ignore = "requires a running Hyprland desktop session and GStreamer hardware encoder"]
    #[test]
    fn live_hyprland_ext_image_copy_capture() {
        struct HeadlessGuard(&'static str);
        impl Drop for HeadlessGuard {
            fn drop(&mut self) {
                let _ = Command::new("hyprctl")
                    .args(["output", "remove", self.0])
                    .output();
            }
        }

        #[derive(Default)]
        struct CaptureEvents {
            surfaces: Vec<(u32, u32, u32)>,
            video_frames: usize,
            total_bytes: usize,
        }

        extern "C" fn on_surface(user: *mut c_void, surface: *const frame_abi::frame_host_surface) {
            if user.is_null() || surface.is_null() {
                return;
            }
            let log = unsafe { &*(user as *const Mutex<CaptureEvents>) };
            let surface = unsafe { &*surface };
            log.lock()
                .unwrap()
                .surfaces
                .push((surface.id, surface.width, surface.height));
        }

        extern "C" fn on_video(user: *mut c_void, _surface: u32, data: *const u8, size: usize) {
            if user.is_null() || data.is_null() {
                return;
            }
            let log = unsafe { &*(user as *const Mutex<CaptureEvents>) };
            let mut guard = log.lock().unwrap();
            guard.video_frames += 1;
            guard.total_bytes += size;
        }

        extern "C" fn on_msg(_user: *mut c_void, _data: *const u8, _size: usize) {}
        extern "C" fn on_removed(_user: *mut c_void, _id: u32) {}
        extern "C" fn on_region(_user: *mut c_void, _id: u32, _region: *const f32) {}

        let _ = Command::new("hyprctl")
            .args(["output", "remove", "FRAME-V1"])
            .output();
        let _guard = HeadlessGuard("FRAME-V1");

        assert!(
            supports_ext_image_copy_capture(),
            "Compositor lacks ext-image-copy-capture-v1"
        );
        gstreamer::init().expect("gstreamer init");

        let backend = crate::compositor::hyprland::HyprlandBackend::from_env()
            .expect("connect to Hyprland IPC");
        let initial_monitors =
            crate::compositor::CompositorBackend::list_monitors(&backend).expect("list monitors");
        let pin_updates: Vec<OutputConfigUpdate> = initial_monitors
            .iter()
            .filter(|m| !capture::is_virtual_output_name(&m.name))
            .map(|m| OutputConfigUpdate {
                name: m.name.clone(),
                mode: None,
                pos: Some(m.logical_pos),
                scale: Some(m.scale),
            })
            .collect();

        let create = Command::new("hyprctl")
            .args(["output", "create", "headless", "FRAME-V1"])
            .output()
            .expect("create FRAME-V1");
        assert_eq!(String::from_utf8_lossy(&create.stdout).trim(), "ok");

        std::thread::sleep(Duration::from_millis(100));
        let mut updates = pin_updates.clone();
        updates.push(OutputConfigUpdate {
            name: "FRAME-V1".into(),
            mode: Some((1920, 1080, 60)),
            pos: Some((0, -1080)),
            scale: Some(1.0),
        });
        apply_output_layout(&updates).expect("configure FRAME-V1 1920x1080@60 above");

        let events: Box<Mutex<CaptureEvents>> = Box::default();
        let user_ptr = Box::into_raw(events);
        let callbacks = frame_abi::frame_host_callbacks {
            user: user_ptr.cast(),
            surface: Some(on_surface),
            surface_region: Some(on_region),
            surface_removed: Some(on_removed),
            video: Some(on_video),
            message: Some(on_msg),
        };
        let output = Output::from_callbacks(callbacks);
        let shared = Arc::new(Shared::default());
        let cancel = Arc::new(AtomicBool::new(false));

        let output_thread = output.clone();
        let shared_thread = shared.clone();
        let cancel_thread = cancel.clone();
        let handle = std::thread::spawn(move || {
            stream_virtual_output(
                wayvr_ipc::frame::VIRTUAL_SURFACE_BASE,
                "FRAME-V1",
                &cancel_thread,
                &output_thread,
                &shared_thread,
            )
        });

        let events_ref = unsafe { &*user_ptr };
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut got_frame = false;
        while Instant::now() < deadline {
            let guard = events_ref.lock().unwrap();
            if !guard.surfaces.is_empty() && guard.video_frames > 0 && guard.total_bytes > 64 {
                got_frame = true;
                println!(
                    "ext-image-copy-capture announced {:?}, frames={}, total_bytes={}",
                    guard.surfaces, guard.video_frames, guard.total_bytes
                );
                break;
            }
            drop(guard);
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(got_frame, "expected encoded frame from ext-image-copy-capture");

        // Reposition FRAME-V1 live while capture session runs
        let mut reposition_updates = pin_updates;
        reposition_updates.push(OutputConfigUpdate {
            name: "FRAME-V1".into(),
            mode: Some((1920, 1080, 60)),
            pos: Some((2560, 0)),
            scale: Some(1.0),
        });
        apply_output_layout(&reposition_updates).expect("reposition FRAME-V1 while capturing");
        std::thread::sleep(Duration::from_millis(200));

        cancel.store(true, Ordering::Release);
        let res = handle.join().expect("join capture thread");
        assert!(res.is_ok(), "capture returned error: {res:?}");
        drop(output);
        unsafe {
            drop(Box::from_raw(user_ptr));
        }
    }
}
