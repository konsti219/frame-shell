use std::{cell::Cell, path::PathBuf, process::Command, rc::Rc};

use chrono::Timelike;
use glam::Vec2;
use strum::EnumCount;
use wgui::{
	assets::{AssetPathRef, AssetProvider},
	components::{button::ComponentButton, checkbox::ComponentCheckbox, slider::ComponentSlider},
	event::{CallbackDataCommon, StyleSetRequest},
	font_config::WguiFontConfig,
	globals::WguiGlobals,
	i18n::Translation,
	layout::{Layout, LayoutParams, LayoutTask, LayoutUpdateParams, LayoutUpdateResult, WidgetID},
	parser::{Fetchable, ParseDocumentParams, ParserState},
	renderer_vk::text::custom_glyph::CustomGlyphData,
	taffy::{self},
	task::Tasks,
	theme::WguiTheme,
	widget::{label::WidgetLabel, rectangle::WidgetRectangle, sprite::WidgetSprite},
	windowing::{
		window::{WguiWindow, WguiWindowParams, WguiWindowParamsExtra, WguiWindowPlacement},
	},
};
use wlx_common::{
	async_executor::AsyncExecutor,
	frame_power,
	audio,
	dash_interface::{AudioControl, BoxDashInterface, ConfigChangeKind, DashPlayspaceTask, ShellRequest},
	locale::WayVRLangProvider,
	palette::load_palette,
	timestep::{self, Timestep},
};

use crate::{
	assets,
	tab::{
		Tab, TabType, apps::TabApps, donate::TabDonate, games::TabGames, home::TabHome, monado::TabMonado,
		settings::TabSettings, welcome::TabWelcome, wivrn::TabWivrn,
	},
	util::{
		popup_manager::{MountPopupOnceParams, PopupHolder, PopupManager, PopupManagerParams},
		toast_manager::ToastManager,
	},
	views::{self, ViewUpdateParams},
};

pub struct FrontendWidgets {
	id_label_time: WidgetID,
	id_rect_content: WidgetID,
	id_sprite_titlebar_icon: WidgetID,
	id_label_titlebar_title: WidgetID,
}

pub type FrontendTasks = Tasks<FrontendTask>;

pub struct Frontend<T> {
	pub layout: Layout,
	pub globals: WguiGlobals,

	pub interface: BoxDashInterface<T>,

	// single-threaded async runtime executor
	pub executor: AsyncExecutor,

	#[allow(dead_code)]
	state: ParserState,

	current_tab: Option<Box<dyn Tab<T>>>,
	side_buttons: Vec<(TabType, Rc<ComponentButton>)>,

	pub tasks: FrontendTasks,

	ticks: u32,

	widgets: FrontendWidgets,
	popup_manager: PopupManager,
	toast_manager: ToastManager,
	timestep: Timestep,
	sounds_to_play: Vec<SoundType>,
	popup_dialog_box: PopupHolder<views::dialog_box::View>,

	window_audio_settings: WguiWindow,
	menu_audio: Option<ParserState>,
	audio_syncing: Rc<Cell<bool>>,

	window_power: WguiWindow,
	menu_power: Option<ParserState>,
	window_playspace: WguiWindow,
	menu_playspace: Option<ParserState>,
	window_battery: WguiWindow,
	menu_battery: Option<ParserState>,
	battery_updated: Option<std::time::Instant>,
}

pub struct FrontendUpdateParams<'a, T> {
	pub data: &'a mut T,
	pub width: f32,
	pub height: f32,
	pub timestep_alpha: f32,
}

pub struct FrontendUpdateResult {
	pub layout_result: LayoutUpdateResult,
	pub sounds_to_play: Vec<SoundType>,
}

pub struct InitParams<'a, T> {
	pub interface: BoxDashInterface<T>,
	pub lang_provider: &'a WayVRLangProvider,
	pub show_welcome: bool,
	pub has_monado: bool,
	pub theme: Rc<WguiTheme>,
	pub color_palette: &'a str,
	pub executor: AsyncExecutor,
}

#[derive(Clone)]
pub enum SoundType {
	Startup,
	Launch,
	Save,
}

