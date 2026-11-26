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
	Setting(Rc<str>, &'static str, f32),
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
	/// Connected at least once: only then can it auto-connect or be forgotten
	pub paired: bool,
	/// Added by address rather than discovered: never shows as visible
	pub manual: bool,
	pub source: Option<u32>,
	pub connection: String,
	pub detail: String,
	pub status: String,
	pub presenting: bool,
	pub version: String,
	pub stream: Value,
	/// Its stream settings
	pub settings: Value,
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
	pub pin_typed: String,
	pub stream: Value,
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
				paired: flag(server, "paired"),
				manual: flag(server, "manual"),
				source: server.get("source").and_then(Value::as_u64).map(|s| s as u32),
				connection: text(server, "state"),
				detail: text(server, "detail"),
				status: text(server, "status"),
				presenting: flag(server, "presenting"),
				version: text(server, "version"),
				stream: server.get("stream").cloned().unwrap_or(Value::Null),
				settings: server.get("settings").cloned().unwrap_or(Value::Null),
			})
			.collect();
		Self {
			state: text(&lobby, "state"),
			server: text(&lobby, "server"),
			detail: text(&lobby, "detail"),
			hosts,
			pin_typed: text(&lobby, "pin_typed"),
			stream,
		}
	}

	/// A PC whose VR app runs can't change its render size
	pub fn game_running(&self, host: &Host) -> bool {
		self.connected_to(host)
			&& (!flag(&host.stream, "runtime_known")
				|| host
					.stream
					.get("running")
					.and_then(Value::as_array)
					.is_some_and(|apps| apps.iter().any(|app| !flag(app, "overlay"))))
	}

	pub fn is_active(&self, host: &Host) -> bool {
		matches!(host.connection.as_str(), "connecting" | "pin" | "connected")
	}
	pub fn connected_to(&self, host: &Host) -> bool {
		host.source.is_some() && host.connection == "connected"
	}

}

pub struct TabWivrn<T> {
	#[allow(dead_code)]
	state: ParserState,
	tasks: Tasks<Task>,
	marker: PhantomData<T>,

	id_hosts: WidgetID,
	id_content: WidgetID,
	id_settings: WidgetID,
	/// Template instances (their components) of the current host bar and page
	cells: Vec<ParserData>,
	/// The stream settings shown, kept across page rebuilds so their sliders don't flash
	settings: Option<Settings>,

