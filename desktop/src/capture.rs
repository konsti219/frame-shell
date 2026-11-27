//! Each monitor through its own screen cast portal session and hardware H.264/HEVC encoder.
use crate::{Output, Shared, lock, notify};
use anyhow::{Context, Result, bail};
use gstreamer::{self as gst, prelude::*};
use gstreamer_app::AppSink;
use std::{
    collections::HashMap,
    io,
    os::fd::{AsRawFd, OwnedFd},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use wayvr_ipc::{
    frame::{DesktopSettings, HostMessage, MonitorStatus},
    metrics,
};
use wlx_capture::pipewire::{
    ScreenCastError, ScreenCastManager, ScreenCastParams, ScreenCastResult,
};

/// Virtual headless outputs managed by Frame Shell use this connector prefix.
pub const VIRTUAL_OUTPUT_PREFIX: &str = "FRAME-V";

pub fn is_virtual_output_name(name: &str) -> bool {
    name.starts_with(VIRTUAL_OUTPUT_PREFIX)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputGeometry {
    pub name: String,
    pub logical_pos: (i32, i32),
    pub logical_size: (i32, i32),
    pub physical_size: (i32, i32),
}

fn normalized_rect(output: &OutputGeometry) -> Option<(i32, i32, i32, i32)> {
    let (mut x, mut y) = output.logical_pos;
    let (mut w, mut h) = output.logical_size;
    if w < 0 {
        x += w;
        w = -w;
    }
    if h < 0 {
        y += h;
        h = -h;
    }
    (w > 0 && h > 0).then_some((x, y, w, h))
}

/// Computes the global bounding box `(origin, extent)` and normalized `[x, y, w, h]` regions per output.
pub fn compute_layout(
    outputs: &[OutputGeometry],
) -> Option<((i32, i32), (i32, i32), HashMap<String, [f32; 4]>)> {
    let rects: Vec<(&str, (i32, i32, i32, i32))> = outputs
        .iter()
        .filter_map(|o| normalized_rect(o).map(|rect| (o.name.as_str(), rect)))
        .collect();
    if rects.is_empty() {
        return None;
    }
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for &(_, (x, y, w, h)) in &rects {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x + w);
        max_y = max_y.max(y + h);
    }
    let origin = (min_x, min_y);
    let extent = ((max_x - min_x).max(1), (max_y - min_y).max(1));
    let mut regions = HashMap::with_capacity(rects.len());
    for (name, (x, y, w, h)) in rects {
        regions.insert(
            name.to_owned(),
            [
                (x - origin.0) as f32 / extent.0 as f32,
                (y - origin.1) as f32 / extent.1 as f32,
                w as f32 / extent.0 as f32,
                h as f32 / extent.1 as f32,
            ],
        );
    }
    Some((origin, extent, regions))
}

#[derive(Default)]
pub struct OutputLayout {
    outputs: Vec<OutputGeometry>,
    origin: (i32, i32),
    extent: (i32, i32),
    regions: HashMap<String, [f32; 4]>,
    active_streams: HashMap<u32, String>,
}

impl OutputLayout {
    pub fn update(&mut self, mut outputs: Vec<OutputGeometry>) -> Vec<(u32, [f32; 4])> {
        outputs.sort_by_key(|output| natural_key(&output.name));
        if self.outputs == outputs {
            return Vec::new();
        }
        self.outputs = outputs;
        let Some((origin, extent, regions)) = compute_layout(&self.outputs) else {
            self.origin = (0, 0);
            self.extent = (0, 0);
            self.regions.clear();
            return Vec::new();
        };
        self.origin = origin;
        self.extent = extent;
        self.regions = regions;
        eprintln!(
            "Desktop layout: origin {:?}, extent {:?}, {} outputs",
            self.origin,
            self.extent,
            self.outputs.len()
        );
        let mut updates = Vec::new();
        for (&id, name) in &self.active_streams {
            if let Some(&region) = self.regions.get(name) {
                updates.push((id, region));
            }
        }
        updates
    }

    pub fn register_stream(&mut self, id: u32, output_name: String) {
        self.active_streams.insert(id, output_name);
    }

    pub fn unregister_stream(&mut self, id: u32) {
        self.active_streams.remove(&id);
    }

    pub fn region_for_output(&self, output_name: &str) -> Option<[f32; 4]> {
        self.regions.get(output_name).copied()
    }

    pub fn region_for_stream(&self, id: u32) -> Option<[f32; 4]> {
        let name = self.active_streams.get(&id)?;
        self.regions.get(name).copied()
    }

    pub fn outputs_snapshot(&self) -> Vec<OutputGeometry> {
        self.outputs.clone()
    }

    pub fn stream_id_for_output(&self, output_name: &str) -> Option<u32> {
        self.active_streams
            .iter()
            .find_map(|(&id, name)| (name == output_name).then_some(id))
    }
}

pub fn snapshot_outputs(wayland: &wlx_capture::wayland::WlxClient) -> Vec<OutputGeometry> {
    // Filter valid logical rectangles directly instead of reading `output.done` / `get_desktop_origin`, because
    // `WlxClient` leaves `done == false` for `(0, 0)` outputs when position precedes size.
    let mut outputs: Vec<OutputGeometry> = wayland
        .outputs
        .iter()
        .filter_map(|(_, output)| {
            if output.name.is_empty()
                || output.name.as_ref() == "Unknown"
                || output.logical_size.0 == 0
                || output.logical_size.1 == 0
            {
                return None;
            }
            Some(OutputGeometry {
                name: output.name.to_string(),
                logical_pos: output.logical_pos,
                logical_size: output.logical_size,
                physical_size: output.size,
            })
        })
        .collect();
    outputs.sort_by_key(|output| natural_key(&output.name));
    outputs
}

fn poll_wayland_outputs(
    wayland: &mut wlx_capture::wayland::WlxClient,
    output: &Output,
    shared: &Shared,
) {
    for _ in 0..4 {
        let _ = wayland.connection.flush();
        let before = wayland.outputs.len();
        wayland.dispatch_pending();
        let events = wayland.iter_events().count();
        if events == 0 && wayland.outputs.len() == before {
            break;
        }
    }
    shared.update_outputs(output, snapshot_outputs(wayland));
}

fn watch_wayland_outputs(
    wayland: &mut wlx_capture::wayland::WlxClient,
    output: &Output,
    shared: &Shared,
) {
    while !output.stopped() {
        poll_wayland_outputs(wayland, output, shared);
        if !output.sleep(Duration::from_millis(50)) {
            break;
        }
    }
}

fn resolve_portal_monitor(
    mapping_id: Option<&str>,
    position: Option<(i32, i32)>,
    expected: Option<usize>,
    monitors: &[Monitor],
) -> std::result::Result<Option<usize>, String> {
    if let Some(name) = mapping_id.filter(|name| is_virtual_output_name(name)) {
        return Err(format!(
            "Virtual output {name} cannot be used as a physical monitor"
        ));
    }
    Ok(mapping_id
        .and_then(|name| monitors.iter().position(|monitor| monitor.name == name))
        .or_else(|| {
            position.and_then(|position| {
                monitors
                    .iter()
                    .position(|monitor| monitor.position == position)
            })
        })
        .or(expected))
}

struct StreamRegistration<'a> {
    id: u32,
    shared: &'a Shared,
}

