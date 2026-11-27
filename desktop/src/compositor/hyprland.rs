//! Hyprland IPC backend over `.socket.sock` / `.socket2.sock` paired with `zwlr_output_manager_v1`.

use super::{
    CompositorBackend, CompositorEvent, MonitorGeometry, WindowInfo, WorkspaceInfo,
    normalize_window_address, wlr_output,
};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    time::{Duration, Instant},
};

pub struct HyprlandBackend {
    cmd_socket: PathBuf,
    event_socket: PathBuf,
    lua_dispatch: bool,
}

#[derive(Deserialize)]
struct HyprWorkspaceRef {
    #[serde(default)]
    id: Option<i32>,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HyprMonitor {
    id: i64,
    name: String,
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    #[serde(default = "default_scale")]
    scale: f32,
    #[serde(default)]
    focused: bool,
    #[serde(default)]
    disabled: bool,
    active_workspace: Option<HyprWorkspaceRef>,
}

fn default_scale() -> f32 {
    1.0
}

#[derive(Deserialize)]
struct HyprClient {
    address: String,
    #[serde(default)]
    mapped: bool,
    #[serde(default)]
    hidden: bool,
    workspace: HyprWorkspaceRef,
    monitor: Option<i64>,
    #[serde(default)]
    class: String,
    #[serde(default, rename = "initialClass")]
    initial_class: String,
    #[serde(default)]
    title: String,
}

#[derive(Deserialize)]
struct HyprWorkspace {
    #[serde(default)]
    id: Option<i32>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    monitor: String,
    #[serde(default)]
    windows: u32,
}

impl HyprlandBackend {
    pub fn from_env() -> Result<Self> {
        let runtime_dir =
            std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
        let sig = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")
            .context("HYPRLAND_INSTANCE_SIGNATURE is not set")?;
        let base = PathBuf::from(runtime_dir).join("hypr").join(sig);
        let cmd_socket = base.join(".socket.sock");
        let event_socket = base.join(".socket2.sock");
        if !cmd_socket.exists() {
            bail!("Missing Hyprland socket at {}", cmd_socket.display());
        }
        let mut backend = Self {
            cmd_socket,
            event_socket,
            lua_dispatch: false,
        };
        backend.lua_dispatch = backend
            .request("repl return type(hl)")
            .is_ok_and(|reply| reply.trim() == "table");
        Ok(backend)
    }

    fn request(&self, payload: &str) -> Result<String> {
        let mut stream = UnixStream::connect(&self.cmd_socket)
            .with_context(|| format!("Connect {}", self.cmd_socket.display()))?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        stream.write_all(payload.as_bytes())?;
        let mut buf = String::new();
        stream.read_to_string(&mut buf)?;
        Ok(buf)
    }

    fn request_ok(&self, payload: &str) -> Result<()> {
        let reply = self.request(payload)?;
        let trimmed = reply.trim();
        if trimmed == "ok" {
            Ok(())
        } else {
            bail!("Hyprland command `{payload}` failed: {trimmed}");
        }
    }
}

impl CompositorBackend for HyprlandBackend {
    fn name(&self) -> &'static str {
        "hyprland"
    }

    fn list_monitors(&self) -> Result<Vec<MonitorGeometry>> {
        let json = self.request("j/monitors")?;
        let raw: Vec<HyprMonitor> =
            serde_json::from_str(&json).context("Parse j/monitors JSON")?;
        Ok(raw
            .into_iter()
            .filter(|m| !m.disabled)
            .map(|m| {
                let scale = if m.scale > 0.05 { m.scale } else { 1.0 };
                let logical_w = ((f32::from(m.width as u16) / scale).round() as i32).max(1);
                let logical_h = ((f32::from(m.height as u16) / scale).round() as i32).max(1);
                let (ws_id, ws_name) = m
                    .active_workspace
                    .map_or((0, String::new()), |ws| (ws.id.unwrap_or(0), ws.name));
                MonitorGeometry {
                    id: m.id,
                    name: m.name,
                    width: m.width,
                    height: m.height,
                    logical_pos: (m.x, m.y),
                    logical_size: (logical_w, logical_h),
                    scale,
                    focused: m.focused,
                    active_workspace_id: ws_id,
                    active_workspace_name: ws_name,
                }
            })
            .collect())
    }

