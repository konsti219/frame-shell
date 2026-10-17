//! The Music strip's volume, through pipemeeter's socket
use crate::Output;
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Read, Write},
    os::unix::net::UnixStream,
    sync::mpsc::Receiver,
    time::Duration,
};
use wayvr_ipc::frame::HostMessage;

pub fn run(output: &Output, requests: &Receiver<u32>) {
    let mut sent = None;
    loop {
        if let Some(percent) = requests.try_iter().last() {
            let _ = request(
                &json!({"cmd": "set_volume", "strip": "Music", "volume": f64::from(percent) / 100.0}),
            );
        }
        let current = request(&json!({"cmd": "get_volume", "strip": "Music"}))
            .and_then(|reply| reply.get("volume").and_then(Value::as_f64))
            .filter(|value| value.is_finite())
            .map(|value| (value * 100.0).round().clamp(0.0, 100.0) as u32);
        if sent != Some(current) {
            output.send(&HostMessage::MusicVolume(current));
            sent = Some(current);
        }
        if !output.sleep(Duration::from_millis(200)) {
            return;
        }
    }
}

fn request(request: &Value) -> Option<Value> {
    let path = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)?
        .join("pipemeeter.sock");
    let mut stream = UnixStream::connect(path).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .ok()?;
    stream
        .set_write_timeout(Some(Duration::from_millis(200)))
        .ok()?;
    writeln!(stream, "{request}").ok()?;
    let mut response = String::new();
    io::BufReader::new(stream)
        .take(16384)
        .read_line(&mut response)
        .ok()?;
    let response: Value = serde_json::from_str(&response).ok()?;
    (response.get("ok")?.as_bool()? == true).then_some(response)
}