impl Drop for StreamRegistration<'_> {
    fn drop(&mut self) {
        self.shared.unregister_stream(self.id);
    }
}

/// One monitor: where it lies in the whole desktop (for pointer input) and its output name.
#[derive(Clone)]
struct Monitor {
    name: String,
    position: (i32, i32),
    region: [f32; 4],
    pixels: f64,
}

/// Restore tokens per output ("<output> <token>" lines)
struct Tokens {
    path: PathBuf,
    outputs: Vec<(String, String)>,
}

impl Tokens {
    fn load(path: PathBuf) -> Self {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let outputs = text
            .lines()
            .filter_map(|line| line.trim().split_once(' '))
            .map(|(output, token)| (output.to_owned(), token.trim().to_owned()))
            .collect();
        Self { path, outputs }
    }
    fn get(&self, output: &str) -> Option<&str> {
        self.outputs
            .iter()
            .find(|(name, _)| name == output)
            .map(|(_, token)| token.as_str())
    }
    fn set(&mut self, output: &str, token: String) -> io::Result<()> {
        self.outputs.retain(|(name, _)| name != output);
        self.outputs.push((output.to_owned(), token));
        let text: String = self
            .outputs
            .iter()
            .map(|(name, token)| format!("{name} {token}\n"))
            .collect();
        std::fs::write(&self.path, text)
    }
}

/// FRAME_SHELL_CAPTURE_TOKEN, else the user's state directory
fn token_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("FRAME_SHELL_CAPTURE_TOKEN") {
        return Ok(path.into());
    }
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .context("No HOME")?;
    let dir = state.join("frame-desktop");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("capture-token"))
}

pub fn run(output: &Output, shared: &Shared) {
    if let Err(error) = capture(output, shared) {
        eprintln!("Desktop capture: {error:#}");
        shared.capture(output, |status| status.failed = Some(format!("{error:#}")));
        output.send(&HostMessage::Error(format!(
            "PC desktop capture failed: {error:#}"
        )));
    }
}