#[derive(Clone)]
pub enum FrontendTask {
	SetTab(TabType),
	RefreshClock,
	RefreshBackground,
	MountPopupOnce(MountPopupOnceParams),
	RefreshPopupManager,
	ShowAudioSettings,
	AudioControl(AudioControl),
	RecenterPlayspace,
	PushToast(Translation),
	PlaySound(SoundType),
	OpenURL(Rc<str>),
	RunCommand(String /* program */, Vec<String> /* args */),
	HideDashboard,
	MarkTutorialGraduated,
	WivrnCommand(Rc<str>),
	WivrnLaunch {
		host: Rc<str>,
		app_id: Rc<str>,
		name: Rc<str>,
	},
	ShowPowerMenu,
	ShowPlayspaceMenu,
	ShowBatteryMenu,
	SetPerformanceMode(frame_power::PerformanceMode),
	DetectFloor,
	DebugFloor(bool),
	PowerAction(&'static str),
	BackendTask(wlx_common::dash_interface::DashBackendTask),
}

impl<T: 'static> Frontend<T> {
	pub fn new(params: InitParams<T>) -> anyhow::Result<Frontend<T>> {
		let mut assets = Box::new(assets::Asset {});

		let font_binary_bold = assets.load_from_path_gzip("Quicksand-Bold.ttf.gz")?;
		let font_binary_regular = assets.load_from_path_gzip("Quicksand-Regular.ttf.gz")?;
		let font_binary_light = assets.load_from_path_gzip("Quicksand-Light.ttf.gz")?;

		let palette = load_palette(params.color_palette);

		let globals = WguiGlobals::new(
			assets,
			params.lang_provider,
			&WguiFontConfig {
				binaries: vec![&font_binary_regular, &font_binary_bold, &font_binary_light],
				family_name_sans_serif: "Quicksand",
				family_name_serif: "Quicksand",
				family_name_monospace: "",
			},
			PathBuf::new(), //FIXME: pass from somewhere else
			palette,
		)?;

		let (layout, state) = wgui::parser::new_layout_from_assets(
			&ParseDocumentParams {
				globals: globals.clone(),
				path: AssetPathRef::BuiltIn("gui/dashboard.xml"),
				extra: Default::default(),
			},
			LayoutParams {
				resize_to_parent: true,
				theme: params.theme,
			},
		)?;

		let id_popup_manager = state.get_widget_id("popup_manager")?;
		let popup_manager = PopupManager::new(PopupManagerParams {
			parent_id: id_popup_manager,
		});

		let toast_manager = ToastManager::new();

		let tasks = FrontendTasks::new();

		let init_tab = if params.show_welcome {
			TabType::Welcome
		} else {
			TabType::Home
		};
		tasks.push(FrontendTask::SetTab(init_tab));

		let id_label_time = state.get_widget_id("label_time")?;
		let id_rect_content = state.get_widget_id("rect_content")?;
		let id_sprite_titlebar_icon = state.get_widget_id("sprite_titlebar_icon")?;
		let id_label_titlebar_title = state.get_widget_id("label_titlebar_title")?;

		let timestep = Timestep::new(60.0);

		let mut frontend = Self {
			layout,
			state,
			current_tab: None,
			side_buttons: Vec::with_capacity(TabType::COUNT),
			globals,
			tasks,
			ticks: 0,
			widgets: FrontendWidgets {
				id_label_time,
				id_rect_content,
				id_sprite_titlebar_icon,
				id_label_titlebar_title,
			},
			timestep,
			interface: params.interface,
			popup_manager,
			toast_manager,
			window_audio_settings: WguiWindow::default(),
			menu_audio: None,
			audio_syncing: Rc::new(Cell::new(false)),
			window_power: WguiWindow::default(),
			menu_power: None,
			window_playspace: WguiWindow::default(),
			menu_playspace: None,
			window_battery: WguiWindow::default(),
			menu_battery: None,
			battery_updated: None,
			executor: params.executor,
			sounds_to_play: Vec::new(),
			popup_dialog_box: Default::default(),
		};

		// init some things first
		frontend.tasks.push(FrontendTask::RefreshBackground);
		frontend.tasks.push(FrontendTask::RefreshClock);

		Frontend::register_widgets(&mut frontend)?;

		Ok(frontend)
	}

