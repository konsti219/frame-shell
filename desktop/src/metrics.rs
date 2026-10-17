use crate::Output;
use crate::metrics_proto as proto;
use anyhow::{Context, Result, bail};
use libloading::Library;
use prost::Message;
use std::{
    ffi::c_void,
    io::{ErrorKind, Read},
    os::{
        fd::{AsFd, AsRawFd},
        unix::net::UnixStream,
    },
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use wayvr_ipc::frame::{HostMessage, SessionFrame};

struct Runtime {
    _library: Library,
    root: *mut c_void,
    destroy: unsafe extern "C" fn(*mut *mut c_void),
}

impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe { (self.destroy)(&mut self.root) };
    }
}

impl Runtime {
    fn connect() -> Result<(Self, UnixStream)> {
        let manifest = runtime_manifest()?;
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest)?)?;
        let library = value["runtime"]["MND_libmonado_path"]
            .as_str()
            .context("Runtime has no libmonado")?;
        let library = manifest
            .canonicalize()?
            .parent()
            .context("Invalid runtime path")?
            .join(library);
        // These signatures are the public libmonado API used by WayVR's metrics reader.
        let library = unsafe { Library::new(library) }?;
        let create = unsafe {
            *library.get::<unsafe extern "C" fn(*mut *mut c_void) -> i32>(b"mnd_root_create\0")?
        };
        let destroy = unsafe {
            *library.get::<unsafe extern "C" fn(*mut *mut c_void)>(b"mnd_root_destroy\0")?
        };
        let push = unsafe {
            *library.get::<unsafe extern "C" fn(*mut c_void, i32, bool) -> i32>(
                b"mnd_root_push_metrics_fd\0",
            )?
        };
        let mut root = std::ptr::null_mut();
        let result = unsafe { create(&mut root) };
        if result != 0 {
            bail!("Connecting to host runtime: {result}");
        }
        let runtime = Self {
            _library: library,
            root,
            destroy,
        };
        let (reader, writer) = UnixStream::pair()?;
        writer.set_nonblocking(true)?;
        reader.set_nonblocking(true)?;
        let result = unsafe { push(root, writer.as_fd().as_raw_fd(), true) };
        if result != 0 {
            bail!("Subscribing to runtime metrics: {result}");
        }
        Ok((runtime, reader))
    }
}

fn runtime_manifest() -> Result<PathBuf> {
    for name in ["FRAME_SHELL_RUNTIME_JSON", "XR_RUNTIME_JSON"] {
        if let Some(path) = std::env::var_os(name) {
            return Ok(path.into());
        }
    }
    let home = PathBuf::from(std::env::var_os("HOME").context("No HOME")?);
    Ok(home.join(".config/openxr/1/active_runtime.json"))
}

pub fn run(output: &Output) {
    while !output.stopped() {
        let result = Runtime::connect().and_then(|(_runtime, stream)| relay(stream, output));
        if let Err(error) = result {
            eprintln!("Host runtime metrics: {error:#}");
        }
        output.sleep(Duration::from_secs(2));
    }
}

fn relay(mut stream: UnixStream, output: &Output) -> Result<()> {
    eprintln!("Host runtime metrics connected");
    let mut buffer = Vec::new();
    let mut frames = Vec::new();
    let mut next = Instant::now();
    while !output.stopped() {
        let mut chunk = [0; 8192];
        for _ in 0..32 {
            match stream.read(&mut chunk) {
                Ok(0) => bail!("Runtime closed metrics stream"),
                Ok(count) => buffer.extend_from_slice(&chunk[..count]),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        loop {
            let Ok(length) = prost::decode_length_delimiter(&buffer[..]) else {
                if buffer.len() >= 10 {
                    bail!("Invalid metrics length");
                }
                break;
            };
            if length > 65536 {
                bail!("Metrics record too large");
            }
            let header = prost::length_delimiter_len(length);
            if buffer.len() < header + length {
                break;
            }
            let record = proto::Record::decode(&buffer[header..header + length])?;
            if let Some(frame) = record.session_frame {
                frames.push(SessionFrame::from(frame));
            }
            buffer.drain(..header + length);
        }
        if Instant::now() >= next {
            for batch in frames.chunks(12) {
                output.send(&HostMessage::Frames(batch.to_vec()));
            }
            frames.clear();
            next = Instant::now() + Duration::from_millis(100);
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
