//! Debugging aid: a local socket ($XDG_RUNTIME_DIR/frame-metrics.sock) streaming records as JSON lines to whoever is
//! connected. Nothing is kept or computed here: without a reader a record is dropped right away.

use serde::Serialize;
use std::{
	os::{
		fd::AsRawFd,
		unix::net::{UnixListener, UnixStream},
	},
	path::PathBuf,
	sync::{
		Mutex, MutexGuard, Once, PoisonError,
		atomic::{AtomicBool, Ordering},
		mpsc::{self, SyncSender, TrySendError},
	},
	time::{SystemTime, UNIX_EPOCH},
};

/// Lines waiting for a slow reader; past this it misses records and is told how many
const BACKLOG: usize = 4096;

static LISTENING: AtomicBool = AtomicBool::new(false);
static READERS: Mutex<Vec<Reader>> = Mutex::new(Vec::new());

struct Reader {
	lines: SyncSender<String>,
	dropped: u64,
}

#[derive(Serialize)]
struct Record<'a> {
	time: f64,
	stream: &'a str,
	stage: &'a str,
	#[serde(skip_serializing_if = "Option::is_none")]
	seq: Option<u64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	bytes: Option<u64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	detail: Option<&'a str>,
}

fn readers() -> MutexGuard<'static, Vec<Reader>> {
	READERS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn socket_path() -> PathBuf {
	std::env::var_os("XDG_RUNTIME_DIR")
		.map_or_else(|| "/tmp".into(), PathBuf::from)
		.join("frame-metrics.sock")
}

/// Opens the socket, once per process
pub fn serve() {
	static START: Once = Once::new();
	START.call_once(|| {
		if let Err(error) = std::thread::Builder::new()
			.name("frame-metrics".into())
			.spawn(accept)
		{
			eprintln!("Metrics socket: {error}");
		}
	});
}

fn accept() {
	let path = socket_path();
	let _ = std::fs::remove_file(&path);
	let listener = match UnixListener::bind(&path) {
		Ok(listener) => listener,
		Err(error) => {
			eprintln!("Metrics socket {}: {error}", path.display());
			return;
		}
	};
	for socket in listener.incoming().flatten() {
		let (lines, queue) = mpsc::sync_channel::<String>(BACKLOG);
		std::thread::spawn(move || {
			for line in queue {
				if !write(&socket, line.as_bytes()) {
					break;
				}
			}
		});
		readers().push(Reader { lines, dropped: 0 });
		LISTENING.store(true, Ordering::Relaxed);
	}
}

/// MSG_NOSIGNAL: a reader going away must not raise SIGPIPE in the WiVRn process hosting this
fn write(socket: &UnixStream, mut data: &[u8]) -> bool {
	while !data.is_empty() {
		let sent = unsafe {
			libc::send(
				socket.as_raw_fd(),
				data.as_ptr().cast(),
				data.len(),
				libc::MSG_NOSIGNAL,
			)
		};
		match usize::try_from(sent) {
			Ok(sent) => data = &data[sent..],
			Err(_) if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => {}
			Err(_) => return false,
		}
	}
	true
}

fn publish(record: &Record) {
	let Ok(mut line) = serde_json::to_string(record) else {
		return;
	};
	line.push('\n');
	let mut readers = readers();
	readers.retain_mut(|reader| {
		if reader.dropped > 0 {
			let notice = format!("{{\"dropped\":{}}}\n", reader.dropped);
			if reader.lines.try_send(notice).is_ok() {
				reader.dropped = 0;
			}
		}
		match reader.lines.try_send(line.clone()) {
			Ok(()) => true,
			Err(TrySendError::Full(_)) => {
				reader.dropped += 1;
				true
			}
			Err(TrySendError::Disconnected(_)) => false,
		}
	});
	LISTENING.store(!readers.is_empty(), Ordering::Relaxed);
}

fn now() -> f64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_or(0.0, |time| time.as_secs_f64())
}

pub fn listening() -> bool {
	LISTENING.load(Ordering::Relaxed)
}

/// Something passed a stage of a stream: `seq` tells which frame, `bytes` how large it is
pub fn emit(stream: &str, stage: &str, seq: u64, bytes: u64) {
	if listening() {
		publish(&Record {
			time: now(),
			stream,
			stage,
			seq: Some(seq),
			bytes: Some(bytes),
			detail: None,
		});
	}
}

/// Something happened to a stream
pub fn event(stream: &str, detail: &str) {
	if listening() {
		publish(&Record {
			time: now(),
			stream,
			stage: "event",
			seq: None,
			bytes: None,
			detail: Some(detail),
		});
	}
}
