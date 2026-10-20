use std::{marker::PhantomData, rc::Rc};

use wgui::{
	assets::AssetPathRef,
	components::button::ComponentButton,
	globals::WguiGlobals,
	layout::WidgetID,
	parser::{Fetchable, ParseDocumentParams, ParserData, ParserState, TemplateParams},
	task::Tasks,
};

use crate::{
	frontend::{Frontend, FrontendTask},
	tab::{Tab, TabType, wivrn::WivrnState},
	util::steam_utils::{AppManifest, SteamUtils},
	views::{ViewTrait, ViewUpdateParams, game_list, running_games_list},
};

#[derive(Clone)]
enum Task {
	/// None: this headset, else a PC's cookie
	Select(Option<Rc<str>>),
	Connect(Rc<str>),
	RemoteCommand(Rc<str>, String),
}

pub struct TabGames<T> {
	#[allow(dead_code)]
	pub state: ParserState,
	tasks: Tasks<Task>,

	view_game_list: Option<game_list::View>,
	view_running_games_list: Option<running_games_list::View>,
	marker: PhantomData<T>,

	id_hosts: WidgetID,
	id_game_list: WidgetID,
	id_running: WidgetID,
	/// Template instances (their components) of the host bar and of the list area
	host_cells: Vec<ParserData>,
	list_cells: Vec<ParserData>,
	running_cells: Vec<ParserData>,
	running_shown: Option<String>,

	/// Selected PC (cookie), None for this headset
	selected: Option<Rc<str>>,
	wivrn_version: Option<u64>,
	/// What the list currently shows, to rebuild it only when that changes
	shown: Option<String>,
}

fn doc_params(globals: &WguiGlobals) -> ParseDocumentParams<'_> {
	ParseDocumentParams {
		globals: globals.clone(),
		path: AssetPathRef::BuiltIn("gui/tab/games.xml"),
		extra: Default::default(),
	}
}

impl<T> Tab<T> for TabGames<T> {
	fn get_type(&self) -> TabType {
		TabType::Games
	}

	fn update(&mut self, frontend: &mut Frontend<T>, time_ms: u32, data: &mut T) -> anyhow::Result<()> {
		let mut force = false;
		for task in self.tasks.drain() {
			match task {
				Task::Select(selected) => {
					self.selected = selected;
					force = true;
				}
				Task::RemoteCommand(cookie, command) => {
					if let Some((_, lobby, stream)) = frontend.interface.wivrn_state(data) {
						let state = WivrnState::parse(&lobby, &stream);
						if state
							.hosts
							.iter()
							.any(|host| host.cookie == cookie && state.connected_to(host))
						{
							frontend.tasks.push(FrontendTask::WivrnCommand(command.into()));
						}
					}
				}
				Task::Connect(cookie) => frontend
					.tasks
					.push(FrontendTask::WivrnCommand(format!("connect {cookie}").into())),
			}
		}

		match frontend.interface.wivrn_state(data) {
			Some((version, lobby, stream)) if force || self.wivrn_version != Some(version) => {
				self.wivrn_version = Some(version);
				self.refresh(frontend, &WivrnState::parse(&lobby, &stream), force)?;
			}
			None if self.shown.is_none() => self.show_local(frontend)?,
			_ => {}
		}

		let mut config_change_kind = None;
		if let Some(view) = &mut self.view_game_list {
			view.update(&mut ViewUpdateParams {
				layout: &mut frontend.layout,
				executor: &frontend.executor,
				general_config: frontend.interface.general_config(data),
				config_change_kind: &mut config_change_kind,
			})?;
		}
		if let Some(kind) = config_change_kind {
			frontend.interface.config_changed(data, kind);
		}

		if let Some(view) = &mut self.view_running_games_list {
			view.update(&mut frontend.layout, time_ms)?;
		}
		Ok(())
	}
}

impl<T> TabGames<T> {
	pub fn new(frontend: &mut Frontend<T>, parent_id: WidgetID) -> anyhow::Result<Self> {
		let globals = frontend.layout.state.globals.clone();
		let state = wgui::parser::parse_from_assets(&doc_params(&globals), &mut frontend.layout, parent_id)?;

		Ok(Self {
			id_hosts: state.get_widget_id("hosts_root")?,
			id_game_list: state.get_widget_id("game_list_parent")?,
			id_running: state.get_widget_id("running_games_list_parent")?,
			state,
			tasks: Tasks::new(),
			view_game_list: None,
			view_running_games_list: None,
			marker: PhantomData,
			host_cells: Vec::new(),
			list_cells: Vec::new(),
			running_cells: Vec::new(),
			running_shown: None,
			selected: None,
			wivrn_version: None,
			shown: None,
		})
	}