fn capture(output: &Output, shared: &Shared) -> Result<()> {
    // Monitors come from the compositor; each gets its own portal session and token, like WayVR's screens.
    // Keep `WlxClient` alive so live output hotplugs/moves update pointer regions without new portal prompts.
    let mut wayland = wlx_capture::wayland::WlxClient::new().context("No Wayland compositor")?;
    let snapshot = snapshot_outputs(&wayland);
    shared.update_outputs(output, snapshot.clone());
    let mut monitors: Vec<Monitor> = snapshot
        .into_iter()
        .filter(|geo| !is_virtual_output_name(&geo.name))
        .map(|geo| {
            let size = if geo.physical_size.0 > 0 && geo.physical_size.1 > 0 {
                geo.physical_size
            } else {
                geo.logical_size
            };
            Monitor {
                region: shared
                    .region_for_output(&geo.name)
                    .unwrap_or([0.0, 0.0, 1.0, 1.0]),
                name: geo.name,
                position: geo.logical_pos,
                pixels: f64::from(size.0.max(1)) * f64::from(size.1.max(1)),
            }
        })
        .collect();
    monitors.sort_by_key(|monitor| natural_key(&monitor.name));
    if monitors.is_empty() {
        bail!("No monitors");
    }
    for (id, monitor) in monitors.iter().enumerate() {
        eprintln!(
            "Desktop surface {id}: {}, region {:?}",
            monitor.name, monitor.region
        );
    }
    shared.capture(output, |status| {
        status.failed = None;
        status.monitors = monitors
            .iter()
            .map(|monitor| (monitor.name.clone(), MonitorStatus::Queued))
            .collect();
    });

    let mut tokens = Tokens::load(token_path()?);
    let mut portal = ScreenCastManager::new()?;
    let mut streaming = vec![false; monitors.len()];
    let mut tried = vec![false; monitors.len()];
    std::thread::scope(|scope| -> Result<()> {
        scope.spawn(move || {
            watch_wayland_outputs(&mut wayland, output, shared);
        });
        // The portal handles one request at a time, so ask one by one: known tokens first, dialogs last
        while !output.stopped() {
            let next = (0..monitors.len())
                .filter(|&index| !streaming[index] && !tried[index])
                .min_by_key(|&index| tokens.get(&monitors[index].name).is_none())
                .map(|index| {
                    tried[index] = true;
                    (
                        Some(index),
                        tokens.get(&monitors[index].name).map(str::to_owned),
                    )
                });
            let Some((expected, token)) = next else { break };
            let wanted = expected.map_or("a monitor", |index| monitors[index].name.as_str());
            let restoring = token.is_some();
            if !restoring {
                eprintln!("Pick {wanted} in the screen sharing dialog");
            }
            let request = portal.request(ScreenCastParams {
                token: token.map(Rc::<str>::from),
                embed_mouse: true,
                screens_only: true,
                persist: true,
                allow_multiple: false,
            })?;
            // Shown while the portal's dialog waits, closed when this request is done. A restore token
            // usually answers without a dialog, so only a request still waiting after a while has one.
            let mut notification = None;
            let requested = Instant::now();
            let response = loop {
                if output.stopped() {
                    break None;
                }
                match portal.check(&request) {
                    ScreenCastResult::Ok(response) => break Some(response),
                    ScreenCastResult::Failed(error) => {
                        eprintln!("Desktop selection: {error}");
                        if let Some(index) = expected {
                            let reason = match error {
                                ScreenCastError::PortalResponse(1) => {
                                    "Screen sharing was declined".to_owned()
                                }
                                error => format!("Screen sharing failed: {error}"),
                            };
                            shared.monitor(
                                output,
                                &monitors[index].name,
                                MonitorStatus::Failed(reason),
                            );
                        }
                        break None;
                    }
                    ScreenCastResult::WaitingForUser
                        if notification.is_none()
                            && (!restoring || requested.elapsed() > Duration::from_millis(500)) =>
                    {
                        if let Some(index) = expected {
                            shared.monitor(output, &monitors[index].name, MonitorStatus::Dialog);
                        }
                        notification = Some(
                            notify::Notification::show(
                                &format!("Pick {wanted} to share"),
                                &format!(
                                    "The headset asks for {wanted} in the screen sharing dialog."
                                ),
                            )
                            .inspect_err(|error| eprintln!("Screen cast notification: {error:#}"))
                            .ok(),
                        );
                    }
                    _ => std::thread::sleep(Duration::from_millis(10)),
                }
            };
            drop(notification);
            let Some(response) = response.filter(|_| !output.stopped()) else {
                continue;
            };
            let Some(selected) = response.streams.first() else {
                if let Some(index) = expected {
                    let reason = MonitorStatus::Failed("No monitor was picked".into());
                    shared.monitor(output, &monitors[index].name, reason);
                }
                continue;
            };
            // The user may pick another monitor than asked for. Match the connector name first,
            // since Hyprland reports the same position for every monitor, and reject virtual outputs.
            let index = match resolve_portal_monitor(
                selected.mapping_id.as_deref(),
                selected.position,
                expected,
                &monitors,
            ) {
                Ok(Some(index)) => index,
                Ok(None) => {
                    eprintln!(
                        "Desktop stream at {:?} matches no monitor",
                        selected.position
                    );
                    continue;
                }
                Err(reason) => {
                    eprintln!("{reason}");
                    if let Some(expected) = expected {
                        shared.monitor(
                            output,
                            &monitors[expected].name,
                            MonitorStatus::Failed(reason),
                        );
                    }
                    continue;
                }
            };
            if streaming[index] {
                eprintln!("{} is already selected", monitors[index].name);
                if let Some(expected) = expected {
                    let reason = format!("{} was already selected", monitors[index].name);
                    shared.monitor(
                        output,
                        &monitors[expected].name,
                        MonitorStatus::Failed(reason),
                    );
                }
                continue;
            }
            if let Some(token) = response.restore_token {
                tokens.set(&monitors[index].name, token)?;
            }
            if let Some(expected) = expected.filter(|&expected| expected != index) {
                tried[expected] = false;
                shared.monitor(output, &monitors[expected].name, MonitorStatus::Queued);
            }
            streaming[index] = true;
            shared.monitor(output, &monitors[index].name, MonitorStatus::Streaming);
            let (monitor, node, remote) =
                (&monitors[index], selected.node_id, response.remote.clone());
            scope.spawn(move || {
                let id = index as u32;
                shared.register_stream(id, monitor.name.clone());
                let registration = StreamRegistration { id, shared };
                if let Err(error) = stream(id, node, &remote, monitor, output, shared) {
                    eprintln!("Desktop surface {id} ({}): {error:#}", monitor.name);
                    metrics::event(&monitor.name, &format!("{error:#}"));
                    let reason = MonitorStatus::Failed(format!("Stopped streaming: {error:#}"));
                    shared.monitor(output, &monitor.name, reason);
                    output.send(&HostMessage::Error(format!(
                        "{} stopped streaming: {error:#}",
                        monitor.name
                    )));
                }
                drop(registration);
                output.removed(id);
            });
        }
        Ok(())
    })?;
    // Portal sessions live as long as this connection
    drop(portal);
    Ok(())
}