    fn list_windows(&self) -> Result<Vec<WindowInfo>> {
        let monitors = self.list_monitors()?;
        let monitor_names: HashMap<i64, String> =
            monitors.into_iter().map(|m| (m.id, m.name)).collect();
        let json = self.request("j/clients")?;
        let raw: Vec<HyprClient> = serde_json::from_str(&json).context("Parse j/clients JSON")?;
        Ok(raw
            .into_iter()
            .filter(|c| c.mapped && !c.hidden && c.workspace.id.is_some_and(|id| id > 0))
            .map(|c| {
                let monitor_id = c.monitor.unwrap_or(-1);
                let monitor_name = monitor_names
                    .get(&monitor_id)
                    .cloned()
                    .unwrap_or_default();
                let app_id = if !c.class.is_empty() {
                    c.class
                } else {
                    c.initial_class
                };
                WindowInfo {
                    address: normalize_window_address(&c.address),
                    title: c.title,
                    app_id,
                    workspace_id: c.workspace.id.unwrap_or(0),
                    workspace_name: c.workspace.name,
                    monitor_id,
                    monitor_name,
                    mapped: c.mapped,
                    hidden: c.hidden,
                }
            })
            .collect())
    }

    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>> {
        let json = self.request("j/workspaces")?;
        let raw: Vec<HyprWorkspace> =
            serde_json::from_str(&json).context("Parse j/workspaces JSON")?;
        Ok(raw
            .into_iter()
            .filter(|w| w.id.is_some_and(|id| id > 0))
            .map(|w| WorkspaceInfo {
                id: w.id.unwrap_or(0),
                name: w.name,
                monitor_name: w.monitor,
                windows: w.windows,
            })
            .collect())
    }

    fn create_headless_output(&self, name: &str) -> Result<()> {
        self.request_ok(&format!("output create headless {name}"))?;
        let deadline = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < deadline {
            if self
                .list_monitors()?
                .iter()
                .any(|monitor| monitor.name == name)
            {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        bail!("Timed out waiting for headless output {name} in j/monitors")
    }

    fn configure_outputs(&self, updates: &[wlr_output::OutputConfigUpdate]) -> Result<()> {
        wlr_output::apply_output_layout(updates)
    }

    fn remove_output(&self, name: &str) -> Result<()> {
        self.request_ok(&format!("output remove {name}"))
    }

    fn move_window_to_workspace(&self, address: &str, workspace_target: &str) -> Result<()> {
        let addr = normalize_window_address(address);
        self.request_ok(&format_move_window_cmd(
            self.lua_dispatch,
            &addr,
            workspace_target,
        ))
    }

    fn move_workspace_to_output(&self, workspace_id: i32, output: &str) -> Result<()> {
        let prev_focused = self
            .list_monitors()?
            .into_iter()
            .find(|m| m.focused)
            .map(|m| m.name);
        self.request_ok(&format_move_workspace_cmd(
            self.lua_dispatch,
            workspace_id,
            output,
        ))?;
        let active_on_target = self
            .list_monitors()?
            .into_iter()
            .find(|m| m.name == output)
            .is_some_and(|m| m.active_workspace_id == workspace_id);
        if !active_on_target {
            // Focus the target monitor briefly only if needed to make the moved workspace visible there,
            // then immediately restore focus to the previous monitor.
            let _ = self.request(&format_focus_monitor_cmd(self.lua_dispatch, output));
            let _ = self.request(&format_focus_workspace_cmd(self.lua_dispatch, workspace_id));
            if let Some(prev) = prev_focused
                && prev != output
            {
                let _ = self.request(&format_focus_monitor_cmd(self.lua_dispatch, &prev));
            }
        }
        Ok(())
    }

    fn subscribe_events(&self, tx: Sender<CompositorEvent>, stop: Arc<AtomicBool>) -> Result<()> {
        let stream = UnixStream::connect(&self.event_socket)
            .with_context(|| format!("Connect {}", self.event_socket.display()))?;
        stream.set_read_timeout(Some(Duration::from_millis(200)))?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        while !stop.load(Ordering::Acquire) {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if let Some(event) = parse_socket2_line(&line)
                        && tx.send(event).is_err()
                    {
                        break;
                    }
                }
                Err(err)
                    if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    continue;
                }
                Err(err) => return Err(err.into()),
            }
        }
        Ok(())
    }
}

fn lua_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\0' => {}
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn format_move_window_cmd(lua: bool, addr: &str, workspace_target: &str) -> String {
    if lua {
        let target = workspace_target.trim();
        let ws_expr = match target.parse::<i32>() {
            Ok(id) => id.to_string(),
            Err(_) => lua_quote(target),
        };
        format!(
            "/dispatch hl.dsp.window.move({{ workspace = {ws_expr}, follow = false, window = {} }})",
            lua_quote(&format!("address:{addr}"))
        )
    } else {
        format!("/dispatch movetoworkspacesilent {workspace_target},address:{addr}")
    }
}

fn format_move_workspace_cmd(lua: bool, workspace_id: i32, output: &str) -> String {
    if lua {
        format!(
            "/dispatch hl.dsp.workspace.move({{ workspace = {workspace_id}, monitor = {} }})",
            lua_quote(output)
        )
    } else {
        format!("/dispatch moveworkspacetomonitor {workspace_id} {output}")
    }
}