	version: Option<u64>,
	page_state: Option<WivrnState>,
	selected: Option<Rc<str>>,
	confirm_forget: Option<Rc<str>>,
	/// Slider changes wait until the drag rests before rebuilding the page
	pending_setting: Option<(Rc<str>, &'static str, f32, Instant)>,
}

/// A PC's stream settings section, created for a PC and its limits, updated in place otherwise
struct Settings {
	cookie: Rc<str>,
	limits: (u64, u64),
	shown: Option<(Value, bool)>,
	data: ParserData,
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
				Task::Setting(cookie, key, value) => self.pending_setting = Some((cookie, key, value, Instant::now())),
				Task::Redraw => dirty = true,
				Task::Forget(cookie) => {
					if self.confirm_forget.as_ref() == Some(&cookie) {
						frontend
							.tasks
							.push(FrontendTask::WivrnCommand(format!("forget {cookie}").into()));
						self.confirm_forget = None;
						if self.pending_setting.as_ref().is_some_and(|(host, ..)| host == &cookie) {
							self.pending_setting = None;
						}
					} else {
						self.confirm_forget = Some(cookie);
					}
					dirty = true;
				}
			}
		}

		let dragging = self.settings.iter().map(|settings| &settings.data).any(|cell| {
			[
				"bitrate",
				"resolution",
				"foveation",
				"desktop_bitrate",
				"fec",
				"frame_margin",
			]
			.iter()
			.any(|id| {
				cell
					.fetch_component_as::<ComponentSlider>(id)
					.is_ok_and(|slider| slider.is_dragging())
			})
		});
		// Apply after release so a state update cannot interrupt the drag
		if let Some((cookie, key, value, changed)) = self.pending_setting.clone()
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
				.push(FrontendTask::WivrnCommand(format!("setting {cookie} {key} {value}").into()));
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
			let can_refresh = self.pending_setting.is_none() && !dragging && self.confirm_forget.is_none();
			if dirty || (can_refresh && self.page_state.as_ref() != Some(&state)) {
				self.rebuild(&mut frontend.layout, &frontend.globals, &state)?;
				self.page_state = Some(state);
			}
			// The live state is the presenting PC's; other PCs show theirs as of the page
			let selected = self
				.page_state
				.as_ref()
				.and_then(|state| state.hosts.iter().find(|host| self.selected.as_ref() == Some(&host.cookie)))
				.filter(|host| !host.presenting)
				.map(|host| host.stream.clone());
			self.update_stream_links(&mut frontend.layout, selected.as_ref().unwrap_or(&stream))?;
			self.update_stream_debug(&mut frontend.layout, selected.as_ref().unwrap_or(&stream))?;
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
			id_settings: state.get_widget_id("settings")?,
			state,
			tasks: Tasks::new(),
			marker: PhantomData,
			cells: Vec::new(),
			version: None,
			page_state: None,
			settings: None,
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

		if wivrn.hosts.is_empty() {
			layout.remove_children(self.id_settings);
			self.settings = None;
			wgui_simple::create_label(
				layout,
				self.id_content,
				Translation::from_raw_text("No PCs found yet. Start WiVRn on a PC on this network."),
			)?;
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
		let status = format!(
			"{}{}{}",
			host.status,
			if host.version.is_empty() { "" } else { " · " },
			host.version
		);
		let can_connect = !active && host.compatible;

		let mut params = TemplateParams::new();
		params.insert("name", &host.name);
		params.insert("hostname", &host.hostname);
		params.insert("status", &status);
		params.insert("show_connect", if can_connect { "flex" } else { "none" });
		params.insert("connect_label", "Connect");
		params.insert(
			"show_disconnect",
			if active {
				"flex"
			} else {
				"none"
			},
		);
		params.insert(
			"disconnect_label",
			if host.connection != "connected" {
				"Cancel"
			} else {
				"Disconnect"
			},
		);
		params.insert("show_auto", if host.paired { "flex" } else { "none" });
		params.insert("show_forget", if host.paired && !active { "flex" } else { "none" });
		params.insert(
			"forget_label",
			if self.confirm_forget.as_ref() == Some(&host.cookie) {
				"Forget (click again)"
			} else {
				"Forget"
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
		let disconnect = format!("disconnect {}", host.cookie);
		self
			.button(page, "btn_disconnect")?
			.on_click(self.tasks.get_button_click_callback(Task::Command(disconnect)));
		self
			.button(page, "btn_auto")?
			.on_click(self.tasks.get_button_click_callback(Task::Command(format!(
				"autoconnect {} {}",
				host.cookie,
				if host.autoconnect { 0 } else { 1 }
			))));
		self.button(page, "btn_forget")?.on_click(self.tasks.get_button_click_callback(Task::Forget(host.cookie.clone())));

		if host.connection == "pin" {
			let shown: String = wivrn.pin_typed.chars().chain(std::iter::repeat('-')).take(6).collect();
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

		self.stream_settings(layout, globals, &host.cookie, &host.settings, wivrn.game_running(host))?;
		Ok(())
	}

	fn stream_settings(
		&mut self,
		layout: &mut Layout,
		globals: &WguiGlobals,
		cookie: &Rc<str>,
		settings: &Value,
		game_running: bool,
	) -> anyhow::Result<()> {
		let Some(bitrate) = settings.get("bitrate").and_then(Value::as_u64) else {
			layout.remove_children(self.id_settings);
			self.settings = None;
			return Ok(());
		};
		let max_bitrate = settings
			.get("max_bitrate")
			.and_then(Value::as_u64)
			.unwrap_or(200_000_000);
		let max_foveation = settings.get("max_foveation").and_then(Value::as_u64).unwrap_or(100);
		if self
			.settings
			.as_ref()
			.is_none_or(|shown| shown.cookie != *cookie || shown.limits != (max_bitrate, max_foveation))
		{
			let mut params = TemplateParams::new();
			params.insert_str("max_bitrate", format!("{}", max_bitrate / 1_000_000));
			params.insert_str("max_foveation", max_foveation.to_string());
			layout.remove_children(self.id_settings);
			let data = self.state.parse_template_only(
				&doc_params(globals),
				"StreamSettings",
				layout,
				self.id_settings,
				params,
			)?;
			for (mode, ..) in LINK_MODES {
				data
					.fetch_component_as::<ComponentButton>(&format!("link_mode_{mode}"))?
					.on_click(
						self
							.tasks
							.get_button_click_callback(Task::Command(format!("linkmode {mode}"))),
					);
			}
			self.settings = Some(Settings {
				cookie: cookie.clone(),
				limits: (max_bitrate, max_foveation),
				shown: None,
				data,
			});
		}
		let section = self.settings.as_mut().expect("created above");
		let shown = Some((settings.clone(), game_running));
		if section.shown == shown {
			return Ok(());
		}
		section.shown = shown;
		let cell = &section.data;

		let desktop_codec = settings.get("desktop_codec").and_then(Value::as_str).unwrap_or("h264");
		let codec = settings.get("codec").and_then(Value::as_str).unwrap_or("auto");
		for (key, selected, choices) in [
			("codec", codec, &["auto", "h264", "h265", "av1"][..]),
			("desktop_codec", desktop_codec, &["h264", "h265"][..]),
		] {
			for choice in choices {
				let button = cell.fetch_component_as::<ComponentButton>(&format!("{key}_{choice}"))?;
				button.set_sticky_state_immediate(&mut layout.common(), *choice == selected);
				let available = *choice == "auto" || settings["codec_support"][*choice].as_bool().unwrap_or(false);
				button.set_enabled(&mut layout.common(), available);
				button.on_click(self.tasks.get_button_click_callback(if *choice == selected {
					Task::Redraw
				} else {
					Task::Command(format!("setting {cookie} {key} {choice}"))
				}));
			}
		}

		for (key, default) in [("foveation_debug", false)] {
			let checkbox = cell.fetch_component_as::<ComponentCheckbox>(key)?;
			checkbox.set_checked(
				&mut layout.common(),
				settings.get(key).and_then(Value::as_bool).unwrap_or(default),
			);
			let tasks = self.tasks.clone();
			let cookie = cookie.clone();
			checkbox.on_toggle(Box::new(move |_, event| {
				tasks.push(Task::Command(format!("setting {cookie} {key} {}", u8::from(event.checked))));
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
			(
				"frame_margin",
				settings.get("frame_margin").and_then(Value::as_f64).unwrap_or(1.0) as f32,
			),
			(
				"desktop_bitrate",
				settings
					.get("desktop_bitrate")
					.and_then(Value::as_f64)
					.unwrap_or(16000.0) as f32
					/ 1000.0,
			),
		] {
			let slider = cell.fetch_component_as::<ComponentSlider>(key)?;
			slider.set_value_primary(&mut layout.common(), value);
			if key == "resolution" {
				slider.set_enabled(&mut layout.common(), !game_running);
			}
			let tasks = self.tasks.clone();
			let cookie = cookie.clone();
			slider.on_value_changed(Box::new(move |_common, e| tasks.push(Task::Setting(cookie.clone(), key, e.value))));
		}
		Ok(())
	}

	/// The network links card at the top of the stream settings
	fn update_stream_links(&self, layout: &mut Layout, stream: &Value) -> anyhow::Result<()> {
		let Some(settings) = &self.settings else {
			return Ok(());
		};
		let cell = &settings.data;
		let set_label = |layout: &mut Layout, id: &str, text: String| -> anyhow::Result<()> {
			let mut common = layout.common();
			let widget = cell.get_widget_id(id)?;
			common.alterables.set_widget_visible(widget, !text.is_empty());
			let mut label = common.state.widgets.cast_as::<WidgetLabel>(widget)?;
			label.set_text(&mut common, Translation::from_raw_text_string(text));
			Ok(())
		};
		let multilink = links_view(stream.get("multilink").unwrap_or(&Value::Null));
		layout
			.alterables
			.set_widget_visible(cell.get_widget_id("links_card")?, multilink.is_some());
		let Some(multilink) = multilink else {
			return Ok(());
		};
		for (mode, ..) in LINK_MODES {
			cell
				.fetch_component_as::<ComponentButton>(&format!("link_mode_{mode}"))?
				.set_sticky_state_immediate(&mut layout.common(), mode == multilink.mode);
		}
		set_label(layout, "link_mode_help", multilink.help.to_owned())?;
		for (index, row) in (1..=3).zip(multilink.rows.iter().map(Some).chain(std::iter::repeat(None))) {
			layout
				.alterables
				.set_widget_visible(cell.get_widget_id(&format!("link_{index}"))?, row.is_some());
			let (name, state, metrics) = row.map_or_else(Default::default, |row| {
				(row.name.clone(), row.state.clone(), row.metrics.clone())
			});
			set_label(layout, &format!("link_{index}_name"), name)?;
			set_label(layout, &format!("link_{index}_state"), state)?;
			set_label(layout, &format!("link_{index}_metrics"), metrics)?;
		}
		set_label(layout, "link_more", multilink.more)?;
		Ok(())
	}

	fn update_stream_debug(&self, layout: &mut Layout, stream: &Value) -> anyhow::Result<()> {
		let Some(settings) = &self.settings else {
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
					if eyes.get("calibrated").and_then(Value::as_bool).unwrap_or(false) {
						""
					} else {
						", calibrating"
					},
					if eyes.get("gaze").and_then(Value::as_bool).unwrap_or(false) {
						""
					} else {
						", no gaze"
					}
				)
			})
			.unwrap_or_default();
		for (id, text) in [
			("video_debug", debug),
			("network_status", network),
			("vr_debug", vr),
			("eyes_debug", eyes),
		] {
			let mut common = layout.common();
			let mut label = settings.data.fetch_widget_as::<WidgetLabel>(common.state, id)?;
			label.set_text(&mut common, Translation::from_raw_text_string(text));
		}
		Ok(())
	}
}

/// A link's jitter or loss above these makes it unsteady for streaming
const UNSTEADY_JITTER_MS: f64 = 5.0;
const UNSTEADY_LOSS_PERCENT: f64 = 1.0;

/// Link modes: the client's name, and what the mode does
const LINK_MODES: [(&str, &str); 3] = [
	(
		"dongle",
		"Streams over the dongle while it is connected, and over every link when it is not.",
	),
	(
		"all",
		"Streams over every working link at once. A slow link can delay frames.",
	),
	(
		"fastest",
		"Streams over the one link that was fastest when you chose this. It does not switch if that link gets worse.",
	),
];

struct LinkRow {
	name: String,
	state: String,
	metrics: String,
}

struct Links {
	mode: &'static str,
	help: &'static str,
	/// The first three links
	rows: Vec<LinkRow>,
	/// How many more there are, when there are more than three
	more: String,
}

/// The link mode and the connection's links; none when the client reports no connection
fn links_view(multilink: &Value) -> Option<Links> {
	if !multilink.is_object() {
		return None;
	}
	let links = multilink
		.get("links")
		.and_then(Value::as_array)
		.map_or(&[][..], Vec::as_slice);
	let number = |link: &Value, key: &str| link.get(key).and_then(Value::as_f64).unwrap_or(0.0);
	let mode = multilink.get("mode").and_then(Value::as_str).unwrap_or("dongle");
	let (mode, help) = LINK_MODES
		.iter()
		.find(|(name, _)| *name == mode)
		.unwrap_or(&LINK_MODES[0]);
	Some(Links {
		mode,
		help,
		rows: links
			.iter()
			.take(3)
			.map(|link| {
				let unsteady =
					number(link, "jitter_ms") >= UNSTEADY_JITTER_MS || number(link, "loss_percent") >= UNSTEADY_LOSS_PERCENT;
				let active = link.get("active").and_then(Value::as_bool).unwrap_or(false);
				LinkRow {
					name: link
						.get("name")
						.and_then(Value::as_str)
						.unwrap_or("Unknown link")
						.to_owned(),
					state: format!(
						"{}{}",
						if active { "in use" } else { "standby" },
						if unsteady { " · unsteady" } else { "" }
					),
					metrics: format!(
						"{:.0} ms delay · {:.1} ms jitter · {:.1}% loss · {:.0} Mbit/s",
						number(link, "rtt_ms"),
						number(link, "jitter_ms"),
						number(link, "loss_percent"),
						number(link, "rx_mbit"),
					),
				}
			})
			.collect(),
		more: match links.len().saturating_sub(3) {
			0 => String::new(),
			more => format!("and {more} more"),
		},
	})
}

#[cfg(test)]
mod link_tests {
	use super::*;

	#[test]
	fn links_show_each_connection_its_jitter_and_the_mode() {
		assert!(links_view(&Value::Null).is_none());
		let links = links_view(&serde_json::json!({"mode": "all", "links": [
			{"name": "wlan0 (Wi-Fi)", "rtt_ms": 6.2, "jitter_ms": 9.8, "loss_percent": 0.24, "rx_mbit": 85.4, "active": true},
			{"name": "wlanap (Dongle)", "rtt_ms": 2.0, "jitter_ms": 0.6, "loss_percent": 0.0, "rx_mbit": 0.0, "active": false},
			{"name": "a"}, {"name": "b"}, {"name": "c"}
		]}))
		.unwrap();
		assert_eq!(links.mode, "all");
		assert_eq!(links.help, LINK_MODES[1].1);
		assert_eq!(links.rows.len(), 3);
		assert_eq!(links.rows[0].name, "wlan0 (Wi-Fi)");
		assert_eq!(links.rows[0].state, "in use · unsteady");
		assert_eq!(
			links.rows[0].metrics,
			"6 ms delay · 9.8 ms jitter · 0.2% loss · 85 Mbit/s"
		);
		assert_eq!(links.rows[1].state, "standby");
		assert_eq!(links.more, "and 2 more");
		let default = links_view(&serde_json::json!({"links": []})).unwrap();
		assert_eq!(default.mode, "dongle");
	}
}