pub(crate) fn scaled_bitrate(settings: DesktopSettings, width: u32, height: u32) -> u32 {
    let pixels = f64::from(width.max(1)) * f64::from(height.max(1));
    ((f64::from(settings.bitrate_per_1080p) * pixels / (1920. * 1080.)) as u32).clamp(2000, 150000)
}

fn monitor_bitrate(settings: DesktopSettings, monitor: &Monitor) -> u32 {
    ((f64::from(settings.bitrate_per_1080p) * monitor.pixels / (1920. * 1080.)) as u32)
        .clamp(2000, 150000)
}

pub(crate) fn encoder_tail(hevc: bool, bitrate: u32) -> Result<(&'static str, String)> {
    let (codec, parse, caps) = if hevc {
        ("h265", "h265parse", "video/x-h265")
    } else {
        ("h264", "h264parse", "video/x-h264")
    };
    // Keyframes come on request when the headset loses a frame; periodic ones would only be large bursts (VA caps at 1024).
    // VBR with the target at the configured rate: CBR pads a static desktop to the full rate over Wi-Fi
    let encoder = if gst::ElementFactory::find(&format!("va{codec}enc")).is_some() {
        format!(
            "vapostproc ! video/x-raw,format=NV12 ! va{codec}enc name=encoder rate-control=vbr target-percentage=100 bitrate={bitrate} key-int-max=1024 b-frames=0"
        )
    } else if gst::ElementFactory::find(&format!("nv{codec}enc")).is_some() {
        format!(
            "videoconvert ! video/x-raw,format=NV12 ! nv{codec}enc name=encoder bitrate={bitrate} gop-size=1200 bframes=0 zerolatency=true"
        )
    } else {
        bail!("No hardware {codec} encoder available (va{codec}enc/nv{codec}enc)");
    };
    Ok((
        codec,
        format!("{encoder} ! {parse} config-interval=-1 ! {caps},stream-format=byte-stream,alignment=au"),
    ))
}