	fn clear_lists(&mut self, frontend: &mut Frontend<T>) {
		self.view_game_list = None;
		self.view_running_games_list = None;
		self.list_cells.clear();
		self.running_cells.clear();
		self.running_shown = None;
		frontend.layout.remove_children(self.id_game_list);
		frontend.layout.remove_children(self.id_running);
	}

	/// The local Steam library
	fn show_local(&mut self, frontend: &mut Frontend<T>) -> anyhow::Result<()> {
		self.clear_lists(frontend);
		self.shown = Some("local".into());
		let globals = frontend.layout.state.globals.clone();
		let mut steam_utils = SteamUtils::new()?;
		self.view_game_list = Some(game_list::View::new(game_list::Params {
			executor: frontend.executor.clone(),
			frontend_tasks: frontend.tasks.clone(),
			globals: globals.clone(),
			layout: &mut frontend.layout,
			parent_id: self.id_game_list,
			steam_utils: &steam_utils,
			remote: None,
		})?);
		self.view_running_games_list = Some(running_games_list::View::new(running_games_list::Params {
			globals,
			layout: &mut frontend.layout,
			parent_id: self.id_running,
			steam_utils: &mut steam_utils,
			frontend_tasks: frontend.tasks.clone(),
		})?);
		Ok(())
	}

	fn template(
		&mut self,
		frontend: &mut Frontend<T>,
		name: &str,
		parent: WidgetID,
		params: TemplateParams,
		host_bar: bool,
	) -> anyhow::Result<Rc<ComponentButton>> {
		let globals = frontend.layout.state.globals.clone();
		let data = self
			.state
			.parse_template_only(&doc_params(&globals), name, &mut frontend.layout, parent, params)?;
		let button = data
			.fetch_component_as::<ComponentButton>("btn")
			.or_else(|_| data.fetch_component_as::<ComponentButton>("btn_connect"))?;
		if host_bar {
			self.host_cells.push(data);
		} else {
			self.list_cells.push(data);
		}
		Ok(button)
	}