	fn queue_play_sound(&mut self, sound_type: SoundType) {
		self.sounds_to_play.push(sound_type);
	}

	fn play_sound(&mut self, audio: &mut Box<dyn audio::AudioProvider>, sound_type: SoundType) -> anyhow::Result<()> {
		let mut assets = self.globals.assets_builtin();

		let path = match sound_type {
			SoundType::Startup => "sound/startup.mp3",
			SoundType::Launch => "sound/app_start.mp3",
			SoundType::Save => "sound/save.mp3",
		};

		// try loading a custom sound; if one doesn't exist (or it failed to load), use the built-in asset
		let sound_bytes = match audio::AudioSample::try_bytes_from_config(path) {
			Ok(bytes) => bytes,
			Err(_) => assets.load_from_path(path)?.into(),
		};

		let sample = audio::AudioSample::from_mp3(&sound_bytes)?;
		audio.play_raw_sample(&sample);
		Ok(())
	}

	pub fn update(&mut self, mut params: FrontendUpdateParams<T>) -> anyhow::Result<FrontendUpdateResult> {
		match self.interface.shell_request(params.data) {
			Some(ShellRequest::PowerMenu) => self.tasks.push(FrontendTask::ShowPowerMenu),
			Some(ShellRequest::Pairing) => self.tasks.push(FrontendTask::SetTab(TabType::Wivrn)),
			None => {}
		}

		let mut tasks = self.tasks.drain();

		while let Some(task) = tasks.pop_front() {
			self.process_task(&mut params, task)?;
		}

		let time_ms = timestep::get_micros() / 1000;

		if let Some(mut tab) = self.current_tab.take() {
			tab.update(self, time_ms as u32, params.data)?;

			self.current_tab = Some(tab);
		}

		self.update_playspace_menu(params.data)?;
		self.update_battery_menu()?;
		self.update_audio_menu(params.data)?;

		let res = self.tick(params)?;
		self.ticks += 1;

		Ok(res)
	}

	pub fn process_update(
		&mut self,
		res: FrontendUpdateResult,
		audio: &mut Box<dyn audio::AudioProvider>,
	) -> anyhow::Result<()> {
		for sound_type in res.sounds_to_play {
			self.play_sound(audio, sound_type)?;
		}

		audio.play_wgui_samples(res.layout_result.sounds_to_play);

		Ok(())
	}

	fn tick(&mut self, params: FrontendUpdateParams<T>) -> anyhow::Result<FrontendUpdateResult> {
		// fixme: timer events instead of this thing
		if self.ticks.is_multiple_of(1000) {
			self.update_time(params.data)?;
		}

		// TODO: refactor: simplify update function (no configs or T are required in most cases)
		let mut config_change_kind = None;
		self.popup_dialog_box.update(&mut ViewUpdateParams {
			layout: &mut self.layout,
			executor: &self.executor,
			general_config: self.interface.general_config(params.data),
			config_change_kind: &mut config_change_kind,
		})?;

		{
			// always 30 times per second
			while self.timestep.on_tick() {
				self.toast_manager.tick(&mut self.layout)?;
			}
		}

		let layout_result = self.layout.update(&mut LayoutUpdateParams {
			size: Vec2::new(params.width, params.height),
			timestep_alpha: params.timestep_alpha,
		})?;

		Ok(FrontendUpdateResult {
			layout_result,
			sounds_to_play: std::mem::take(&mut self.sounds_to_play),
		})
	}

	fn update_time(&mut self, data: &mut T) -> anyhow::Result<()> {
		{
			let mut common = self.layout.common();
			let mut label = common
				.state
				.widgets
				.cast_as::<WidgetLabel>(self.widgets.id_label_time)?;

			let now = chrono::Local::now();
			let hours = now.hour();
			let minutes = now.minute();

			let text: String = if self.interface.general_config(data).clock_12h {
				let hours_ampm = (hours + 11) % 12 + 1;
				let suffix = if hours >= 12 { "PM" } else { "AM" };
				format!("{hours_ampm:02}:{minutes:02} {suffix}")
			} else {
				format!("{hours:02}:{minutes:02}")
			};

			label.set_text(&mut common, Translation::from_raw_text(&text));
		}

		Ok(())
	}