pub(crate) fn install_live_latency_probe(pipeline: &gst::Pipeline) {
    // VA encoders hold 4 frames unless upstream is live when their format is set, and sources only report live
    // once their stream properties arrive: a held desktop frame waits for 4 more changes or keepalives.
    if let Some(encoder) = pipeline.by_name("encoder")
        && let Some(pad) = encoder.static_pad("sink")
    {
        pad.add_probe(gst::PadProbeType::QUERY_UPSTREAM, |_, info| {
            if let Some(gst::PadProbeData::Query(query)) = &mut info.data
                && let gst::QueryViewMut::Latency(latency) = query.view_mut()
            {
                latency.set(true, gst::ClockTime::ZERO, gst::ClockTime::NONE);
                return gst::PadProbeReturn::Handled;
            }
            gst::PadProbeReturn::Ok
        });
    }
}

pub(crate) fn force_keyframe(pipeline: &gst::Pipeline) {
    pipeline.send_event(gst::event::CustomUpstream::new(
        gst::Structure::builder("GstForceKeyUnit")
            .field("running-time", u64::MAX)
            .field("all-headers", true)
            .field("count", 0u32)
            .build(),
    ));
}

/// Captures and encodes one monitor at its own pixel size until stopped; a codec change rebuilds the pipeline on
/// the same PipeWire node, a bitrate change is applied to the running encoder.
fn stream(
    id: u32,
    node: u32,
    remote: &OwnedFd,
    monitor: &Monitor,
    output: &Output,
    shared: &Shared,
) -> Result<()> {
    while !output.stopped() {
        stream_codec(id, node, remote, monitor, output, shared, shared.settings())?;
    }
    Ok(())
}

