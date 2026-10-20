use std::{
    process::Command,
    sync::{Arc, LazyLock, Mutex, mpsc},
    time::{Duration, Instant},
};
use wlx_common::dash_interface::{AudioControl, AudioEndpoint, HeadsetAudioState};

enum Request {
    Control(AudioControl),
    Step(bool),
}
struct Worker {
    sender: mpsc::Sender<Request>,
    state: Arc<Mutex<(HeadsetAudioState, Instant, u64)>>,
}
static WORKER: LazyLock<Worker> = LazyLock::new(|| {
    let (sender, receiver) = mpsc::channel();
    let state = Arc::new(Mutex::new((
        HeadsetAudioState::default(),
        Instant::now(),
        0,
    )));
    let shared = state.clone();
    std::thread::spawn(move || run(receiver, shared));
    Worker { sender, state }
});

pub fn snapshot() -> HeadsetAudioState {
    let mut state = WORKER.state.lock().unwrap();
    state.1 = Instant::now();
    state.0
}

pub fn control(control: AudioControl) {
    let mut state = WORKER.state.lock().unwrap();
    state.1 = Instant::now();
    state.2 += 1;
    match control {
        AudioControl::OutputVolume(value) => {
            if let Some(output) = &mut state.0.output {
                output.volume = value;
            }
        }
        AudioControl::OutputMute(value) => {
            if let Some(output) = &mut state.0.output {
                output.muted = value;
            }
        }
        AudioControl::MicrophoneVolume(value) => {
            if let Some(mic) = &mut state.0.microphone {
                mic.volume = value;
            }
        }
        AudioControl::MicrophoneMute(value) => {
            if let Some(mic) = &mut state.0.microphone {
                mic.muted = value;
            }
        }
    }
    let _ = WORKER.sender.send(Request::Control(control));
}

pub fn step_output(up: bool) {
    WORKER.state.lock().unwrap().2 += 1;
    let _ = WORKER.sender.send(Request::Step(up));
}

fn read(kind: &str) -> Option<(String, AudioEndpoint)> {
    let output = Command::new("pactl")
        .args(["-f", "json", "list", kind])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let nodes: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let node = nodes.as_array()?.iter().find(|node| {
        node["name"]
            .as_str()
            .is_some_and(|name| name.contains("platform-sound") && !name.ends_with(".monitor"))
    })?;
    let volumes = node["volume"].as_object()?;
    let mut total = 0.0;
    for volume in volumes.values() {
        total += volume["value_percent"]
            .as_str()?
            .trim_end_matches('%')
            .parse::<f32>()
            .ok()?;
    }
    if volumes.is_empty() {
        return None;
    }
    Some((
        node["name"].as_str()?.to_owned(),
        AudioEndpoint {
            volume: total / volumes.len() as f32,
            muted: node["mute"].as_bool()?,
        },
    ))
}

fn apply(command: &str, name: &str, value: String) {
    match Command::new("pactl").args([command, name, &value]).status() {
        Ok(status) if status.success() => {}
        _ => log::warn!("Headset audio command failed: {command}"),
    }
}

fn run(receiver: mpsc::Receiver<Request>, shared: Arc<Mutex<(HeadsetAudioState, Instant, u64)>>) {
    let mut output = read("sinks");
    let mut microphone = read("sources");
    let mut next_refresh = Instant::now();
    loop {
        let request = receiver.recv_timeout(Duration::from_millis(100));
        let mut requests = Vec::new();
        match request {
            Ok(request) => requests.push(request),
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        requests.extend(receiver.try_iter());
        let changed = !requests.is_empty();
        let generation = shared.lock().unwrap().2;
        let mut seen = [false; 4];
        requests.reverse();
        requests.retain(|request| {
            let Request::Control(control) = request else {
                return true;
            };
            let index = match control {
                AudioControl::OutputVolume(_) => 0,
                AudioControl::OutputMute(_) => 1,
                AudioControl::MicrophoneVolume(_) => 2,
                AudioControl::MicrophoneMute(_) => 3,
            };
            let keep = !seen[index];
            seen[index] = true;
            keep
        });
        requests.reverse();
        for request in requests {
            match request {
                Request::Control(AudioControl::OutputVolume(value)) => {
                    if let Some((name, _)) = &output {
                        apply(
                            "set-sink-volume",
                            name,
                            format!("{}%", value.round().clamp(0.0, 100.0)),
                        );
                    }
                }
                Request::Control(AudioControl::OutputMute(value)) => {
                    if let Some((name, _)) = &output {
                        apply("set-sink-mute", name, u8::from(value).to_string());
                    }
                }
                Request::Control(AudioControl::MicrophoneVolume(value)) => {
                    if let Some((name, _)) = &microphone {
                        apply(
                            "set-source-volume",
                            name,
                            format!("{}%", value.round().clamp(0.0, 100.0)),
                        );
                    }
                }
                Request::Control(AudioControl::MicrophoneMute(value)) => {
                    if let Some((name, _)) = &microphone {
                        apply("set-source-mute", name, u8::from(value).to_string());
                    }
                }
                Request::Step(up) => {
                    if let Some((name, endpoint)) = &mut output {
                        endpoint.volume =
                            (endpoint.volume + if up { 5.0 } else { -5.0 }).clamp(0.0, 100.0);
                        apply("set-sink-volume", name, format!("{}%", endpoint.volume));
                        crate::frame_shell::volume_feedback(endpoint.volume as u32);
                    }
                }
            }
        }
        let active = shared.lock().unwrap().1.elapsed() < Duration::from_secs(1);
        if changed || (active && Instant::now() >= next_refresh) {
            output = read("sinks");
            microphone = read("sources");
            let mut state = shared.lock().unwrap();
            if state.2 == generation {
                state.0 = HeadsetAudioState {
                    output: output.as_ref().map(|(_, state)| *state),
                    microphone: microphone.as_ref().map(|(_, state)| *state),
                };
            }
            next_refresh = Instant::now() + Duration::from_millis(500);
        }
    }
}
