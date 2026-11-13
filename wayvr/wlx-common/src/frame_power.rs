//! The Steam Frame's battery, as SteamVR reads it (Valve's charger service in /run/deckardcharger), its power draw
//! from the board's power monitors, and the frame shell's performance modes: steamos-manager's performance profiles,
//! which the shell keeps applied (frame-shell.service goes back to Performance when the shell stops).

use std::path::PathBuf;

const CHARGER: &str = "/run/deckardcharger";

pub struct Battery {
	pub percent: f32,
	/// On the charger (not necessarily charging, it may be full)
	pub external_power: bool,
	pub full: bool,
	/// Into the battery in watts, negative while it drains
	pub charge_watts: Option<f32>,
	/// Until empty (on battery) or full (charging), from the charger service
	pub seconds_left: Option<u32>,
}

fn read(name: &str) -> Option<String> {
	std::fs::read_to_string(format!("{CHARGER}/{name}"))
		.ok()
		.map(|text| text.trim().to_owned())
}

fn read_number(name: &str) -> Option<f64> {
	read(name)?.parse().ok()
}

pub fn battery() -> Option<Battery> {
	let percent = read_number("battery_percent")? as f32;
	// "Connected" or "Connected slow", as Monado reads it for the watch
	let external_power = read("ac_status").is_some_and(|status| status.starts_with("Connected"));
	let full = read("battery_status").is_some_and(|status| status == "Full");
	// The fuel gauge (BAT0 links to it): microvolts times microamperes, negative discharging
	let charge_watts = read_number("BAT0/voltage_now")
		.zip(read_number("BAT0/current_avg"))
		.map(|(volts, amps)| (volts * amps) as f32 * 1e-12);
	let seconds_left = if full {
		None
	} else if external_power {
		read_number("secs_until_battery_full")
	} else {
		read_number("secs_until_shutdown_request")
	}
	.filter(|seconds| *seconds > 0.0 && *seconds < 48.0 * 3600.0)
	.map(|seconds| seconds as u32);
	Some(Battery {
		percent,
		external_power,
		full,
		charge_watts,
		seconds_left,
	})
}

/// The percentage as the watch and the battery menu show it: rounded, as truncating shows 0.29 as 28
pub fn shown_percent(fraction: f32) -> u32 {
	(fraction * 100.).round().min(99.) as u32
}

/// Power draw in watts, from the MAX34417 power monitors (refreshed about every 2 s)
pub struct Power {
	/// The system rail (VPH), everything the battery or charger feeds
	pub system: f32,
	pub cpu: f32,
	pub gpu: f32,
}

pub fn power() -> Option<Power> {
	let mut rails = std::collections::HashMap::<String, f32>::new();
	for entry in std::fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
		let dir = entry.path();
		if !std::fs::read_to_string(dir.join("name")).is_ok_and(|name| name.starts_with("max34417")) {
			continue;
		}
		for channel in 1..=4 {
			let label = std::fs::read_to_string(dir.join(format!("power{channel}_label")));
			let microwatts = std::fs::read_to_string(dir.join(format!("power{channel}_input")));
			if let (Ok(label), Some(microwatts)) = (label, microwatts.ok().and_then(|text| text.trim().parse::<f32>().ok())) {
				rails.insert(label.trim().to_owned(), microwatts * 1e-6);
			}
		}
	}
	let rail = |name: &str| rails.get(name).copied().unwrap_or_default();
	Some(Power {
		system: *rails.get("vph")?,
		cpu: rail("apc0") + rail("apc1") + rail("apc2"),
		gpu: rail("gfx"),
	})
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PerformanceMode {
	/// PC streaming: steamos-manager's Powersave profile (GPU capped at 629 MHz, CPUs at 2.04 GHz)
	StreamingOptimized,
	/// Native games: the Performance profile (uncapped GPU)
	Standard,
}

