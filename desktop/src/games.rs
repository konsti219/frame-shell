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

pub fn list_running_games() -> anyhow::Result<Vec<RunningGame>> {
    let mut res = Vec::<RunningGame>::new();

    let entries = std::fs::read_dir("/proc")?;
    for entry in entries.into_iter().flatten() {
        let path_cmdline = entry.path().join("cmdline");
        let Ok(cmdline) = std::fs::read(path_cmdline) else {
            continue;
        };

        let proc_file_name = entry.file_name();
        let Some(pid) = proc_file_name.to_str() else {
            continue;
        };

        let Ok(pid) = pid.parse::<i32>() else {
            continue;
        };

        let args: Vec<&str> = cmdline
            .split(|byte| *byte == 0x00)
            .filter_map(|arg| std::str::from_utf8(arg).ok())
            .collect();

        if !args.contains(&"SteamLaunch") {
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

        std::process::Command::new("pkill")
            .arg(if force_kill { "-9" } else { "-15" })
            .arg("-P")
            .arg(format!("{}", game.pid))
            .status()?;
    }

    Ok(())
}