	fn mount_popup_once(&mut self, params: MountPopupOnceParams, data: &mut T) -> anyhow::Result<()> {
		let config = self.interface.general_config(data);

		self
			.popup_manager
			.mount_popup_once(&self.globals, &mut self.layout, &self.tasks, params, config)?;
		Ok(())
	}

	fn refresh_popup_manager(&mut self) -> anyhow::Result<()> {
		self.popup_manager.refresh(&mut self.layout.alterables);
		Ok(())
	}

	fn update_background(&mut self, data: &mut T) -> anyhow::Result<()> {
		self.layout.mark_redraw();

		let Some(mut rect) = self
			.layout
			.state
			.widgets
			.get_as::<WidgetRectangle>(self.widgets.id_rect_content)
		else {
			anyhow::bail!("");
		};

		let (alpha1, alpha2) = if self.interface.general_config(data).opaque_background {
			(1.0, 1.0)
		} else {
			(0.8666, 0.9333)
		};

		rect.params.color = rect.params.color.with_alpha(alpha1);
		rect.params.color2 = rect.params.color2.with_alpha(alpha2);

		Ok(())
	}

	fn process_task(&mut self, params: &mut FrontendUpdateParams<T>, task: FrontendTask) -> anyhow::Result<()> {
		match task {
			FrontendTask::SetTab(tab_type) => self.set_tab(params.data, tab_type)?,
			FrontendTask::RefreshClock => self.update_time(params.data)?,
			FrontendTask::RefreshBackground => self.update_background(params.data)?,
			FrontendTask::MountPopupOnce(popup_params) => self.mount_popup_once(popup_params, params.data)?,
			FrontendTask::RefreshPopupManager => self.refresh_popup_manager()?,
			FrontendTask::ShowAudioSettings => self.action_show_audio_settings()?,
			FrontendTask::AudioControl(control) => self.interface.headset_audio_control(params.data, control),
			FrontendTask::RecenterPlayspace => self.action_recenter_playspace(params.data)?,
			FrontendTask::PushToast(content) => self.toast_manager.push(content),
			FrontendTask::PlaySound(sound_type) => self.queue_play_sound(sound_type),
			FrontendTask::HideDashboard => self.action_hide_dashboard(params.data),
			FrontendTask::OpenURL(url) => self.action_open_url(url),
			FrontendTask::RunCommand(program, args) => self.action_run_command(program, args),
			FrontendTask::MarkTutorialGraduated => self.action_tutorial_graduated(params.data),
			FrontendTask::WivrnCommand(command) => self.interface.wivrn_command(params.data, &command),
			FrontendTask::WivrnLaunch { host, app_id, name } => {
				self.interface.wivrn_launch(params.data, &host, &app_id, &name)
			}
			FrontendTask::ShowPowerMenu => self.action_show_power_menu()?,
			FrontendTask::ShowBatteryMenu => self.action_show_battery_menu()?,
			FrontendTask::SetPerformanceMode(mode) => {
				if let Err(error) = frame_power::set_performance_mode(mode) {
					log::warn!("Could not set the performance mode: {error:?}");
				}
				self.battery_updated = None;
			}
			FrontendTask::ShowPlayspaceMenu => self.action_show_playspace_menu(params.data)?,
			FrontendTask::DetectFloor => self.interface.floor_detect(params.data),
			FrontendTask::DebugFloor(visible) => self.interface.floor_debug_set(params.data, visible),
			FrontendTask::PowerAction(action) => {
				self.window_power.close();
				self.interface.power_action(params.data, action);
			}
			FrontendTask::BackendTask(task) => self.interface.backend_task(params.data, task)?,
		};
		Ok(())
	}