impl PerformanceMode {
	const fn profile(self) -> &'static str {
		match self {
			Self::StreamingOptimized => "Powersave",
			Self::Standard => "Performance",
		}
	}

	/// The GPU clock limit the profile sets, to notice when something else changed it
	const fn gpu_max_mhz(self) -> u32 {
		match self {
			Self::StreamingOptimized => 629,
			Self::Standard => 903,
		}
	}
}

fn home() -> PathBuf {
	PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn mode_path() -> PathBuf {
	home().join(".config/frame-shell/performance-mode")
}

/// "streaming" or "standard" in the mode file, streaming without one
pub fn performance_mode() -> PerformanceMode {
	match std::fs::read_to_string(mode_path()).as_deref().map(str::trim) {
		Ok("standard") => PerformanceMode::Standard,
		_ => PerformanceMode::StreamingOptimized,
	}
}

const GPU: &str = "/sys/class/devfreq/3d00000.gpu";

/// The GPU's clock limit (Hz) for a cap: the highest available one within it
fn gpu_limit(mhz: u32) -> Option<u64> {
	let cap = u64::from(mhz) * 1_000_000;
	std::fs::read_to_string(format!("{GPU}/available_frequencies"))
		.ok()?
		.split_whitespace()
		.filter_map(|hz| hz.parse::<u64>().ok())
		.filter(|hz| *hz <= cap)
		.max()
}

fn set_profile(profile: &str) -> anyhow::Result<()> {
	let status = std::process::Command::new("steamosctl")
		.args(["set-performance-profile", profile])
		.stdout(std::process::Stdio::null())
		.status()?;
	anyhow::ensure!(status.success(), "steamosctl set-performance-profile {profile} failed: {status}");
	Ok(())
}

/// The GPU limit a profile left in place for a mode: when it differs from `gpu_max_mhz`, it's not applied again
static SETTLED: std::sync::Mutex<Option<(PerformanceMode, u64)>> = std::sync::Mutex::new(None);

fn gpu_max_freq() -> anyhow::Result<u64> {
	Ok(std::fs::read_to_string(format!("{GPU}/max_freq"))?.trim().parse::<u64>()?)
}

/// Applies the mode's profile when the GPU's limit differs: at the shell's start, and when something else changed it
/// (Steam sets its GPU performance level to auto, which lifts the cap). steamosctl runs on its own thread, one at a
/// time, since the caller is the render loop.
pub fn enforce_performance_mode() -> anyhow::Result<()> {
	static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
	let mode = performance_mode();
	let Some(limit) = gpu_limit(mode.gpu_max_mhz()) else {
		return Ok(());
	};
	let current = gpu_max_freq()?;
	let settled = *SETTLED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
	if current == limit || settled == Some((mode, current)) || RUNNING.swap(true, std::sync::atomic::Ordering::AcqRel) {
		return Ok(());
	}
	log::info!("GPU limit {current} Hz instead of {limit} Hz: applying the {} profile", mode.profile());
	std::thread::spawn(move || {
		match set_profile(mode.profile()).and_then(|()| gpu_max_freq()) {
			Ok(after) => {
				if after != limit {
					log::warn!("The {} profile left the GPU limit at {after} Hz, not {limit} Hz", mode.profile());
				}
				*SETTLED.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((mode, after));
			}
			Err(error) => log::warn!("Performance mode: {error:?}"),
		}
		RUNNING.store(false, std::sync::atomic::Ordering::Release);
	});
	Ok(())
}

/// Stores the mode (so the next session starts with it) and applies it now
pub fn set_performance_mode(mode: PerformanceMode) -> anyhow::Result<()> {
	let path = mode_path();
	std::fs::create_dir_all(path.parent().unwrap())?;
	let name = match mode {
		PerformanceMode::StreamingOptimized => "streaming",
		PerformanceMode::Standard => "standard",
	};
	std::fs::write(path, name)?;
	enforce_performance_mode()
}
