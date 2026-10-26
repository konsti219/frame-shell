//! The headset's buttons (power, volume), power actions and battery warnings

use super::*;

/// Last battery check, and the lowest warning level shown since the headset was last on the charger
pub(crate) static BATTERY_WARNING: Mutex<(Option<std::time::Instant>, Option<u32>)> = Mutex::new((None, None));

/// Every 5 s: keeps the performance mode applied, and warns once each at 10, 5 and 1 % on battery (only the lowest
/// level crossed since the last check)
pub fn check_battery(app: &mut AppState) {
    let mut state = BATTERY_WARNING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (checked, warned) = &mut *state;
    if checked.is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(5)) {
        return;
    }
    *checked = Some(std::time::Instant::now());
    if let Err(error) = wlx_common::frame_power::enforce_performance_mode() {
        log::warn!("Performance mode: {error:?}");
    }
    let Some(battery) = wlx_common::frame_power::battery() else {
        return;
    };
    if battery.external_power {
        *warned = None;
        return;
    }
    let Some(level) = [1, 5, 10].into_iter().find(|level| {
        battery.percent <= *level as f32 && warned.is_none_or(|warned| *level < warned)
    }) else {
        return;
    };
    *warned = Some(level);
    drop(state);
    crate::overlays::toast::Toast::new(
        wlx_common::overlays::ToastTopic::Priority,
        Some(wgui::i18n::Translation::from_raw_text("Battery low")),
        wgui::i18n::Translation::from_raw_text_string(format!("{level} % left")),
    )
    .with_timeout(6.0)
    .with_sound(true)
    .submit(app);
}

pub fn power_action(action: &str) {
    // frame-sleep stops the VR stack around the suspend (and so this process): run it as its own unit
    let (program, args): (&str, &[&str]) = match action {
        "sleep" => (
            "systemd-run",
            &[
                "--user",
                "--collect",
                "--unit=frame-sleep",
                "--",
                "/bin/sh",
                "-c",
                "exec \"$HOME/.local/bin/frame-sleep\"",
            ],
        ),
        "restart_stack" => (
            "systemctl",
            &[
                "--user",
                "--no-block",
                "restart",
                "monado.service",
                "frame-shell.service",
            ],
        ),
        "restart" => ("systemctl", &["reboot"]),
        "poweroff" => ("systemctl", &["poweroff"]),
        "steamvr" => ("steamosctl", &["switch-to-game-mode"]),
        _ => return,
    };
    if let Err(error) = std::process::Command::new(program).args(args).spawn() {
        log::error!("{program} {args:?}: {error}");
    }
}

/// The next thing the dashboard should show for the shell
pub fn take_shell_request() -> Option<wlx_common::dash_interface::ShellRequest> {
    use wlx_common::dash_interface::ShellRequest;
    if POWER_MENU_REQUEST.swap(false, Ordering::Relaxed) {
        Some(ShellRequest::PowerMenu)
    } else if PAIRING_REQUEST.swap(false, Ordering::Relaxed) {
        Some(ShellRequest::Pairing)
    } else {
        None
    }
}

/// Steps the headset output volume and queues the toast showing it
pub(crate) fn step_volume(up: bool) {
    crate::headset_audio::step_output(up);
}

pub(crate) fn volume_feedback(percent: u32) {
    VOLUME_TOAST.store(percent + 1, Ordering::Relaxed);
}

/// Reads the headset's buttons: power (`pmic_pwrkey`; short press sleeps, held 1 s opens the power menu like
/// SteamOS) and volume (`pmic_resin` down, `gpio-keys` up; repeats while held)
pub(crate) fn watch_keys() {
    let Ok(entries) = std::fs::read_dir("/sys/class/input") else {
        return;
    };
    for entry in entries.flatten() {
        let file = entry.file_name().to_string_lossy().into_owned();
        let Ok(name) = std::fs::read_to_string(entry.path().join("device/name")) else {
            continue;
        };
        if file.starts_with("event")
            && matches!(name.trim(), "pmic_pwrkey" | "pmic_resin" | "gpio-keys")
        {
            watch_key_device(format!("/dev/input/{file}"));
        }
    }
}

pub(crate) fn watch_key_device(path: String) {
    std::thread::spawn(move || {
        use std::io::Read;
        let mut device = match std::fs::File::open(&path) {
            Ok(device) => device,
            Err(error) => {
                log::warn!("Power key {path}: {error}");
                return;
            }
        };
        // struct input_event on 64-bit: timeval (16), type (2), code (2), value (4)
        let mut event = [0u8; 24];
        while device.read_exact(&mut event).is_ok() {
            let kind = u16::from_ne_bytes([event[16], event[17]]);
            let code = u16::from_ne_bytes([event[18], event[19]]);
            let value = i32::from_ne_bytes([event[20], event[21], event[22], event[23]]);
            const EV_KEY: u16 = 1;
            const KEY_VOLUMEDOWN: u16 = 114;
            const KEY_VOLUMEUP: u16 = 115;
            const KEY_POWER: u16 = 116;
            if kind != EV_KEY {
                continue;
            }
            if matches!(code, KEY_VOLUMEDOWN | KEY_VOLUMEUP) {
                // Press and auto-repeat
                if value != 0 {
                    step_volume(code == KEY_VOLUMEUP);
                }
                continue;
            }
            if code != KEY_POWER {
                continue;
            }
            if value == 1 {
                POWER_KEY_HELD.store(false, Ordering::Relaxed);
                POWER_KEY_DOWN_MS
                    .store(START.elapsed().as_millis().max(1) as u64, Ordering::Relaxed);
            } else if value == 0 {
                // A release without a press seen here (the shell started while the key was down) does nothing
                let pressed = POWER_KEY_DOWN_MS.swap(0, Ordering::Relaxed) != 0;
                if !POWER_KEY_HELD.swap(false, Ordering::Relaxed) && pressed {
                    power_action("sleep");
                }
            }
        }
    });
}