	fn set_tab_title(&mut self, translation: &str, icon: &str) -> anyhow::Result<()> {
		let mut common = self.layout.common();

		{
			let mut label = common
				.state
				.widgets
				.cast_as::<WidgetLabel>(self.widgets.id_label_titlebar_title)?;
			label.set_text(&mut common, Translation::from_translation_key(translation));
		}

		{
			let mut sprite = common
				.state
				.widgets
				.cast_as::<WidgetSprite>(self.widgets.id_sprite_titlebar_icon)?;
			sprite.set_content(
				common.alterables,
				Some(CustomGlyphData::from_assets(
					&self.globals,
					AssetPathRef::BuiltIn(icon),
				)?),
			);
		}

		Ok(())
	}

	fn set_tab(&mut self, data: &mut T, tab_type: TabType) -> anyhow::Result<()> {
		log::info!("Setting tab to {tab_type:?}");
		let widget_content = self.state.fetch_widget(&self.layout.state, "content")?;
		self.layout.remove_children(widget_content.id);

		let padding = tab_type.get_preferred_padding();
		self.layout.tasks.push(LayoutTask::SetWidgetStyle(
			widget_content.id,
			StyleSetRequest::Padding(taffy::Rect::length(padding)),
		));

		let (tab_translation, icon_path) = match tab_type {
			TabType::Welcome => ("GETTING_STARTED", "dashboard/welcome.svg"),
			TabType::Home => ("HOME_SCREEN", "dashboard/home.svg"),
			TabType::Apps => ("APPLICATIONS", "dashboard/apps.svg"),
			TabType::Games => ("GAMES", "dashboard/games.svg"),
			TabType::Monado => ("MONADO_RUNTIME", "dashboard/monado.svg"),
			TabType::Wivrn => ("WIVRN", "dashboard/wivrn_head_symbolic.svg"),
			TabType::Settings => ("SETTINGS", "dashboard/settings.svg"),
			TabType::Donate => ("DONATE.SUPPORT_US", "dashboard/opencollective.svg"),
		};

		self.set_tab_title(tab_translation, icon_path)?;

		let tab: Box<dyn Tab<T>> = match tab_type {
			TabType::Welcome => Box::new(TabWelcome::new(self, widget_content.id, data)?),
			TabType::Home => Box::new(TabHome::new(self, widget_content.id, data)?),
			TabType::Apps => Box::new(TabApps::new(self, widget_content.id, data)?),
			TabType::Games => Box::new(TabGames::new(self, widget_content.id)?),
			TabType::Monado => Box::new(TabMonado::new(self, widget_content.id)?),
			TabType::Wivrn => Box::new(TabWivrn::new(self, widget_content.id)?),
			TabType::Settings => Box::new(TabSettings::new(self, widget_content.id, data)?),
			TabType::Donate => Box::new(TabDonate::new(self, widget_content.id, data)?),
		};

		self.current_tab = Some(tab);

		let mut common = CallbackDataCommon {
			state: &self.layout.state,
			alterables: &mut self.layout.alterables,
		};

		for (t, btn) in self.side_buttons.iter() {
			btn.set_sticky_state(&mut common, tab_type == *t);
		}

		Ok(())
	}