/// One pipeline for one codec, until the codec changes or the host stops
fn stream_codec(
    id: u32,
    node: u32,
    remote: &OwnedFd,
    monitor: &Monitor,
    output: &Output,
    shared: &Shared,
    settings: DesktopSettings,
) -> Result<()> {
    let bitrate = monitor_bitrate(settings, monitor);
    let (codec, tail) = encoder_tail(settings.hevc, bitrate)?;
    // DMA-bufs when the encoder path takes them: a shared-memory cast has the compositor read every frame back
    // (tens of ms on Intel iGPUs). Load is shed by the leaky raw queue: a dropped encoded frame would corrupt the
    // stream until the next keyframe without the headset seeing a loss.
    let fd = remote.as_raw_fd();
    let pipeline = gst::parse::launch(&format!(
        "pipewiresrc name=source fd={fd} path={node} do-timestamp=true keepalive-time=100 ! queue max-size-buffers=2 leaky=downstream ! videorate name=capture_rate drop-only=true ! video/x-raw(memory:DMABuf),framerate=60/1;video/x-raw,framerate=60/1 ! {tail} ! appsink name=encoded sync=false max-buffers=2 drop=false"
    ))?
    .downcast::<gst::Pipeline>()
    .map_err(|_| anyhow::anyhow!("Invalid capture pipeline"))?;
    install_live_latency_probe(&pipeline);
    let sink = pipeline
        .by_name("encoded")
        .unwrap()
        .downcast::<AppSink>()
        .unwrap();
    // KWin's PTS is the output's last presentation, stale while a monitor is off: with a clock, pipewiresrc would
    // sleep every frame by that age, so run clockless and stamp the time of capture (keepalives have no PTS then)
    pipeline.use_clock(None::<&gst::Clock>);
    let started = Instant::now();
    // A keepalive repeats the last PipeWire buffer, so its seq repeats too
    let name = monitor.name.clone();
    pipeline
        .by_name("source")
        .unwrap()
        .static_pad("src")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if let Some(gst::PadProbeData::Buffer(buffer)) = &mut info.data {
                let buffer = buffer.make_mut();
                let now = gst::ClockTime::from_nseconds(started.elapsed().as_nanos() as u64);
                buffer.set_pts(now);
                buffer.set_dts(now);
                metrics::emit(&name, "captured", buffer.offset(), buffer.size() as u64);
            }
            gst::PadProbeReturn::Ok
        })
        .unwrap();
    let capture_enabled = Arc::new(AtomicBool::new(true));
    let enabled = capture_enabled.clone();
    let name = monitor.name.clone();
    // Keep rate limiting active while hidden so reopening does not produce catch-up bursts.
    pipeline
        .by_name("capture_rate")
        .unwrap()
        .static_pad("src")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if enabled.load(Ordering::Acquire) {
                if let Some(buffer) = info.buffer() {
                    metrics::emit(&name, "to_encoder", buffer.offset(), buffer.size() as u64);
                }
                gst::PadProbeReturn::Ok
            } else {
                gst::PadProbeReturn::Drop
            }
        })
        .unwrap();
    pipeline.set_state(gst::State::Playing)?;
    eprintln!("Desktop surface {id}: {codec} at {} Mbit/s", bitrate / 1000);
    metrics::event(&monitor.name, &format!("{codec} at {bitrate} kbit/s"));
    let force_keyframe_cb = || force_keyframe(&pipeline);
    let result = (|| -> Result<()> {
        let mut announced = None;
        let mut playing = true;
        let mut applied_bitrate = bitrate;
        while !output.stopped() {
            let current = shared.settings();
            if current.hevc != settings.hevc {
                return Ok(());
            }
            if monitor_bitrate(current, monitor) != applied_bitrate {
                applied_bitrate = monitor_bitrate(current, monitor);
                pipeline
                    .by_name("encoder")
                    .context("Missing encoder")?
                    .set_property("bitrate", applied_bitrate);
                eprintln!("Desktop surface {id}: {} Mbit/s", applied_bitrate / 1000);
            }
            let visible = lock(&shared.visibility).get(&id).copied().unwrap_or(true);
            if announced.is_some() && visible != playing {
                // Drop only buffers: changing a valve's drop property renegotiates and stalls PipeWire.
                if visible {
                    while sink.try_pull_sample(gst::ClockTime::ZERO).is_some() {}
                    force_keyframe_cb();
                }
                capture_enabled.store(visible, Ordering::Release);
                playing = visible;
                let state = if visible {
                    "encoding"
                } else {
                    "encoding stopped"
                };
                metrics::event(&monitor.name, state);
                eprintln!(
                    "Desktop surface {id}: {}",
                    if playing {
                        "streaming"
                    } else {
                        "hidden; encoding stopped"
                    }
                );
            }
            if !playing {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            // The headset lost video and waits for a keyframe
            if lock(&shared.keyframes).remove(&id) {
                metrics::event(&monitor.name, "keyframe requested");
                force_keyframe_cb();
            }
            if let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(100)) {
                // Announce (again) with the encoded size and codec before the first frame of each pipeline and size
                let caps = sample.caps().context("Encoded frame without caps")?;
                let structure = caps.structure(0).context("Empty caps")?;
                let size = (
                    u32::try_from(structure.get::<i32>("width")?)?,
                    u32::try_from(structure.get::<i32>("height")?)?,
                );
                if announced != Some(size) {
                    let region = shared
                        .region_for_output(&monitor.name)
                        .unwrap_or(monitor.region);
                    output.surface(id, size, region, &monitor.name, settings.hevc);
                    shared.sync_surface_region(output, id);
                    announced = Some(size);
                }
                let buffer = sample.buffer().context("Empty encoded frame")?;
                let map = buffer.map_readable()?;
                let bytes = map.len() as u64;
                if !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT) {
                    metrics::emit(&monitor.name, "keyframe", buffer.offset(), bytes);
                }
                metrics::emit(&monitor.name, "encoded", buffer.offset(), bytes);
                if map.len() > 4 * 1024 * 1024 {
                    bail!("Desktop frame exceeds 4 MiB");
                }
                output.video(id, &map);
                metrics::emit(&monitor.name, "sent", buffer.offset(), bytes);
            }
            if let Some(message) = pipeline.bus().unwrap().pop_filtered(&[
                gst::MessageType::Error,
                gst::MessageType::Eos,
                gst::MessageType::Warning,
            ]) {
                if message.type_() != gst::MessageType::Warning {
                    bail!("Desktop capture stopped: {message:?}");
                }
                metrics::event(&monitor.name, &format!("{message:?}"));
            }
        }
        Ok(())
    })();
    pipeline.set_state(gst::State::Null)?;
    result
}

