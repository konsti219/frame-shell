//! Each monitor through its own screen cast portal session and hardware H.264/HEVC encoder.
use crate::{Output, Shared, lock, notify};
use anyhow::{Context, Result, bail};
use gstreamer::{self as gst, prelude::*};
use gstreamer_app::AppSink;
use std::{
    io,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use wayvr_ipc::frame::{DesktopSettings, HostMessage};
use wlx_capture::pipewire::{ScreenCastManager, ScreenCastParams, ScreenCastResult};

/// One monitor: where it lies in the whole desktop (for pointer input) and its output name.
#[derive(Clone)]
struct Monitor {
    name: String,
    position: (i32, i32),
    region: [f32; 4],
    pixels: f64,
}

/// Restore tokens per output ("<output> <token>" lines); a bare token is from the single-screen helper.
struct Tokens {
    path: PathBuf,
    outputs: Vec<(String, String)>,
    unassigned: Option<String>,
}

impl Tokens {
    fn load(path: PathBuf) -> Self {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut outputs = Vec::new();
        let mut unassigned = None;
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            match line.split_once(' ') {
                Some((output, token)) => outputs.push((output.to_owned(), token.trim().to_owned())),
                None => unassigned = Some(line.to_owned()),
            }
        }
        Self {
            path,
            outputs,
            unassigned,
        }
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
        output.send(&HostMessage::Error(format!(
            "PC desktop capture failed: {error:#}"
        )));
    }
}

