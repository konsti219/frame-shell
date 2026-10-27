//! The Steam Frame's battery, as SteamVR reads it (Valve's charger service in /run/deckardcharger), its power draw
//! from the board's power monitors, and the frame shell's performance modes: Monado's GPU clock cap (`frame.gpu_max_mhz` in its config), which its session-power
//! script applies.

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
	/// The frame shell's tuning for PC streaming: GPU capped at 578 MHz
	StreamingOptimized,
	/// Native games: GPU uncapped
	Standard,
}

impl PerformanceMode {
	const fn gpu_max_mhz(self) -> u32 {
		match self {
			Self::StreamingOptimized => 578,
			Self::Standard => 903,
		}
	}
}

fn home() -> PathBuf {
	PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn monado_config_path() -> PathBuf {
	home().join(".config/monado/config_v0.json")
}

fn read_monado_config() -> serde_json::Value {
	std::fs::read(monado_config_path())
		.ok()
		.and_then(|bytes| serde_json::from_slice(&bytes).ok())
		.unwrap_or_else(|| serde_json::json!({}))
}

/// The GPU cap in Monado's config, as session-power reads it
fn configured_gpu_mhz() -> u64 {
	read_monado_config()["frame"]["gpu_max_mhz"].as_u64().unwrap_or(578)
}

pub fn performance_mode() -> PerformanceMode {
	if configured_gpu_mhz() > u64::from(PerformanceMode::StreamingOptimized.gpu_max_mhz()) {
		PerformanceMode::Standard
	} else {
		PerformanceMode::StreamingOptimized
	}
}

const GPU: &str = "/sys/class/devfreq/3d00000.gpu";

/// The GPU's clock limit (Hz) for a cap: the highest available one within it
fn gpu_limit(mhz: u64) -> Option<u64> {
	let cap = mhz * 1_000_000;
	std::fs::read_to_string(format!("{GPU}/available_frequencies"))
		.ok()?
		.split_whitespace()
		.filter_map(|hz| hz.parse::<u64>().ok())
		.filter(|hz| *hz <= cap)
		.max()
}

fn apply_performance_mode() -> anyhow::Result<()> {
	let status = std::process::Command::new(home().join("sf/frame-monado/session-power"))
		.arg("start")
		.status()?;
	anyhow::ensure!(status.success(), "session-power failed: {status}");
	Ok(())
}

/// Applies the mode again when something else changed the GPU's limit (Steam sets its performance level to auto,
/// which lifts the cap)
pub fn enforce_performance_mode() -> anyhow::Result<()> {
	let Some(limit) = gpu_limit(configured_gpu_mhz()) else {
		return Ok(());
	};
	let current = std::fs::read_to_string(format!("{GPU}/max_freq"))?.trim().parse::<u64>()?;
	if current != limit {
		log::info!("GPU limit {current} Hz instead of {limit} Hz: applying the performance mode again");
		apply_performance_mode()?;
	}
	Ok(())
}

/// Stores the mode in Monado's config (so the next session starts with it) and applies it now
pub fn set_performance_mode(mode: PerformanceMode) -> anyhow::Result<()> {
	let mut config = read_monado_config();
	if !config.is_object() {
		config = serde_json::json!({});
	}
	if !config["frame"].is_object() {
		config["frame"] = serde_json::json!({});
	}
	config["frame"]["gpu_max_mhz"] = mode.gpu_max_mhz().into();
	std::fs::write(monado_config_path(), serde_json::to_vec_pretty(&config)?)?;
	apply_performance_mode()
}