	fn refresh(&mut self, frontend: &mut Frontend<T>, wivrn: &WivrnState, force: bool) -> anyhow::Result<()> {
		// The host bar: this headset and every known PC
		frontend.layout.remove_children(self.id_hosts);
		self.host_cells.clear();
		if self
			.selected
			.as_ref()
			.is_some_and(|cookie| !wivrn.hosts.iter().any(|host| host.cookie == *cookie))
		{
			self.selected = None;
		}
		let mut entries: Vec<(Option<Rc<str>>, Rc<str>, &str)> =
			vec![(None, "This headset".into(), "@/dashboard/games.svg")];
		for host in &wivrn.hosts {
			entries.push((Some(host.cookie.clone()), host.name.clone(), "@/dashboard/display.svg"));
		}
		for (cookie, name, icon) in entries {
			let mut params = TemplateParams::new();
			params.insert("name", &name);
			params.insert("icon", icon);
			let button = self.template(frontend, "HostButton", self.id_hosts, params, true)?;
			button.set_sticky_state(&mut frontend.layout.common(), cookie == self.selected);
			button.on_click(self.tasks.get_button_click_callback(Task::Select(cookie)));
		}

		// The list: local games, a PC's applications, or how to get them
		let Some(host) = self
			.selected
			.as_ref()
			.and_then(|cookie| wivrn.hosts.iter().find(|host| host.cookie == *cookie))
		else {
			if force || self.shown.as_deref() != Some("local") {
				self.show_local(frontend)?;
			}
			return Ok(());
		};

		let apps: Vec<AppManifest> = if wivrn.connected_to(host) {
			wivrn
				.stream
				.get("apps")
				.and_then(serde_json::Value::as_array)
				.into_iter()
				.flatten()
				.map(|app| {
					let id = app
						.get("id")
						.and_then(serde_json::Value::as_str)
						.unwrap_or_default()
						.to_owned();
					AppManifest {
						app_id: id.clone(),
						run_game_id: id,
						name: app
							.get("name")
							.and_then(serde_json::Value::as_str)
							.unwrap_or_default()
							.to_owned(),
						raw_state_flags: 0,
						last_played: None,
						icon_path: app
							.get("icon")
							.and_then(serde_json::Value::as_str)
							.filter(|path| !path.is_empty())
							.map(Into::into),
					}
				})
				.collect()
		} else {
			Vec::new()
		};
		let shown = format!(
			"{}:{}:{}",
			host.cookie,
			wivrn.connected_to(host),
			apps
				.iter()
				.map(|app| format!("{}:{:?}", app.app_id, app.icon_path))
				.collect::<Vec<_>>()
				.join(",")
		);
		if !force && self.shown.as_ref() == Some(&shown) {
			self.refresh_running(frontend, wivrn, &host.cookie)?;
			return Ok(());
		}
		self.clear_lists(frontend);
		self.shown = Some(shown);

		if !wivrn.connected_to(host) {
			let (text, connect) = if wivrn.is_active(host) {
				(format!("Connecting to {}…", host.name), false)
			} else if !host.visible {
				(format!("{} is not on the network", host.name), false)
			} else {
				(format!("Not connected to {}", host.name), true)
			};
			let mut params = TemplateParams::new();
			params.insert("text", &text);
			params.insert("show_connect", if connect { "flex" } else { "none" });
			let button = self.template(frontend, "NotConnected", self.id_game_list, params, false)?;
			button.on_click(self.tasks.get_button_click_callback(Task::Connect(host.cookie.clone())));
			return Ok(());
		}

		let globals = frontend.layout.state.globals.clone();
		let steam_utils = SteamUtils::new()?;
		self.view_game_list = Some(game_list::View::new(game_list::Params {
			executor: frontend.executor.clone(),
			frontend_tasks: frontend.tasks.clone(),
			globals,
			layout: &mut frontend.layout,
			parent_id: self.id_game_list,
			steam_utils: &steam_utils,
			remote: Some((host.name.clone(), apps)),
		})?);
		self.refresh_running(frontend, wivrn, &host.cookie)?;
		Ok(())
	}
	fn refresh_running(
		&mut self,
		frontend: &mut Frontend<T>,
		state: &WivrnState,
		cookie: &Rc<str>,
	) -> anyhow::Result<()> {
		let steam = state
			.stream
			.get("steam_running")
			.cloned()
			.unwrap_or_else(|| serde_json::json!([]));
		let sessions = state
			.stream
			.get("running")
			.cloned()
			.unwrap_or_else(|| serde_json::json!([]));
		let key = format!("{cookie}:{steam}:{sessions}");
		if self.running_shown.as_ref() == Some(&key) {
			return Ok(());
		}
		self.running_shown = Some(key);
		frontend.layout.remove_children(self.id_running);
		self.running_cells.clear();
		let globals = frontend.layout.state.globals.clone();
		let mut rows = Vec::new();
		for id in steam
			.as_array()
			.into_iter()
			.flatten()
			.filter_map(serde_json::Value::as_str)
		{
			let name = state
				.stream
				.get("apps")
				.and_then(serde_json::Value::as_array)
				.and_then(|apps| {
					apps
						.iter()
						.find(|app| app.get("id").and_then(serde_json::Value::as_str) == Some(id))
				})
				.and_then(|app| app.get("name").and_then(serde_json::Value::as_str))
				.unwrap_or(id);
			rows.push((
				"RunningSteamGame",
				name.to_owned(),
				format!(
					"host {}",
					serde_json::json!({"stop_game": {"app_id": id, "force": false}})
				),
				Some(format!(
					"host {}",
					serde_json::json!({"stop_game": {"app_id": id, "force": true}})
				)),
			));
		}
		for session in sessions.as_array().into_iter().flatten() {
			let Some(id) = session.get("id").and_then(serde_json::Value::as_u64) else {
				continue;
			};
			let name = session
				.get("name")
				.and_then(serde_json::Value::as_str)
				.unwrap_or("VR application");
			let suffix = if session.get("overlay").and_then(serde_json::Value::as_bool) == Some(true) {
				"overlay"
			} else if session.get("active").and_then(serde_json::Value::as_bool) == Some(true) {
				"active"
			} else {
				"background"
			};
			rows.push((
				"RunningVrSession",
				format!("{name} ({suffix})"),
				format!("stop_app {id}"),
				Some(format!("set_active_app {id}")),
			));
		}
		if rows.is_empty() {
			return Ok(());
		}
		let heading = self.state.parse_template_only(
			&doc_params(&globals),
			"RunningHeading",
			&mut frontend.layout,
			self.id_running,
			TemplateParams::new(),
		)?;
		self.running_cells.push(heading);
		for (template, name, stop, other) in rows {
			let mut params = TemplateParams::new();
			params.insert("name", &name);
			let row = self.state.parse_template_only(
				&doc_params(&globals),
				template,
				&mut frontend.layout,
				self.id_running,
				params,
			)?;
			row.fetch_component_as::<ComponentButton>("btn_stop")?.on_click(
				self
					.tasks
					.get_button_click_callback(Task::RemoteCommand(cookie.clone(), stop)),
			);
			if let Some(command) = other {
				row.fetch_component_as::<ComponentButton>("btn_other")?.on_click(
					self
						.tasks
						.get_button_click_callback(Task::RemoteCommand(cookie.clone(), command)),
				);
			}
			self.running_cells.push(row);
		}
		Ok(())
	}
}
