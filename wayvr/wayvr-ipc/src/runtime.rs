//! The frame shell's OpenXR runtime (Monado) through libmonado, loaded from the active runtime's manifest: its
//! clients with the Steam game each one is, focusing one, and the metrics stream.

use crate::frame::{ClientAction, RuntimeClient};
use anyhow::{Context, Result, bail};
use libloading::Library;
use std::{
	ffi::{CStr, c_char, c_void},
	os::fd::RawFd,
	path::PathBuf,
};

const CLIENT_PRIMARY_APP: u32 = 1 << 0;
const CLIENT_SESSION_ACTIVE: u32 = 1 << 1;
const CLIENT_SESSION_OVERLAY: u32 = 1 << 4;
const CLIENT_SESSION_VISIBLE: u32 = 1 << 2;
const CLIENT_PRESENTATION_DEFAULT: u32 = 1 << 10;
const CLIENT_PRESENTATION_OVERLAY: u32 = 1 << 11;

pub struct Runtime {
	library: Library,
	root: *mut c_void,
}

// libmonado's root is used by one thread at a time
unsafe impl Send for Runtime {}

impl Drop for Runtime {
	fn drop(&mut self) {
		if let Ok(destroy) =
			self.function::<unsafe extern "C" fn(*mut *mut c_void)>(b"mnd_root_destroy\0")
		{
			unsafe { destroy(&mut self.root) };
		}
	}
}

/// The runtime manifest the shell's PC server or the headset uses
fn runtime_manifest() -> Result<PathBuf> {
	for name in ["FRAME_SHELL_RUNTIME_JSON", "XR_RUNTIME_JSON"] {
		if let Some(path) = std::env::var_os(name) {
			return Ok(path.into());
		}
	}
	let config = std::env::var_os("XDG_CONFIG_HOME")
		.map(PathBuf::from)
		.or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
		.context("No HOME")?;
	Ok(config.join("openxr/1/active_runtime.json"))
}

fn libmonado_path() -> Result<PathBuf> {
	if let Some(path) = std::env::var_os("LIBMONADO_PATH") {
		return Ok(path.into());
	}
	let manifest = runtime_manifest()?;
	let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest)?)?;
	let library = value["runtime"]["MND_libmonado_path"]
		.as_str()
		.context("Runtime has no libmonado")?;
	// A bare file name goes through the library search path, anything else is relative to the manifest
	if !library.contains('/') {
		return Ok(library.into());
	}
	Ok(
		manifest
			.canonicalize()?
			.parent()
			.context("Invalid runtime path")?
			.join(library),
	)
}

/// The Steam game a process belongs to: Steam starts games with SteamAppId in their environment
fn steam_app_id(pid: i32) -> Option<String> {
	let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
	environ
		.split(|byte| *byte == 0)
		.find_map(|entry| entry.strip_prefix(b"SteamAppId="))
		.map(|id| String::from_utf8_lossy(id).into_owned())
		.filter(|id| !id.is_empty() && id != "0")
}

impl Runtime {
	pub fn connect() -> Result<Self> {
		let library = unsafe { Library::new(libmonado_path()?) }?;
		let create = unsafe {
			*library.get::<unsafe extern "C" fn(*mut *mut c_void) -> i32>(b"mnd_root_create\0")?
		};
		let mut root = std::ptr::null_mut();
		let result = unsafe { create(&mut root) };
		if result != 0 {
			bail!("Connecting to the runtime: {result}");
		}
		Ok(Self { library, root })
	}

	fn function<T: Copy>(&self, name: &[u8]) -> Result<T> {
		Ok(unsafe { *self.library.get::<T>(name)? })
	}

	fn check(result: i32, what: &str) -> Result<()> {
		if result != 0 {
			bail!("{what}: {result}");
		}
		Ok(())
	}

	/// Streams the runtime's frame metrics (length-delimited protobuf records) into `fd`
	pub fn push_metrics_fd(&self, fd: RawFd) -> Result<()> {
		let push = self.function::<unsafe extern "C" fn(*mut c_void, i32, bool) -> i32>(
			b"mnd_root_push_metrics_fd\0",
		)?;
		Self::check(
			unsafe { push(self.root, fd, true) },
			"Subscribing to runtime metrics",
		)
	}