	fn register_widgets(&mut self) -> anyhow::Result<()> {
		// "X" button
		self.tasks.handle_button(
			&self.state.fetch_component_as::<ComponentButton>("btn_close")?,
			FrontendTask::HideDashboard,
		);

		// ################################
		// SIDE BUTTONS
		// ################################

		// "Home" side button
		let btn = self.state.fetch_component_as::<ComponentButton>("btn_side_home")?;
		self.tasks.handle_button(&btn, FrontendTask::SetTab(TabType::Home));
		self.side_buttons.push((TabType::Home, btn));

		// "Apps" side button
		let btn = self.state.fetch_component_as::<ComponentButton>("btn_side_apps")?;
		self.tasks.handle_button(&btn, FrontendTask::SetTab(TabType::Apps));
		self.side_buttons.push((TabType::Apps, btn));

		// "Games" side button
		let btn = self.state.fetch_component_as::<ComponentButton>("btn_side_games")?;
		self.tasks.handle_button(&btn, FrontendTask::SetTab(TabType::Games));
		self.side_buttons.push((TabType::Games, btn));

		// "Monado side button"
		let btn = self.state.fetch_component_as::<ComponentButton>("btn_side_monado")?;
		self.tasks.handle_button(&btn, FrontendTask::SetTab(TabType::Monado));
		self.side_buttons.push((TabType::Monado, btn));

		// "WiVRn" side button
		let btn = self.state.fetch_component_as::<ComponentButton>("btn_side_wivrn")?;
		self.tasks.handle_button(&btn, FrontendTask::SetTab(TabType::Wivrn));
		self.side_buttons.push((TabType::Wivrn, btn));

		// "Settings" side button
		let btn = self.state.fetch_component_as::<ComponentButton>("btn_side_settings")?;
		self.tasks.handle_button(&btn, FrontendTask::SetTab(TabType::Settings));
		self.side_buttons.push((TabType::Settings, btn));

		// ################################
		// BOTTOM BAR BUTTONS
		// ################################

		// "Audio" bottom bar button
		self.tasks.handle_button(
			&self.state.fetch_component_as::<ComponentButton>("btn_audio")?,
			FrontendTask::ShowAudioSettings,
		);

		// "Recenter playspace" bottom bar button
		self.tasks.handle_button(
			&self.state.fetch_component_as::<ComponentButton>("btn_recenter")?,
			FrontendTask::RecenterPlayspace,
		);

		self.tasks.handle_button(
			&self.state.fetch_component_as::<ComponentButton>("btn_playspace")?,
			FrontendTask::ShowPlayspaceMenu,
		);

		self.tasks.handle_button(
			&self.state.fetch_component_as::<ComponentButton>("btn_battery")?,
			FrontendTask::ShowBatteryMenu,
		);

		// "Power" bottom bar button
		self.tasks.handle_button(
			&self.state.fetch_component_as::<ComponentButton>("btn_power")?,
			FrontendTask::ShowPowerMenu,
		);

		Ok(())
	}

	fn action_show_audio_settings(&mut self) -> anyhow::Result<()> {
		let state = self.open_shell_menu(
			&self.window_audio_settings.clone(),
			"Audio",
			"gui/view/audio_settings.xml",
		)?;
		for (id, microphone) in [("output_volume", false), ("mic_volume", true)] {
			let tasks = self.tasks.clone();
			let syncing = self.audio_syncing.clone();
			state
				.fetch_component_as::<ComponentSlider>(id)?
				.on_value_changed(Box::new(move |_, event| {
					if !syncing.get() {
						tasks.push(FrontendTask::AudioControl(if microphone {
							AudioControl::MicrophoneVolume(event.value)
						} else {
							AudioControl::OutputVolume(event.value)
						}));
					}
				}));
		}
		for (id, microphone) in [("output_mute", false), ("mic_mute", true)] {
			let tasks = self.tasks.clone();
			let syncing = self.audio_syncing.clone();
			state
				.fetch_component_as::<ComponentCheckbox>(id)?
				.on_toggle(Box::new(move |_, event| {
					if syncing.get() {
						return Ok(());
					}
					tasks.push(FrontendTask::AudioControl(if microphone {
						AudioControl::MicrophoneMute(event.checked)
					} else {
						AudioControl::OutputMute(event.checked)
					}));
					Ok(())
				}));
		}
		self.menu_audio = Some(state);
		// Controls only act once they show PipeWire's values (see update_audio_menu)
		self.audio_syncing.set(true);
		Ok(())
	}

	fn update_audio_menu(&mut self, data: &mut T) -> anyhow::Result<()> {
		let Some(state) = &mut self.menu_audio else {
			return Ok(());
		};
		if state.fetch_widget(&self.layout.state, "output_value").is_err() {
			return Ok(());
		}
		let audio = self.interface.headset_audio(data);
		self.audio_syncing.set(true);
		let result = (|| -> anyhow::Result<()> {
			for (prefix, endpoint) in [("output", audio.output), ("mic", audio.microphone)] {
				let label = state.fetch_widget(&self.layout.state, &format!("{prefix}_value"))?;
				let text = endpoint.map_or_else(
					|| "Unavailable".to_owned(),
					|endpoint| format!("{} %", endpoint.volume.round()),
				);
				label
					.widget
					.cast::<WidgetLabel>()?
					.set_text(&mut self.layout.common(), Translation::from_raw_text(&text));
				if let Some(endpoint) = endpoint {
					state
						.fetch_component_as::<ComponentSlider>(&format!("{prefix}_volume"))?
						.set_value_primary(&mut self.layout.common(), endpoint.volume);
					state
						.fetch_component_as::<ComponentCheckbox>(&format!("{prefix}_mute"))?
						.set_checked(&mut self.layout.common(), endpoint.muted);
				}
			}
			Ok(())
		})();
		// Until both endpoints were read, the controls still show their defaults and must not apply them
		self
			.audio_syncing
			.set(audio.output.is_none() || audio.microphone.is_none());
		result
	}

