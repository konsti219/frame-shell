use std::{
    ffi::OsStr, os::unix::process::CommandExt, path::PathBuf, process::Command,
    sync::atomic::Ordering,
};

use anyhow::Context;
use wlx_common::desktop_finder::{DesktopEntry, DesktopFinder};

const EXEC: &str = "/usr/bin/steamos-nested-desktop";
const APP_ID: &str = "deckard-nested-desktop";

pub fn resolve_executable(exec: &str) -> &str {
    if crate::frame_shell::EMBEDDED.load(Ordering::Relaxed)
        && std::path::Path::new(exec).file_name() == Some(OsStr::new("frame-shell-desktop"))
    {
        EXEC
    } else {
        exec
    }
}

pub fn register(finder: &mut DesktopFinder) {
    if !crate::frame_shell::EMBEDDED.load(Ordering::Relaxed) || !std::path::Path::new(EXEC).exists()
    {
        return;
    }
    let mut entry = finder
        .find_entries()
        .remove(APP_ID)
        .unwrap_or_else(|| DesktopEntry {
            app_id: APP_ID.into(),
            app_name: "Desktop".into(),
            exec_path: EXEC.into(),
            exec_args: "".into(),
            icon_path: None,
            categories: vec!["Utility".into()],
        });
    entry.exec_path = EXEC.into();
    entry.exec_args = "".into();
    finder.add_builtin(entry);
}

#[derive(Debug)]
pub struct PlasmaLaunch {
    config_dir: PathBuf,
}

impl PlasmaLaunch {
    pub fn prepare(cmd: &mut Command) -> anyhow::Result<Option<Self>> {
        if !crate::frame_shell::EMBEDDED.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let executable = std::path::Path::new(cmd.get_program()).file_name();
        if executable != Some(OsStr::new("steamos-nested-desktop")) {
            return Ok(None);
        }

        let runtime = PathBuf::from(
            std::env::var_os("XDG_RUNTIME_DIR").context("Plasma needs XDG_RUNTIME_DIR")?,
        );
        let launch = Self {
            config_dir: runtime.join(format!("frame-shell-plasma-{}", uuid::Uuid::new_v4())),
        };
        std::fs::create_dir_all(launch.config_dir.join("autostart"))?;
        std::fs::write(
            launch.config_dir.join("autostart/steam.desktop"),
            "[Desktop Entry]\nHidden=true\n",
        )?;

        let display = cmd
            .get_envs()
            .find_map(|(key, value)| (key == "WAYLAND_DISPLAY").then_some(value).flatten())
            .context("Plasma needs WayVR's Wayland socket")?;
        let socket = runtime.join(display);
        let mut config_dirs = launch.config_dir.clone().into_os_string();
        config_dirs.push(":");
        config_dirs.push(std::env::var_os("XDG_CONFIG_DIRS").unwrap_or_else(|| "/etc/xdg".into()));
        cmd.env("WAYLAND_DISPLAY", socket);
        cmd.env("XDG_CONFIG_DIRS", config_dirs);
        // Nested Plasma chooses its own application backends
        for key in [
            "SDL_VIDEODRIVER",
            "GDK_BACKEND",
            "QT_QPA_PLATFORM",
            "CLUTTER_BACKEND",
            "MOZ_ENABLE_WAYLAND",
            "ELECTRON_OZONE_PLATFORM_HINT",
            "LD_LIBRARY_PATH",
        ] {
            cmd.env_remove(key);
        }
        cmd.env("XDG_SESSION_DESKTOP", "KDE");
        cmd.process_group(0);
        link_gamescope_socket(runtime.join("nested_plasma"), "wayland-0".into());
        Ok(Some(launch))
    }
}

/// Lepton (Android games) takes Gamescope's socket in the session's runtime folder: a link to `socket` there, once
/// it exists, unless Gamescope runs
pub(crate) fn link_gamescope_socket(dir: PathBuf, socket: String) {
    std::thread::spawn(move || {
        for _ in 0..120 {
            if dir.join("gamescope-0").symlink_metadata().is_ok() {
                return;
            }
            if dir.join(&socket).exists() {
                if let Err(error) = std::os::unix::fs::symlink(&socket, dir.join("gamescope-0")) {
                    log::warn!("Could not link Gamescope's socket for Lepton: {error}");
                }
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
}

impl Drop for PlasmaLaunch {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.config_dir) {
            log::warn!("Could not remove Plasma runtime configuration: {error}");
        }
    }
}
