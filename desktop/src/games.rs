use crate::Output;
use std::{sync::mpsc::Receiver, time::Duration};
use wayvr_ipc::frame::HostMessage;

pub enum Command {
    Stop(String, bool),
    Refresh,
}

/// Sends the running games when they change (or on `Refresh`)
pub fn run(output: &Output, commands: &Receiver<Command>) {
    let mut sent = None;
    loop {
        for command in commands.try_iter() {
            match command {
                Command::Stop(app_id, force) => {
                    if let Err(error) = stop(&app_id, force) {
                        output.send(&HostMessage::Error(format!(
                            "Stopping PC game failed: {error}"
                        )));
                    }
                }
                Command::Refresh => sent = None,
            }
        }
        match list_running_games() {
            Ok(games) => {
                let ids: std::collections::BTreeSet<_> =
                    games.into_iter().map(|game| game.app_id).collect();
                if sent.as_ref() != Some(&ids) {
                    output.send(&HostMessage::Games(ids.iter().cloned().collect()));
                    sent = Some(ids);
                }
            }
            Err(error) => eprintln!("Running PC games: {error}"),
        }
        if !output.sleep(Duration::from_secs(1)) {
            return;
        }
    }
}

pub struct RunningGame {
    pub app_id: String,
    pub pid: i32,
}

/// Flatpak puts this file in every sandbox (as the WiVRn server checks)
fn is_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

/// A host program: a Flatpak's own /proc and processes are the sandbox's
fn host_command(program: &str) -> std::process::Command {
    if is_flatpak() {
        let mut command = std::process::Command::new("flatpak-spawn");
        command.args(["--host", program]);
        command
    } else {
        std::process::Command::new(program)
    }
}

/// Every process with its arguments
fn processes() -> anyhow::Result<Vec<(i32, Vec<String>)>> {
    if is_flatpak() {
        let output = host_command("pgrep").args(["-a", "-f", "SteamLaunch"]).output()?;
        return Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut words = line.split_whitespace();
                let pid = words.next()?.parse().ok()?;
                Some((pid, words.map(str::to_owned).collect()))
            })
            .collect());
    }
    let mut res = Vec::new();
    for entry in std::fs::read_dir("/proc")?.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args = cmdline
            .split(|byte| *byte == 0x00)
            .filter_map(|arg| std::str::from_utf8(arg).ok())
            .map(str::to_owned)
            .collect();
        res.push((pid, args));
    }
    Ok(res)
}

pub fn list_running_games() -> anyhow::Result<Vec<RunningGame>> {
    let mut res = Vec::<RunningGame>::new();

    for (pid, args) in processes()? {
        if !args.iter().any(|arg| arg == "SteamLaunch") {
            continue;
        }

        for arg in &args {
            let Some(app_id) = arg.strip_prefix("AppId=") else {
                continue;
            };

            let Ok(app_id_num) = app_id.parse::<u64>() else {
                continue;
            };

            res.push(RunningGame {
                app_id: app_id_num.to_string(),
                pid,
            });
            break;
        }
    }

    Ok(res)
}

pub fn stop(app_id: &str, force_kill: bool) -> anyhow::Result<()> {
    eprintln!("Stopping Steam game with AppID {}", app_id);
    for game in list_running_games()? {
        if game.app_id != app_id {
            continue;
        }

        eprintln!(
            "Stopping Steam game with AppID {} and PID {}, force={}",
            app_id, game.pid, force_kill
        );

        host_command("pkill")
            .arg(if force_kill { "-9" } else { "-15" })
            .arg("-P")
            .arg(format!("{}", game.pid))
            .status()?;
    }

    Ok(())
}