fn capture(output: &Output, shared: &Shared) -> Result<()> {
    // Monitors come from the compositor; each gets its own portal session and token, like WayVR's screens
    let wayland = wlx_capture::wayland::WlxClient::new().context("No Wayland compositor")?;
    let origin = wayland.get_desktop_origin();
    let extent = wayland.get_desktop_extent();
    let mut monitors: Vec<Monitor> = wayland
        .outputs
        .iter()
        .map(|(_, output)| Monitor {
            name: output.name.to_string(),
            position: output.logical_pos,
            region: [
                (output.logical_pos.0 - origin.0) as f32 / extent.0.max(1) as f32,
                (output.logical_pos.1 - origin.1) as f32 / extent.1.max(1) as f32,
                output.logical_size.0 as f32 / extent.0.max(1) as f32,
                output.logical_size.1 as f32 / extent.1.max(1) as f32,
            ],
            pixels: f64::from(output.size.0.max(1)) * f64::from(output.size.1.max(1)),
        })
        .collect();
    drop(wayland);
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

    let mut tokens = Tokens::load(token_path()?);
    let mut portal = ScreenCastManager::new()?;
    let mut streaming = vec![false; monitors.len()];
    let mut tried = vec![false; monitors.len()];
    std::thread::scope(|scope| -> Result<()> {
        // The portal handles one request at a time, so ask one by one: known tokens first, dialogs last
        while !output.stopped() {
            let unassigned = tokens.unassigned.take();
            let next = unassigned.map(|token| (None, Some(token))).or_else(|| {
                let index = (0..monitors.len())
                    .filter(|&index| !streaming[index] && !tried[index])
                    .min_by_key(|&index| tokens.get(&monitors[index].name).is_none())?;
                tried[index] = true;
                Some((
                    Some(index),
                    tokens.get(&monitors[index].name).map(str::to_owned),
                ))
            });
            let Some((expected, token)) = next else { break };
            let wanted = expected.map_or("a monitor", |index| monitors[index].name.as_str());
            if token.is_none() {
                eprintln!("Pick {wanted} in the screen sharing dialog");
            }
            let request = portal.request(ScreenCastParams {
                token: token.map(Rc::<str>::from),
                embed_mouse: true,
                screens_only: true,
                persist: true,
                allow_multiple: false,
            })?;
            // Shown while the portal's dialog waits, closed when this request is done
            let mut notification = None;
            let response = loop {
                if output.stopped() {
                    break None;
                }
                match portal.check(&request) {
                    ScreenCastResult::Ok(response) => break Some(response),
                    ScreenCastResult::Failed(error) => {
                        eprintln!("Desktop selection: {error}");
                        break None;
                    }
                    ScreenCastResult::WaitingForUser if notification.is_none() => {
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
                continue;
            };
            // The user may pick another monitor than asked for; the stream's position says which one it is
            let Some(index) = selected
                .position
                .and_then(|position| {
                    monitors
                        .iter()
                        .position(|monitor| monitor.position == position)
                })
                .or(expected)
            else {
                eprintln!(
                    "Desktop stream at {:?} matches no monitor",
                    selected.position
                );
                continue;
            };
            if let Some(token) = response.restore_token {
                tokens.set(&monitors[index].name, token)?;
            }
            if std::mem::replace(&mut streaming[index], true) {
                eprintln!("{} is already streaming", monitors[index].name);
                // Picked the wrong monitor: ask for the wanted one again
                if let Some(expected) = expected.filter(|&expected| expected != index) {
                    tried[expected] = false;
                }
                continue;
            }
            let (monitor, node) = (&monitors[index], selected.node_id);
            scope.spawn(move || {
                let id = index as u32;
                if let Err(error) = stream(id, node, monitor, output, shared) {
                    eprintln!("Desktop surface {id} ({}): {error:#}", monitor.name);
                    output.send(&HostMessage::Error(format!(
                        "{} stopped streaming: {error:#}",
                        monitor.name
                    )));
                }
                output.removed(id);
            });
        }
        Ok(())
    })?;
    // Portal sessions live as long as this connection
    drop(portal);
    Ok(())
}

fn monitor_bitrate(settings: DesktopSettings, monitor: &Monitor) -> u32 {
    ((f64::from(settings.bitrate_per_1080p) * monitor.pixels / (1920. * 1080.)) as u32)
        .clamp(2000, 150000)
}

/// Captures and encodes one monitor at its own pixel size until stopped; a codec change rebuilds the pipeline on
/// the same PipeWire node, a bitrate change is applied to the running encoder.
fn stream(id: u32, node: u32, monitor: &Monitor, output: &Output, shared: &Shared) -> Result<()> {
    while !output.stopped() {
        stream_codec(id, node, monitor, output, shared, shared.settings())?;
    }
    Ok(())
}

/// One pipeline for one codec, until the codec changes or the host stops
fn stream_codec(
    id: u32,
    node: u32,
    monitor: &Monitor,
    output: &Output,
    shared: &Shared,
    settings: DesktopSettings,
) -> Result<()> {
    let bitrate = monitor_bitrate(settings, monitor);
    let (codec, parse, caps) = if settings.hevc {
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
    let pipeline = gst::parse::launch(&format!(
        "pipewiresrc path={node} do-timestamp=true keepalive-time=100 ! queue max-size-buffers=2 leaky=downstream ! videorate name=capture_rate drop-only=true ! video/x-raw,framerate=60/1 ! {encoder} ! {parse} config-interval=-1 ! {caps},stream-format=byte-stream,alignment=au ! appsink name=encoded sync=false max-buffers=2 drop=true"
    ))?
    .downcast::<gst::Pipeline>()
    .map_err(|_| anyhow::anyhow!("Invalid capture pipeline"))?;
    let sink = pipeline
        .by_name("encoded")
        .unwrap()
        .downcast::<AppSink>()
        .unwrap();
    let capture_enabled = Arc::new(AtomicBool::new(true));
    let enabled = capture_enabled.clone();
    // Keep rate limiting active while hidden so reopening does not produce catch-up bursts.
    pipeline
        .by_name("capture_rate")
        .unwrap()
        .static_pad("src")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            if enabled.load(Ordering::Acquire) {
                gst::PadProbeReturn::Ok
            } else {
                gst::PadProbeReturn::Drop
            }
        })
        .unwrap();
    pipeline.set_state(gst::State::Playing)?;
    eprintln!("Desktop surface {id}: {codec} at {} Mbit/s", bitrate / 1000);
    let force_keyframe = || {
        pipeline.send_event(gst::event::CustomUpstream::new(
            gst::Structure::builder("GstForceKeyUnit")
                .field("running-time", u64::MAX)
                .field("all-headers", true)
                .field("count", 0u32)
                .build(),
        ));
    };
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
                    force_keyframe();
                }
                capture_enabled.store(visible, Ordering::Release);
                playing = visible;
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
                force_keyframe();
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
                    output.surface(id, size, monitor.region, &monitor.name, settings.hevc);
                    announced = Some(size);
                }
                let map = sample
                    .buffer()
                    .context("Empty encoded frame")?
                    .map_readable()?;
                if map.len() > 4 * 1024 * 1024 {
                    bail!("Desktop frame exceeds 4 MiB");
                }
                output.video(id, &map);
            }
            if let Some(message) = pipeline
                .bus()
                .unwrap()
                .pop_filtered(&[gst::MessageType::Error, gst::MessageType::Eos])
            {
                bail!("Desktop capture stopped: {message:?}");
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