	/// The runtime's clients with an active session
	pub fn clients(&self) -> Result<Vec<RuntimeClient>> {
		let update = self
			.function::<unsafe extern "C" fn(*mut c_void) -> i32>(b"mnd_root_update_client_list\0")?;
		let count = self.function::<unsafe extern "C" fn(*mut c_void, *mut u32) -> i32>(
			b"mnd_root_get_number_clients\0",
		)?;
		let id_at = self.function::<unsafe extern "C" fn(*mut c_void, u32, *mut u32) -> i32>(
			b"mnd_root_get_client_id_at_index\0",
		)?;
		let name = self.function::<unsafe extern "C" fn(*mut c_void, u32, *mut *const c_char) -> i32>(
			b"mnd_root_get_client_name\0",
		)?;
		let state = self.function::<unsafe extern "C" fn(*mut c_void, u32, *mut u32) -> i32>(
			b"mnd_root_get_client_state\0",
		)?;
		// API 1.9 (frame-monado); without it no client maps to a Steam game
		let pid = self
			.function::<unsafe extern "C" fn(*mut c_void, u32, *mut i32) -> i32>(
				b"mnd_root_get_client_pid\0",
			)
			.ok();

		Self::check(unsafe { update(self.root) }, "Listing runtime clients")?;
		let mut number = 0;
		Self::check(
			unsafe { count(self.root, &mut number) },
			"Counting runtime clients",
		)?;
		let mut clients = Vec::new();
		for index in 0..number {
			let mut id = 0;
			let mut flags = 0;
			let mut text: *const c_char = std::ptr::null();
			if unsafe { id_at(self.root, index, &mut id) } != 0
				|| unsafe { state(self.root, id, &mut flags) } != 0
				|| unsafe { name(self.root, id, &mut text) } != 0
				|| text.is_null()
				|| flags & CLIENT_SESSION_ACTIVE == 0
			{
				continue;
			}
			let mut process = 0;
			if let Some(pid) = pid {
				unsafe { pid(self.root, id, &mut process) };
			}
			clients.push(RuntimeClient {
				id,
				name: unsafe { CStr::from_ptr(text) }
					.to_string_lossy()
					.into_owned(),
				pid: process,
				steam_app_id: (process > 0).then(|| steam_app_id(process)).flatten(),
				primary: flags & CLIENT_PRIMARY_APP != 0,
				overlay: flags & CLIENT_SESSION_OVERLAY != 0,
				visible: flags & CLIENT_SESSION_VISIBLE != 0,
				presentation_mode: if flags & CLIENT_PRESENTATION_DEFAULT != 0 {
					1
				} else if flags & CLIENT_PRESENTATION_OVERLAY != 0 {
					2
				} else {
					0
				},
			});
		}
		Ok(clients)
	}

	pub fn set_input_capture(
		&self,
		profiles: &[std::ffi::CString; 2],
		paths: &[std::ffi::CString],
		pose_mask: u32,
	) -> Result<()> {
		let set = self.function::<unsafe extern "C" fn(
			*mut c_void,
			*const c_char,
			*const c_char,
			*const *const c_char,
			u32,
			u32,
		) -> i32>(b"mnd_root_set_input_capture\0")?;
		let pointers: Vec<_> = paths.iter().map(|p| p.as_ptr()).collect();
		Self::check(
			unsafe {
				set(
					self.root,
					profiles[0].as_ptr(),
					profiles[1].as_ptr(),
					pointers.as_ptr(),
					pointers.len().try_into()?,
					pose_mask,
				)
			},
			"Setting input capture",
		)
	}

	pub fn background_visible(&self, visible: bool) -> Result<()> {
		let set = self.function::<unsafe extern "C" fn(*mut c_void, bool) -> i32>(
			b"mnd_root_set_background_visible\0",
		)?;
		Self::check(
			unsafe { set(self.root, visible) },
			"Setting background visibility",
		)
	}

	pub fn control(&self, client: u32, action: ClientAction) -> Result<()> {
		let state = self
			.clients()?
			.into_iter()
			.find(|state| state.id == client)
			.context("Runtime client no longer running")?;
		if matches!(action, ClientAction::Select) && !state.overlay && !(state.primary && state.visible)
		{
			return self.set_primary(client);
		}
		let (mode, visible) = match action {
			ClientAction::Select => (state.presentation_mode, !state.visible),
			ClientAction::Show => (state.presentation_mode, true),
			ClientAction::Automatic => (0, true),
			ClientAction::Default => (1, true),
			ClientAction::Overlay => (2, true),
		};
		let set = self.function::<unsafe extern "C" fn(*mut c_void, u32, u32, bool) -> i32>(
			b"mnd_root_set_client_presentation\0",
		)?;
		Self::check(
			unsafe { set(self.root, client, mode, visible) },
			"Changing app presentation",
		)?;
		if matches!(action, ClientAction::Default)
			|| (matches!(action, ClientAction::Show) && !state.overlay)
		{
			self.set_primary(client)?;
		}
		Ok(())
	}

	/// Makes a client the one shown
	pub fn set_primary(&self, client: u32) -> Result<()> {
		let primary = self.function::<unsafe extern "C" fn(*mut c_void, u32) -> i32>(
			b"mnd_root_set_client_primary\0",
		)?;
		Self::check(
			unsafe { primary(self.root, client) },
			"Focusing a runtime client",
		)
	}
}
