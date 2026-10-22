use crate::Output;
use crate::metrics_proto as proto;
use anyhow::{Result, bail};
use prost::Message;
use std::{
    io::{ErrorKind, Read},
    os::{
        fd::{AsFd, AsRawFd},
        unix::net::UnixStream,
    },
    thread,
    time::{Duration, Instant},
};
use wayvr_ipc::{
    frame::{HostMessage, SessionFrame},
    runtime::Runtime,
};

/// The runtime's metrics stream, through libmonado
fn connect() -> Result<(Runtime, UnixStream)> {
    let runtime = Runtime::connect()?;
    let (reader, writer) = UnixStream::pair()?;
    writer.set_nonblocking(true)?;
    reader.set_nonblocking(true)?;
    runtime.push_metrics_fd(writer.as_fd().as_raw_fd())?;
    Ok((runtime, reader))
}

pub fn run(output: &Output) {
    while !output.stopped() {
        let result = connect().and_then(|(_runtime, stream)| relay(stream, output));
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