/// Sort key for output names: DP-1, DP-2, ..., DP-10 in numeric order
fn natural_key(name: &str) -> (String, u64) {
    let digits = name.len() - name.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let (prefix, number) = name.split_at(name.len() - digits);
    (prefix.to_owned(), number.parse().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_region(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(b.iter()).all(|(x, y)| (x - y).abs() < 1e-4)
    }

    fn geo(name: &str, x: i32, y: i32, w: i32, h: i32) -> OutputGeometry {
        OutputGeometry {
            name: name.into(),
            logical_pos: (x, y),
            logical_size: (w, h),
            physical_size: (w, h),
        }
    }

    #[test]
    fn compute_layout_negative_origin_and_virtual_above() {
        let base = vec![
            geo("HDMI-A-2", -1920, 0, 1920, 1080),
            geo("DP-4", 0, 0, 2560, 1080),
        ];
        let (origin, extent, regions) = compute_layout(&base).unwrap();
        assert_eq!(origin, (-1920, 0));
        assert_eq!(extent, (4480, 1080));
        assert!(approx_region(
            regions["HDMI-A-2"],
            [0.0, 0.0, 1920.0 / 4480.0, 1.0]
        ));
        assert!(approx_region(
            regions["DP-4"],
            [1920.0 / 4480.0, 0.0, 2560.0 / 4480.0, 1.0]
        ));

        let with_virtual = vec![
            geo("HDMI-A-2", -1920, 0, 1920, 1080),
            geo("DP-4", 0, 0, 2560, 1080),
            geo("FRAME-V1", 0, -1080, 1920, 1080),
        ];
        let (origin, extent, regions) = compute_layout(&with_virtual).unwrap();
        assert_eq!(origin, (-1920, -1080));
        assert_eq!(extent, (4480, 2160));
        assert!(approx_region(
            regions["DP-4"],
            [1920.0 / 4480.0, 0.5, 2560.0 / 4480.0, 0.5]
        ));
        assert!(approx_region(
            regions["FRAME-V1"],
            [1920.0 / 4480.0, 0.0, 1920.0 / 4480.0, 0.5]
        ));
    }

    #[test]
    fn compute_layout_l_shaped_scaled_and_zero_ignored() {
        let outputs = vec![
            geo("Unknown", 0, 0, 0, 0),
            geo("DP-1", 0, 0, 2560, 1440),
            geo("FRAME-V1", 320, -1440, 1280, 1440),
        ];
        let (origin, extent, regions) = compute_layout(&outputs).unwrap();
        assert_eq!(origin, (0, -1440));
        assert_eq!(extent, (2560, 2880));
        assert!(approx_region(regions["DP-1"], [0.0, 0.5, 1.0, 0.5]));
        assert!(approx_region(
            regions["FRAME-V1"],
            [320.0 / 2560.0, 0.0, 0.5, 0.5]
        ));
        assert!(!regions.contains_key("Unknown"));
    }

    #[test]
    fn output_layout_updates_only_active_streams_and_restores() {
        let mut layout = OutputLayout::default();
        layout.register_stream(0, "DP-4".into());

        let base = vec![
            geo("HDMI-A-2", -1920, 0, 1920, 1080),
            geo("DP-4", 0, 0, 2560, 1080),
        ];
        let updates = layout.update(base.clone());
        assert_eq!(updates.len(), 1);
        let initial = updates[0].1;

        assert!(layout.update(base.clone()).is_empty());

        let moved = vec![
            geo("HDMI-A-2", -1920, 0, 1920, 1080),
            geo("DP-4", 0, 0, 2560, 1080),
            geo("FRAME-V1", 2560, 0, 1920, 1080),
        ];
        let updates = layout.update(moved);
        assert_eq!(updates.len(), 1);
        assert!(approx_region(updates[0].1, [1920.0 / 6400.0, 0.0, 2560.0 / 6400.0, 1.0]));

        let restored = layout.update(base);
        assert_eq!(restored.len(), 1);
        assert!(approx_region(restored[0].1, initial));
    }

    #[test]
    fn portal_guard_rejects_virtual_output_before_fallback() {
        let monitors = vec![Monitor {
            name: "DP-4".into(),
            position: (0, 0),
            region: [0.0, 0.0, 1.0, 1.0],
            pixels: 2560.0 * 1080.0,
        }];
        let rejected = resolve_portal_monitor(Some("FRAME-V1"), Some((0, 0)), Some(0), &monitors);
        assert!(rejected.is_err());

        let matched = resolve_portal_monitor(Some("DP-4"), None, None, &monitors);
        assert_eq!(matched, Ok(Some(0)));
    }

    #[ignore = "requires a running Hyprland desktop session"]
    #[test]
    fn live_hyprland_headless_output_regions() {
        use std::ffi::c_void;
        use std::process::Command;
        use std::sync::Mutex;

        struct HeadlessGuard(&'static str);
        impl Drop for HeadlessGuard {
            fn drop(&mut self) {
                let _ = Command::new("hyprctl")
                    .args(["output", "remove", self.0])
                    .output();
            }
        }

        extern "C" fn record_region(user: *mut c_void, id: u32, region: *const f32) {
            if user.is_null() || region.is_null() {
                return;
            }
            let log = unsafe { &*(user as *const Mutex<Vec<(u32, [f32; 4])>>) };
            let slice = unsafe { std::slice::from_raw_parts(region, 4) };
            log.lock()
                .unwrap()
                .push((id, [slice[0], slice[1], slice[2], slice[3]]));
        }

        let _ = Command::new("hyprctl")
            .args(["output", "remove", "FRAME-V1"])
            .output();
        let _guard = HeadlessGuard("FRAME-V1");

        let mut wayland =
            wlx_capture::wayland::WlxClient::new().expect("connect to Hyprland Wayland");
        let initial = snapshot_outputs(&wayland);
        let primary = initial
            .iter()
            .find(|o| !is_virtual_output_name(&o.name))
            .expect("at least one physical monitor")
            .clone();

        let events: Box<Mutex<Vec<(u32, [f32; 4])>>> = Box::default();
        let user_ptr = Box::into_raw(events);
        let callbacks = frame_abi::frame_host_callbacks {
            user: user_ptr.cast(),
            surface_region: Some(record_region),
            ..Default::default()
        };
        let output = Output::from_callbacks(callbacks);
        let shared = Shared::default();
        shared.register_stream(0, primary.name.clone());
        shared.update_outputs(&output, initial.clone());
        let initial_region = shared
            .region_for_output(&primary.name)
            .expect("initial primary region");

        let create = Command::new("hyprctl")
            .args(["output", "create", "headless", "FRAME-V1"])
            .output()
            .expect("run hyprctl output create");
        assert!(create.status.success(), "create failed: {:?}", create);
        assert_eq!(
            String::from_utf8_lossy(&create.stdout).trim(),
            "ok",
            "unexpected create output: {}",
            String::from_utf8_lossy(&create.stdout)
        );

        let place_virtual = |x: i32, y: i32| {
            let mut cmd = Command::new("wlr-randr");
            for mon in &initial {
                if !is_virtual_output_name(&mon.name) {
                    cmd.args([
                        "--output",
                        &mon.name,
                        "--pos",
                        &format!("{},{}", mon.logical_pos.0, mon.logical_pos.1),
                    ]);
                }
            }
            cmd.args([
                "--output",
                "FRAME-V1",
                "--mode",
                "1920x1080@60Hz",
                "--pos",
                &format!("{x},{y}"),
                "--scale",
                "1",
            ]);
            let status = cmd.status().expect("run wlr-randr");
            assert!(status.success(), "wlr-randr failed: {status}");
        };

        let above_y = primary.logical_pos.1 - 1080;
        place_virtual(primary.logical_pos.0, above_y);

        let events_ref = unsafe { &*user_ptr };
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut saw_above = None;
        while Instant::now() < deadline {
            poll_wayland_outputs(&mut wayland, &output, &shared);
            let latest = events_ref.lock().unwrap().last().copied();
            let current = shared.region_for_output(&primary.name);
            let v_region = shared.region_for_output("FRAME-V1");
            if v_region.is_some()
                && let (Some((_, r)), Some(cur)) = (latest, current)
                && approx_region(r, cur)
                && cur[1] > initial_region[1] + 0.05
            {
                saw_above = Some(cur);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let above_region = saw_above.expect("expected live region shift with FRAME-V1 above primary");
        println!(
            "Above placement: initial {:?} -> shifted {:?}",
            initial_region, above_region
        );

        let max_x = initial
            .iter()
            .map(|o| o.logical_pos.0 + o.logical_size.0)
            .max()
            .unwrap();
        place_virtual(max_x, 0);

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut saw_right = None;
        while Instant::now() < deadline {
            poll_wayland_outputs(&mut wayland, &output, &shared);
            let latest = events_ref.lock().unwrap().last().copied();
            let current = shared.region_for_output(&primary.name);
            if let (Some((_, r)), Some(cur)) = (latest, current)
                && approx_region(r, cur)
                && (cur[1] - initial_region[1]).abs() < 1e-4
                && cur[2] < initial_region[2] - 0.01
            {
                saw_right = Some(cur);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let right_region = saw_right.expect("expected live region shift with FRAME-V1 right");
        println!("Right placement: {:?}", right_region);

        drop(_guard);

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut restored = None;
        while Instant::now() < deadline {
            poll_wayland_outputs(&mut wayland, &output, &shared);
            let latest = events_ref.lock().unwrap().last().copied();
            let current = shared.region_for_output(&primary.name);
            if shared.region_for_output("FRAME-V1").is_none()
                && let (Some((_, r)), Some(cur)) = (latest, current)
                && approx_region(r, cur)
                && approx_region(cur, initial_region)
            {
                restored = Some(cur);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            restored.is_some(),
            "expected primary region to restore to {:?} after FRAME-V1 removal",
            initial_region
        );
        drop(output);
        unsafe {
            drop(Box::from_raw(user_ptr));
        }
    }
}
