use crate::Output;
use std::{process::Command, sync::mpsc::Receiver, time::Duration};
use sysinfo::{Components, System};
use wayvr_ipc::frame::{HostMessage, SystemStats};

pub fn run(output: &Output, power: &Receiver<u8>) {
    let mut system = System::new_all();
    let mut components = Components::new_with_refreshed_list();
    let device = find_amdgpu_device();
    let hwmon = find_amdgpu_hwmon();
    loop {
        if let Some(profile) = power.try_iter().last() {
            if let Some((_, maximum)) = lact_get_power_range() {
                let watts = [230, 304, maximum][usize::from(profile)].min(maximum);
                if !lact_set_power_limit(watts) {
                    output.send(&HostMessage::Error(
                        "PC GPU power limit could not be changed".into(),
                    ));
                }
            } else {
                output.send(&HostMessage::Error(
                    "PC GPU power controls are unavailable".into(),
                ));
            }
        }
        system.refresh_cpu_usage();
        system.refresh_memory();
        components.refresh(false);
        let (draw, cap) = hwmon
            .as_deref()
            .and_then(query_gpu_power_sysfs)
            .unwrap_or_default();
        let range = lact_get_power_range();
        let vram = query_vram_sysfs(device.as_deref());
        output.send(&HostMessage::System(SystemStats {
            cpu_usage: system.global_cpu_usage(),
            cpu_temp: find_cpu_temp(&components),
            gpu_usage: query_gpu_busy_sysfs(device.as_deref()),
            gpu_temp: find_gpu_temp(&components),
            ram_used_gib: kib_to_gib(system.used_memory()),
            ram_total_gib: kib_to_gib(system.total_memory()),
            vram_used_gib: vram.and_then(|(used, _)| used),
            vram_total_gib: vram.map(|(_, total)| total),
            core_usage: cpu_usage_buckets(system.cpus(), 32),
            power_draw_watts: (draw != 0).then_some(draw),
            power_cap_watts: (cap != 0).then_some(cap),
            power_max_watts: range.map(|(_, maximum)| maximum),
            power_profile: range.map(|(current, maximum)| {
                if current <= 230 {
                    0
                } else if current >= maximum {
                    2
                } else {
                    1
                }
            }),
        }));
        if !output.sleep(Duration::from_secs(1)) {
            return;
        }
    }
}

fn kib_to_gib(value: u64) -> f32 {
    value as f32 / 1024.0 / 1024.0 / 1024.0
}

fn cpu_usage_buckets(cpus: &[sysinfo::Cpu], buckets: usize) -> Vec<f32> {
    if buckets == 0 {
        return Vec::new();
    }

    if cpus.is_empty() {
        return vec![0.0; buckets];
    }

    let mut out = vec![0.0; buckets];
    let mut counts = vec![0usize; buckets];

    for (idx, cpu) in cpus.iter().enumerate() {
        let bucket = idx * buckets / cpus.len();
        out[bucket] += cpu.cpu_usage();
        counts[bucket] += 1;
    }

    for (value, count) in out.iter_mut().zip(counts) {
        if count > 0 {
            *value /= count as f32;
        }
    }

    out
}

fn find_cpu_temp(components: &Components) -> Option<f32> {
    components.iter().find_map(|component| {
        let label = component.label().to_ascii_lowercase();
        if label.contains("package")
            || label.contains("tctl")
            || label.contains("tdie")
            || label.contains("cpu")
        {
            component.temperature().filter(|temp| temp.is_finite())
        } else {
            None
        }
    })
}

fn find_gpu_temp(components: &Components) -> Option<f32> {
    components.iter().find_map(|component| {
        let label = component.label().to_ascii_lowercase();
        if label.contains("gpu") || label.contains("edge") || label.contains("junction") {
            component.temperature().filter(|temp| temp.is_finite())
        } else {
            None
        }
    })
}

fn find_amdgpu_hwmon() -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
        let path = entry.path();
        let name = std::fs::read_to_string(path.join("name")).unwrap_or_default();
        if name.trim() == "amdgpu" && path.join("power1_cap").exists() {
            return Some(path);
        }
    }
    None
}

fn find_amdgpu_device() -> Option<std::path::PathBuf> {
    // Canonical hwmon path is …/device/hwmon/hwmonN; two levels up is the PCI device.
    let hwmon = find_amdgpu_hwmon()?;
    let canonical = hwmon.canonicalize().ok()?;
    let device = canonical.parent()?.parent()?;
    device
        .join("mem_info_vram_total")
        .exists()
        .then(|| device.to_path_buf())
}

fn query_gpu_power_sysfs(hwmon: &std::path::Path) -> Option<(u32, u32)> {
    let draw_uw: u64 = std::fs::read_to_string(hwmon.join("power1_average"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let cap_uw: u64 = std::fs::read_to_string(hwmon.join("power1_cap"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some((draw_uw as u32 / 1_000_000, cap_uw as u32 / 1_000_000))
}

fn query_gpu_busy_sysfs(device: Option<&std::path::Path>) -> Option<u32> {
    let busy: u32 = std::fs::read_to_string(device?.join("gpu_busy_percent"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(busy.min(100))
}

fn query_vram_sysfs(device: Option<&std::path::Path>) -> Option<(Option<f32>, f32)> {
    let device = device?;
    let total_bytes: u64 = std::fs::read_to_string(device.join("mem_info_vram_total"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let used_bytes: u64 = std::fs::read_to_string(device.join("mem_info_vram_used"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let gib = 1024.0 * 1024.0 * 1024.0;
    Some((Some(used_bytes as f32 / gib), total_bytes as f32 / gib))
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn lact_get_power_range() -> Option<(u32, u32)> {
    // "Current power limit: 304W (Configurable Range: 212W to 374W)"
    let output = command_output("lact", &["cli", "power-limit", "get"])?;
    let current: u32 = output
        .split("Current power limit: ")
        .nth(1)?
        .split('W')
        .next()?
        .trim()
        .parse()
        .ok()?;
    let max: u32 = output
        .split(" to ")
        .nth(1)?
        .split('W')
        .next()?
        .trim()
        .parse()
        .ok()?;
    Some((current, max))
}

fn lact_set_power_limit(watts: u32) -> bool {
    Command::new("lact")
        .args(["cli", "power-limit", "set", &watts.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