	fn open_shell_menu(&mut self, window: &WguiWindow, title: &str, path: &'static str) -> anyhow::Result<ParserState> {
		self.window_audio_settings.close();
		self.window_power.close();
		self.window_playspace.close();
		self.window_battery.close();
		window.open(&mut WguiWindowParams {
			position: Vec2::new(64.0, 64.0),
			layout: &mut self.layout,
			extra: WguiWindowParamsExtra {
				fixed_width: Some(400.0),
				placement: WguiWindowPlacement::BottomLeft,
				close_if_clicked_outside: true,
				title: Some(Translation::from_raw_text(title)),
				..Default::default()
			},
		})?;
		wgui::parser::parse_from_assets(
			&ParseDocumentParams {
				globals: self.globals.clone(),
				path: AssetPathRef::BuiltIn(path),
				extra: Default::default(),
			},
			&mut self.layout,
			window.get_content().id,
		)
	}

	fn action_show_power_menu(&mut self) -> anyhow::Result<()> {
		let state = self.open_shell_menu(&self.window_power.clone(), "Power", "gui/view/power_menu.xml")?;
		for action in ["sleep", "restart_stack", "restart", "poweroff", "steamvr"] {
			self.tasks.handle_button(
				&state.fetch_component_as::<ComponentButton>(action)?,
				FrontendTask::PowerAction(action),
			);
		}
		self.menu_power = Some(state);
		Ok(())
	}

	fn action_show_playspace_menu(&mut self, data: &mut T) -> anyhow::Result<()> {
		let state = self.open_shell_menu(
			&self.window_playspace.clone(),
			"Floor / playspace",
			"gui/view/playspace_menu.xml",
		)?;
		self.tasks.handle_button(
			&state.fetch_component_as::<ComponentButton>("detect_floor")?,
			FrontendTask::DetectFloor,
		);
		let tasks = self.tasks.clone();
		state
			.fetch_component_as::<ComponentCheckbox>("debug_floor")?
			.on_toggle(Box::new(move |_, event| {
				tasks.push(FrontendTask::DebugFloor(event.checked));
				Ok(())
			}));
		self.menu_playspace = Some(state);
		self.update_playspace_menu(data)
	}

	fn action_show_battery_menu(&mut self) -> anyhow::Result<()> {
		let state = self.open_shell_menu(
			&self.window_battery.clone(),
			"Battery / performance",
			"gui/view/battery_menu.xml",
		)?;
		for (id, mode) in [
			("mode_streaming", frame_power::PerformanceMode::StreamingOptimized),
			("mode_standard", frame_power::PerformanceMode::Standard),
		] {
			self.tasks.handle_button(
				&state.fetch_component_as::<ComponentButton>(id)?,
				FrontendTask::SetPerformanceMode(mode),
			);
		}
		self.menu_battery = Some(state);
		self.battery_updated = None;
		self.update_battery_menu()
	}

	/// Battery state, power draw and the performance mode, once a second while the menu is open
	fn update_battery_menu(&mut self) -> anyhow::Result<()> {
		let Some(state) = &mut self.menu_battery else {
			return Ok(());
		};
		if self
			.battery_updated
			.is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(1))
		{
			return Ok(());
		}
		self.battery_updated = Some(std::time::Instant::now());
		let battery = frame_power::battery();
		let battery_text = match &battery {
			None => "Battery state unavailable".to_owned(),
			Some(battery) => {
				let status = if battery.external_power { "charging" } else { "on battery" };
				let mut text = if battery.full {
					"Full".to_owned()
				} else {
					format!("{} %, {status}", frame_power::shown_percent(battery.percent / 100.))
				};
				if let Some(seconds) = battery.seconds_left {
					let until = if battery.external_power { "until full" } else { "left" };
					text += &format!(", {}:{:02} h {until}", seconds / 3600, seconds / 60 % 60);
				}
				text
			}
		};

