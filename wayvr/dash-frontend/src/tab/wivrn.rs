//! WiVRn: the PCs the headset streams from (the frame shell's WiVRn client), one page per PC.

use std::{
	marker::PhantomData,
	rc::Rc,
	time::{Duration, Instant},
};

use serde_json::Value;
use wgui::{
	assets::AssetPathRef,
	components::{button::ComponentButton, checkbox::ComponentCheckbox, slider::ComponentSlider},
	globals::WguiGlobals,
	i18n::Translation,
	layout::{Layout, WidgetID},
	parser::{Fetchable, ParseDocumentParams, ParserData, ParserState, TemplateParams},
	task::Tasks,
	widget::label::WidgetLabel,
};

use crate::{
	frontend::{Frontend, FrontendTask},
	tab::{Tab, TabType},
	util::wgui_simple,
};

#[derive(Clone)]
enum Task {
	Select(Rc<str>),
	Command(String),
	Forget(Rc<str>),
	Setting(&'static str, f32),
	// Clicking the selected choice toggled its look off: draw the page again
	Redraw,
}

/// A known PC from the WiVRn client's lobby state
#[derive(PartialEq)]
pub struct Host {
	pub cookie: Rc<str>,
	pub name: Rc<str>,
	pub hostname: String,
	pub visible: bool,
	pub compatible: bool,
	pub autoconnect: bool,
}

/// The WiVRn client's lobby and stream state, parsed
#[derive(PartialEq)]
pub struct WivrnState {
	pub state: String,
	/// Server the lobby is connecting or connected to (its name)
	pub server: String,
	pub detail: String,
	pub hosts: Vec<Host>,
	/// Digits typed on the PIN pad so far
	pub pin_typed: usize,
	pub stream: Value,
	pub settings: Value,
	pub game_running: bool,
}

fn text(value: &Value, key: &str) -> String {
	value.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn flag(value: &Value, key: &str) -> bool {
	value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

impl WivrnState {
	pub fn parse(lobby: &str, stream: &str) -> Self {
		let lobby: Value = serde_json::from_str(lobby).unwrap_or(Value::Null);
		let stream: Value = serde_json::from_str(stream).unwrap_or(Value::Null);
		let hosts = lobby
			.get("servers")
			.and_then(Value::as_array)
			.into_iter()
			.flatten()
			.map(|server| Host {
				cookie: text(server, "cookie").into(),
				name: text(server, "name").into(),
				hostname: text(server, "hostname"),
				visible: flag(server, "visible"),
				compatible: flag(server, "compatible"),
				autoconnect: flag(server, "autoconnect"),
			})
			.collect();
		Self {
			game_running: stream
				.get("running")
				.and_then(Value::as_array)
				.is_some_and(|apps| apps.iter().any(|app| !flag(app, "overlay"))),
			settings: if text(&lobby, "state") == "connected" {
				stream.get("settings").or_else(|| lobby.get("settings"))
			} else {
				lobby.get("settings")
			}
			.cloned()
			.unwrap_or(Value::Null),
			state: text(&lobby, "state"),
			server: text(&lobby, "server"),
			detail: text(&lobby, "detail"),
			hosts,
			pin_typed: lobby.get("pin_typed").and_then(Value::as_u64).unwrap_or(0) as usize,
			stream,
		}
	}

	/// Whether this PC is the one being connected to or streamed from
	pub fn is_active(&self, host: &Host) -> bool {
		!self.server.is_empty() && *host.name == *self.server && self.state != "idle" && self.state != "error"
	}

	pub fn connected_to(&self, host: &Host) -> bool {
		self.is_active(host) && self.state == "connected"
	}
}

pub struct TabWivrn<T> {
	#[allow(dead_code)]
	state: ParserState,
	tasks: Tasks<Task>,
	marker: PhantomData<T>,

	id_hosts: WidgetID,
	id_content: WidgetID,
	/// Template instances (their components) of the current host bar and page
	cells: Vec<ParserData>,

	version: Option<u64>,
	page_state: Option<WivrnState>,
	settings_cell: Option<usize>,
	selected: Option<Rc<str>>,
	confirm_forget: Option<Rc<str>>,
	/// Slider changes wait until the drag rests before rebuilding the page
	pending_setting: Option<(&'static str, f32, Instant)>,
}

fn doc_params(globals: &WguiGlobals) -> ParseDocumentParams<'_> {
	ParseDocumentParams {
		globals: globals.clone(),
		path: AssetPathRef::BuiltIn("gui/tab/wivrn.xml"),
		extra: Default::default(),
	}
}

impl<T> Tab<T> for TabWivrn<T> {
	fn get_type(&self) -> TabType {
		TabType::Wivrn
	}

	fn update(&mut self, frontend: &mut Frontend<T>, _time_ms: u32, data: &mut T) -> anyhow::Result<()> {
		let mut dirty = false;
		for task in self.tasks.drain() {
			match task {
				Task::Select(cookie) => {
					self.selected = Some(cookie);
					self.confirm_forget = None;
					dirty = true;
				}
				Task::Command(command) => frontend.tasks.push(FrontendTask::WivrnCommand(command.into())),
				Task::Setting(key, value) => self.pending_setting = Some((key, value, Instant::now())),
				Task::Redraw => dirty = true,
				Task::Forget(cookie) => {
					if self.confirm_forget.as_ref() == Some(&cookie) {
						frontend
							.tasks
							.push(FrontendTask::WivrnCommand(format!("forget {cookie}").into()));
						self.confirm_forget = None;
						self.selected = None;
					} else {
						self.confirm_forget = Some(cookie);
					}
					dirty = true;
				}
			}
		}

		let dragging = self.cells.iter().any(|cell| {
			["bitrate", "resolution", "foveation", "desktop_bitrate", "fec", "frame_margin"]
				.iter()
				.any(|id| {
					cell
						.fetch_component_as::<ComponentSlider>(id)
						.is_ok_and(|slider| slider.is_dragging())
				})
		});
		// Apply after release so a state update cannot interrupt the drag
		if let Some((key, value, changed)) = self.pending_setting
			&& !dragging
			&& changed.elapsed() > Duration::from_millis(500)
		{
			self.pending_setting = None;
			let value = match key {
				"bitrate" => format!("{:.0}", value * 1_000_000.0),
				"desktop_bitrate" => format!("{:.0}", value * 1000.0),
				"fec" | "frame_margin" => format!("{value:.0}"),
				_ => format!("{:.2}", value / 100.0),
			};
			frontend
				.tasks
				.push(FrontendTask::WivrnCommand(format!("setting {key} {value}").into()));
		}

		let Some((version, lobby, stream)) = frontend.interface.wivrn_state(data) else {
			if self.version.is_none() {
				self.version = Some(0);
				frontend.layout.remove_children(self.id_content);
				wgui_simple::create_label(
					&mut frontend.layout,
					self.id_content,
					Translation::from_raw_text("WiVRn is only available in the headset shell"),
				)?;
			}
			return Ok(());
		};
		if dirty || self.version != Some(version) {
			let mut state = WivrnState::parse(&lobby, &stream);
			let stream = std::mem::take(&mut state.stream);
			let can_refresh = self.pending_setting.is_none() && !dragging;
			if dirty || (can_refresh && self.page_state.as_ref() != Some(&state)) {
				self.rebuild(&mut frontend.layout, &frontend.globals, &state)?;
				self.page_state = Some(state);
			}
			self.update_stream_debug(&mut frontend.layout, &stream)?;
			if can_refresh {
				self.version = Some(version);
			}
		}
		Ok(())
	}
}

impl<T> TabWivrn<T> {
	pub fn new(frontend: &mut Frontend<T>, parent_id: WidgetID) -> anyhow::Result<Self> {
		let globals = frontend.layout.state.globals.clone();
		let state = wgui::parser::parse_from_assets(&doc_params(&globals), &mut frontend.layout, parent_id)?;
		Ok(Self {
			id_hosts: state.get_widget_id("hosts_root")?,
			id_content: state.get_widget_id("content")?,
			state,
			tasks: Tasks::new(),
			marker: PhantomData,
			cells: Vec::new(),
			version: None,
			page_state: None,
			settings_cell: None,
			selected: None,
			confirm_forget: None,
			pending_setting: None,
		})
	}

	fn template(
		&mut self,
		layout: &mut Layout,
		globals: &WguiGlobals,
		name: &str,
		parent: WidgetID,
		params: TemplateParams,
	) -> anyhow::Result<usize> {
		let data = self
			.state
			.parse_template_only(&doc_params(globals), name, layout, parent, params)?;
		self.cells.push(data);
		Ok(self.cells.len() - 1)
	}

	fn button(&self, cell: usize, id: &str) -> anyhow::Result<Rc<ComponentButton>> {
		self.cells[cell].fetch_component_as::<ComponentButton>(id)
	}

	fn rebuild(&mut self, layout: &mut Layout, globals: &WguiGlobals, wivrn: &WivrnState) -> anyhow::Result<()> {
		layout.remove_children(self.id_hosts);
		layout.remove_children(self.id_content);
		self.cells.clear();
		self.settings_cell = None;

		if wivrn.hosts.is_empty() {
			wgui_simple::create_label(
				layout,
				self.id_content,
				Translation::from_raw_text("No PCs found yet. Start WiVRn on a PC on this network."),
			)?;
			self.stream_settings(layout, globals, &wivrn.settings, wivrn.game_running)?;
			return Ok(());
		}

		// Keep the selection; otherwise the active PC (or pairing), else the first
		let selected = self
			.selected
			.clone()
			.filter(|cookie| wivrn.hosts.iter().any(|host| host.cookie == *cookie))
			.or_else(|| {
				wivrn
					.hosts
					.iter()
					.find(|host| wivrn.is_active(host))
					.map(|host| host.cookie.clone())
			})
			.unwrap_or_else(|| wivrn.hosts[0].cookie.clone());
		self.selected = Some(selected.clone());

		for host in &wivrn.hosts {
			let mut params = TemplateParams::new();
			params.insert("name", &host.name);
			params.insert("detail", &host.hostname);
			let cell = self.template(layout, globals, "HostButton", self.id_hosts, params)?;
			let button = self.button(cell, "btn")?;
			button.set_sticky_state_immediate(&mut layout.common(), host.cookie == selected);
			button.on_click(self.tasks.get_button_click_callback(Task::Select(host.cookie.clone())));
		}

		let Some(host) = wivrn.hosts.iter().find(|host| host.cookie == selected) else {
			return Ok(());
		};
		let active = wivrn.is_active(host);
		let status = if active {
			match wivrn.state.as_str() {
				"connected" => "Connected".to_owned(),
				"connecting" => format!("Connecting: {}", wivrn.detail),
				"pin" => "Pairing: enter the PIN shown on the PC".to_owned(),
				_ => wivrn.state.clone(),
			}
		} else if !host.visible {
			"Not found on the network".to_owned()
		} else if !host.compatible {
			"Incompatible WiVRn version".to_owned()
		} else if wivrn.state == "error" && *host.name == *wivrn.server {
			format!("Not connected: {}", wivrn.detail)
		} else {
			"Not connected".to_owned()
		};
		let can_connect = !active && host.visible && host.compatible && wivrn.state != "connected";

		let mut params = TemplateParams::new();
		params.insert("name", &host.name);
		params.insert("hostname", &host.hostname);
		params.insert("status", &status);
		params.insert("show_connect", if can_connect { "flex" } else { "none" });
		params.insert("show_disconnect", if active { "flex" } else { "none" });
		params.insert(
			"disconnect_label",
			if wivrn.state == "connected" {
				"Disconnect"
			} else {
				"Cancel"
			},
		);
		params.insert(
			"auto_label",
			if host.autoconnect {
				"Auto-connect: on"
			} else {
				"Auto-connect: off"
			},
		);
		let page = self.template(layout, globals, "HostPage", self.id_content, params)?;
		self.button(page, "btn_connect")?.on_click(
			self
				.tasks
				.get_button_click_callback(Task::Command(format!("connect {}", host.cookie))),
		);
		self
			.button(page, "btn_disconnect")?
			.on_click(self.tasks.get_button_click_callback(Task::Command("disconnect".into())));
		self
			.button(page, "btn_auto")?
			.on_click(self.tasks.get_button_click_callback(Task::Command(format!(
				"autoconnect {} {}",
				host.cookie,
				if host.autoconnect { 0 } else { 1 }
			))));
		let forget = self.button(page, "btn_forget")?;
		if self.confirm_forget.as_ref() == Some(&host.cookie) {
			forget.set_text(
				&mut layout.common(),
				Translation::from_raw_text("Click again to forget"),
			);
		}
		forget.on_click(self.tasks.get_button_click_callback(Task::Forget(host.cookie.clone())));

		if active && wivrn.state == "pin" {
			let shown: String = (0..6).map(|i| if i < wivrn.pin_typed { '•' } else { '-' }).collect();
			let mut params = TemplateParams::new();
			params.insert("pin", &shown);
			let pad = self.template(layout, globals, "PinPad", self.id_content, params)?;
			let rows = self.cells[pad].get_widget_id("rows")?;
			for keys in [
				["1", "2", "3"],
				["4", "5", "6"],
				["7", "8", "9"],
				["Del", "0", "Cancel"],
			] {
				let row = self.template(layout, globals, "PinRow", rows, TemplateParams::new())?;
				let row = self.cells[row].get_widget_id("row")?;
				for key in keys {
					let mut params = TemplateParams::new();
					params.insert("label", key);
					let cell = self.template(layout, globals, "PinKey", row, params)?;
					let command = match key {
						"Del" => "pin_back".to_owned(),
						"Cancel" => "pin_cancel".to_owned(),
						digit => format!("pin_digit {digit}"),
					};
					self
						.button(cell, "btn")?
						.on_click(self.tasks.get_button_click_callback(Task::Command(command)));
				}
			}
		}

		self.stream_settings(layout, globals, &wivrn.settings, wivrn.game_running)?;
		Ok(())
	}

	fn stream_settings(
		&mut self,
		layout: &mut Layout,
		globals: &WguiGlobals,
		settings: &Value,
		game_running: bool,
	) -> anyhow::Result<()> {
		let Some(bitrate) = settings.get("bitrate").and_then(Value::as_u64) else {
			return Ok(());
		};
		let max_bitrate = settings
			.get("max_bitrate")
			.and_then(Value::as_u64)
			.unwrap_or(200_000_000);
		let mut params = TemplateParams::new();
		params.insert_str("max_bitrate", format!("{}", max_bitrate / 1_000_000));
		params.insert_str(
			"max_foveation",
			settings
				.get("max_foveation")
				.and_then(Value::as_u64)
				.unwrap_or(100)
				.to_string(),
		);

		let desktop_codec = settings.get("desktop_codec").and_then(Value::as_str).unwrap_or("h264");
		let cell = self.template(layout, globals, "StreamSettings", self.id_content, params)?;
		self.settings_cell = Some(cell);
		let codec = settings.get("codec").and_then(Value::as_str).unwrap_or("auto");
		for (key, selected, choices) in [
			("codec", codec, &["auto", "h264", "h265", "av1"][..]),
			("desktop_codec", desktop_codec, &["h264", "h265"][..]),
		] {
			for choice in choices {
				let button = self.button(cell, &format!("{key}_{choice}"))?;
				button.set_sticky_state_immediate(&mut layout.common(), *choice == selected);
				let available = *choice == "auto" || settings["codec_support"][*choice].as_bool().unwrap_or(false);
				button.set_enabled(&mut layout.common(), available);
				button.on_click(self.tasks.get_button_click_callback(if *choice == selected {
					Task::Redraw
				} else {
					Task::Command(format!("setting {key} {choice}"))
				}));
			}
		}

		for (key, default) in [("foveation_debug", false)] {
			let checkbox = self.cells[cell].fetch_component_as::<ComponentCheckbox>(key)?;
			checkbox.set_checked(
				&mut layout.common(),
				settings.get(key).and_then(Value::as_bool).unwrap_or(default),
			);
			let tasks = self.tasks.clone();
			checkbox.on_toggle(Box::new(move |_, event| {
				tasks.push(Task::Command(format!("setting {key} {}", u8::from(event.checked))));
				Ok(())
			}));
		}
		for (key, value) in [
			("bitrate", bitrate as f32 / 1_000_000.0),
			(
				"resolution",
				settings.get("resolution").and_then(Value::as_f64).unwrap_or(1.0) as f32 * 100.0,
			),
			(
				"foveation",
				settings.get("foveation").and_then(Value::as_f64).unwrap_or(1.0) as f32 * 100.0,
			),
			("fec", settings.get("fec").and_then(Value::as_f64).unwrap_or(0.0) as f32),
			("frame_margin", settings.get("frame_margin").and_then(Value::as_f64).unwrap_or(1.0) as f32),
			(
				"desktop_bitrate",
				settings
					.get("desktop_bitrate")
					.and_then(Value::as_f64)
					.unwrap_or(16000.0) as f32
					/ 1000.0,
			),
		] {
			let slider = self.cells[cell].fetch_component_as::<ComponentSlider>(key)?;
			slider.set_value_primary(&mut layout.common(), value);
			if key == "resolution" {
				slider.set_enabled(&mut layout.common(), !game_running);
			}
			let tasks = self.tasks.clone();
			slider.on_value_changed(Box::new(move |_common, e| tasks.push(Task::Setting(key, e.value))));
		}
		Ok(())
	}

	fn update_stream_debug(&self, layout: &mut Layout, stream: &Value) -> anyhow::Result<()> {
		let Some(cell) = self.settings_cell else {
			return Ok(());
		};
		let debug = stream
			.get("video_debug")
			.filter(|value| value.is_object())
			.map(|video| {
				let number = |key| video.get(key).and_then(Value::as_u64).unwrap_or(0);
				format!(
					"Received VR video per eye: {}×{} → {}×{}; foveation {}",
					number("encoded_width"),
					number("encoded_height"),
					number("render_width"),
					number("render_height"),
					if video.get("foveated").and_then(Value::as_bool).unwrap_or(false) {
						"active"
					} else {
						"off"
					}
				)
			})
			.unwrap_or_else(|| "Foveation debug: waiting for a VR video frame".to_owned());
		let network = stream
			.get("network")
			.filter(|value| value.is_object())
			.map(|network| {
				let number = |key| {
					network
						.get(key)
						.and_then(Value::as_f64)
						.map_or_else(|| "?".to_owned(), |v| format!("{v:.0}"))
				};
				format!(
					"Wi-Fi: signal {} dBm, {} Mbit/s to the PC, {} Mbit/s from it",
					number("signal"),
					number("tx_mbit"),
					number("rx_mbit")
				)
			})
			.unwrap_or_default();
		let vr = stream
			.get("vr_debug")
			.filter(|value| value.is_object())
			.map(|vr| {
				let number = |key| vr.get(key).and_then(Value::as_u64).unwrap_or(0);
				format!(
					"PC VR: {} repeated frames (jumps), {} freezes over 50 ms, longest {} ms (last 5 s)",
					number("repeats"),
					number("freezes"),
					number("longest_ms")
				)
			})
			.unwrap_or_default();
		let eyes = stream
			.get("eyes_debug")
			.filter(|value| value.is_object())
			.map(|eyes| {
				let state = eyes.get("state").and_then(Value::as_str).unwrap_or("?");
				if state != "tracking" {
					return format!("Eye tracking: {state}");
				}
				format!(
					"Eye tracking: {:.0} updates/s{}{}",
					eyes.get("update_hz").and_then(Value::as_f64).unwrap_or(0.0),
					if eyes.get("calibrated").and_then(Value::as_bool).unwrap_or(false) { "" } else { ", calibrating" },
					if eyes.get("gaze").and_then(Value::as_bool).unwrap_or(false) { "" } else { ", no gaze" }
				)
			})
			.unwrap_or_default();
		for (id, text) in [("video_debug", debug), ("network_status", network), ("vr_debug", vr), ("eyes_debug", eyes)] {
			let mut common = layout.common();
			let mut label = self.cells[cell].fetch_widget_as::<WidgetLabel>(common.state, id)?;
			label.set_text(&mut common, Translation::from_raw_text_string(text));
		}
		Ok(())
	}
}