fn format_focus_monitor_cmd(lua: bool, output: &str) -> String {
    if lua {
        format!("/dispatch hl.dsp.focus({{ monitor = {} }})", lua_quote(output))
    } else {
        format!("/dispatch focusmonitor {output}")
    }
}

fn format_focus_workspace_cmd(lua: bool, workspace_id: i32) -> String {
    if lua {
        format!("/dispatch hl.dsp.focus({{ workspace = {workspace_id} }})")
    } else {
        format!("/dispatch workspace {workspace_id}")
    }
}

pub fn parse_socket2_line(line: &str) -> Option<CompositorEvent> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let (event, data) = trimmed.split_once(">>")?;
    match event {
        "closewindow" => {
            let address = normalize_window_address(data);
            (!address.is_empty()).then_some(CompositorEvent::WindowClosed { address })
        }
        "movewindowv2" => {
            let mut parts = data.splitn(3, ',');
            let address = normalize_window_address(parts.next()?);
            let ws_id = parts.next()?.trim().parse::<i32>().ok()?;
            let ws_name = parts.next()?.trim().to_owned();
            Some(CompositorEvent::WindowMoved {
                address,
                workspace_id: ws_id,
                workspace_name: ws_name,
            })
        }
        "monitorremoved" | "monitorremovedv2" => {
            let name = data.split(',').nth(if event == "monitorremovedv2" { 1 } else { 0 })?;
            let name = name.trim();
            (!name.is_empty()).then(|| CompositorEvent::MonitorRemoved {
                name: name.to_owned(),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_normalization() {
        assert_eq!(normalize_window_address("56384C00C620"), "0x56384c00c620");
        assert_eq!(normalize_window_address("0x5638AbCd"), "0x5638abcd");
        assert_eq!(normalize_window_address("0X0012"), "0x0012");
    }

    #[test]
    fn parse_socket2_events() {
        assert_eq!(
            parse_socket2_line("closewindow>>56384c00c620\n"),
            Some(CompositorEvent::WindowClosed {
                address: "0x56384c00c620".into(),
            })
        );
        assert_eq!(
            parse_socket2_line("movewindowv2>>56384c00c620,12,frame-v1\n"),
            Some(CompositorEvent::WindowMoved {
                address: "0x56384c00c620".into(),
                workspace_id: 12,
                workspace_name: "frame-v1".into(),
            })
        );
        assert_eq!(
            parse_socket2_line("monitorremoved>>FRAME-V1\n"),
            Some(CompositorEvent::MonitorRemoved {
                name: "FRAME-V1".into(),
            })
        );
        assert_eq!(
            parse_socket2_line("monitorremovedv2>>2,FRAME-V1,headless\n"),
            Some(CompositorEvent::MonitorRemoved {
                name: "FRAME-V1".into(),
            })
        );
        assert_eq!(parse_socket2_line("focusedmon>>DP-4,2\n"), None);
    }

    #[test]
    fn lua_quoting_escapes_special_chars() {
        assert_eq!(
            lua_quote("a\\b\"c\nd\re\0f"),
            "\"a\\\\b\\\"c\\nd\\ref\""
        );
    }

    #[test]
    fn dispatch_command_formatting() {
        assert_eq!(
            format_move_window_cmd(false, "0x56384c00c620", "13"),
            "/dispatch movetoworkspacesilent 13,address:0x56384c00c620"
        );
        assert_eq!(
            format_move_window_cmd(true, "0x56384c00c620", "13"),
            "/dispatch hl.dsp.window.move({ workspace = 13, follow = false, window = \"address:0x56384c00c620\" })"
        );
        assert_eq!(
            format_move_window_cmd(true, "0x56384c00c620", " name:foo\"bar "),
            "/dispatch hl.dsp.window.move({ workspace = \"name:foo\\\"bar\", follow = false, window = \"address:0x56384c00c620\" })"
        );

        assert_eq!(
            format_move_workspace_cmd(false, 2, "FRAME-V6"),
            "/dispatch moveworkspacetomonitor 2 FRAME-V6"
        );
        assert_eq!(
            format_move_workspace_cmd(true, 2, "FRAME-V6"),
            "/dispatch hl.dsp.workspace.move({ workspace = 2, monitor = \"FRAME-V6\" })"
        );

        assert_eq!(
            format_focus_monitor_cmd(false, "DP-4"),
            "/dispatch focusmonitor DP-4"
        );
        assert_eq!(
            format_focus_monitor_cmd(true, "DP-4"),
            "/dispatch hl.dsp.focus({ monitor = \"DP-4\" })"
        );

        assert_eq!(format_focus_workspace_cmd(false, 7), "/dispatch workspace 7");
        assert_eq!(
            format_focus_workspace_cmd(true, 7),
            "/dispatch hl.dsp.focus({ workspace = 7 })"
        );
    }
}