		// The same rows every time, with what's unknown marked
		let watts = |watts: Option<f32>| watts.map_or_else(|| "–".to_owned(), |watts| format!("{watts:.1} W"));
		let power = frame_power::power();
		let charge = battery.as_ref().and_then(|battery| battery.charge_watts);
		let battery_row = match charge {
			Some(charge) if charge < -0.05 => format!("{:.1} W draining", -charge),
			Some(charge) if charge > 0.05 => format!("{charge:.1} W charging"),
			Some(_) => "idle".to_owned(),
			None => "–".to_owned(),
		};
		// The charger feeds the system and charges the battery
		let charger_row = match &battery {
			Some(battery) if !battery.external_power => "not connected".to_owned(),
			_ => watts(power.as_ref().zip(charge).map(|(power, charge)| power.system + charge)),
		};
		let values = [
			watts(power.as_ref().map(|power| power.system)),
			watts(power.as_ref().map(|power| power.cpu)),
			watts(power.as_ref().map(|power| power.gpu)),
			battery_row,
			charger_row,
		]
		.join("\n");

		for (id, text) in [
			("battery", battery_text.as_str()),
			("power_names", "System\nCPU\nGPU\nBattery\nCharger"),
			("power_values", values.as_str()),
		] {
			if let Ok(label) = state.fetch_widget(&self.layout.state, id) {
				label
					.widget
					.cast::<WidgetLabel>()?
					.set_text(&mut self.layout.common(), Translation::from_raw_text(text));
			}
		}
		let mode = frame_power::performance_mode();
		for (id, button_mode) in [
			("mode_streaming", frame_power::PerformanceMode::StreamingOptimized),
			("mode_standard", frame_power::PerformanceMode::Standard),
		] {
			state
				.fetch_component_as::<ComponentButton>(id)?
				.set_sticky_state(&mut self.layout.common(), mode == button_mode);
		}
		Ok(())
	}

	fn update_playspace_menu(&mut self, data: &mut T) -> anyhow::Result<()> {
		let Some(state) = &mut self.menu_playspace else {
			return Ok(());
		};
		let Ok(label) = state.fetch_widget(&self.layout.state, "status") else {
			return Ok(());
		};
		let status = self.interface.floor_status(data);
		let text = match status.remaining_seconds {
			Some(seconds) => format!("Collecting floor samples: {seconds} s remaining"),
			None => "Floor height is fixed.".to_owned(),
		};
		label
			.widget
			.cast::<WidgetLabel>()?
			.set_text(&mut self.layout.common(), Translation::from_raw_text(&text));
		state
			.fetch_component_as::<ComponentCheckbox>("debug_floor")?
			.set_checked(&mut self.layout.common(), status.debug_visible);
		Ok(())
	}

	fn action_recenter_playspace(&mut self, data: &mut T) -> anyhow::Result<()> {
		self.interface.playspace_task(data, DashPlayspaceTask::Recenter)?;
		Ok(())
	}

	fn action_hide_dashboard(&mut self, data: &mut T) {
		self.interface.toggle_dashboard(data);
	}

	fn action_tutorial_graduated(&mut self, data: &mut T) {
		let config = self.interface.general_config(data);
		config.tutorial_graduated = true;
		self.interface.config_changed(data, ConfigChangeKind::Other);
	}

	fn action_open_url(&mut self, url: Rc<str>) {
		let _ = std::process::Command::new("xdg-open").arg(url.as_ref()).spawn();
		self
			.tasks
			.push(FrontendTask::PushToast(Translation::from_raw_text_string(format!(
				"Opened URL: {}",
				url
			))));
	}

	fn action_run_command(&mut self, command: String, args: Vec<String>) {
		if let Err(e) = Command::new(command).args(args).spawn() {
			self
				.tasks
				.push(FrontendTask::PushToast(Translation::from_raw_text_string(format!(
					"Failed to launch command: {e}"
				))))
		};
	}
}

