//! Gamepad mode: the Steam Frame controllers become a virtual DualSense (sticks, buttons, triggers, gyro,
//! accelerometer and rumble) for flat games. Holding both stick clicks switches it on and off; meanwhile only the
//! grips still act on the shell, so windows can still be grabbed and moved, and the pointer lines only show while a
//! grip is held.

use std::time::{Duration, Instant};

use glam::{Quat, Vec3};
use openxr::{self as xr, Vector2f};
use wgui::i18n::Translation;
use wlx_common::overlays::ToastTopic;

use crate::{
    backend::input::PointerState,
    overlays::toast::Toast,
    state::AppState,
    subsystem::hid::ds5::{Ds5Rumble, Ds5State, VirtualDs5, button},
};

use super::XrState;

pub(super) const FRAME_PROFILE: &str = "/interaction_profiles/valve/frame_controller_valve";

const DEVICE_NAME: &str = "DualSense Wireless Controller";
const STANDARD_GRAVITY: f32 = 9.806_65;
/// Stick clicks drop out for a moment when the stick tilts while pressed; shorter gaps don't end the hold
const HOLD_GRACE: Duration = Duration::from_millis(150);
/// Steam gives a controller that appears while a game has focus the desktop layout until focus changes, so after
/// switching on, keyboard focus is taken away once Steam has the device and given back shortly after
const FOCUS_BOUNCE_AFTER: Duration = Duration::from_millis(1000);
const FOCUS_BOUNCE_GAP: Duration = Duration::from_millis(200);
const LEFT: usize = 0;
const RIGHT: usize = 1;

pub(super) struct GamepadActions {
    hands: [xr::Path; 2],
    stick: xr::Action<Vector2f>,
    stick_click: xr::Action<bool>,
    trigger: xr::Action<f32>,
    bumper: xr::Action<bool>,
    view: xr::Action<bool>,
    menu: xr::Action<bool>,
    a: xr::Action<bool>,
    b: xr::Action<bool>,
    x: xr::Action<bool>,
    y: xr::Action<bool>,
    dpad_up: xr::Action<bool>,
    dpad_down: xr::Action<bool>,
    dpad_left: xr::Action<bool>,
    dpad_right: xr::Action<bool>,
    aim: xr::Action<xr::Posef>,
}

impl GamepadActions {
    pub(super) fn new(action_set: &xr::ActionSet, instance: &xr::Instance) -> anyhow::Result<Self> {
        let hands = [
            instance.string_to_path("/user/hand/left")?,
            instance.string_to_path("/user/hand/right")?,
        ];
        let button = |name: &str| {
            action_set.create_action::<bool>(
                &format!("gamepad_{name}"),
                &format!("Gamepad {name}"),
                &hands,
            )
        };
        Ok(Self {
            hands,
            stick: action_set.create_action("gamepad_stick", "Gamepad stick", &hands)?,
            stick_click: button("stick_click")?,
            trigger: action_set.create_action("gamepad_trigger", "Gamepad trigger", &hands)?,
            bumper: button("bumper")?,
            view: button("view")?,
            menu: button("menu")?,
            a: button("a")?,
            b: button("b")?,
            x: button("x")?,
            y: button("y")?,
            dpad_up: button("dpad_up")?,
            dpad_down: button("dpad_down")?,
            dpad_left: button("dpad_left")?,
            dpad_right: button("dpad_right")?,
            aim: action_set.create_action("gamepad_aim", "Gamepad aim pose", &hands)?,
        })
    }

    /// Bindings for the Frame controller profile; they go into that profile's single binding suggestion
    pub(super) fn bindings(&self, instance: &xr::Instance) -> Vec<xr::Binding<'_>> {
        let mut bindings = vec![];
        for side in ["left", "right"] {
            let hand = format!("/user/hand/{side}");
            bind(
                &mut bindings,
                instance,
                &format!("{hand}/input/thumbstick"),
                &self.stick,
            );
            bind(
                &mut bindings,
                instance,
                &format!("{hand}/input/thumbstick/click"),
                &self.stick_click,
            );
            bind(
                &mut bindings,
                instance,
                &format!("{hand}/input/trigger/value"),
                &self.trigger,
            );
            bind(
                &mut bindings,
                instance,
                &format!("{hand}/input/bumper/click"),
                &self.bumper,
            );
            bind(
                &mut bindings,
                instance,
                &format!("{hand}/input/aim/pose"),
                &self.aim,
            );
        }
        for (path, action) in [
            ("/user/hand/left/input/view/click", &self.view),
            ("/user/hand/left/input/dpad_up/click", &self.dpad_up),
            ("/user/hand/left/input/dpad_down/click", &self.dpad_down),
            ("/user/hand/left/input/dpad_left/click", &self.dpad_left),
            ("/user/hand/left/input/dpad_right/click", &self.dpad_right),
            ("/user/hand/right/input/menu/click", &self.menu),
            ("/user/hand/right/input/a/click", &self.a),
            ("/user/hand/right/input/b/click", &self.b),
            ("/user/hand/right/input/x/click", &self.x),
            ("/user/hand/right/input/y/click", &self.y),
        ] {
            bind(&mut bindings, instance, path, action);
        }
        bindings
    }
}

fn bind<'a, T: xr::ActionTy>(
    bindings: &mut Vec<xr::Binding<'a>>,
    instance: &xr::Instance,
    path: &str,
    action: &'a xr::Action<T>,
) {
    match instance.string_to_path(path) {
        Ok(path) => bindings.push(xr::Binding::new(action, path)),
        Err(error) => log::warn!("Gamepad mode: no binding for {path}: {error}"),
    }
}

pub(super) struct GamepadMode {
    actions: GamepadActions,
    aim_spaces: Option<[xr::Space; 2]>,
    device: Option<VirtualDs5>,
    hold_started: Option<Instant>,
    /// When both clicks were last seen together
    hold_seen: Option<Instant>,
    hold_handled: bool,
    /// Short buzz on both controllers when the mode switches
    pending_buzz: bool,
    /// When to take keyboard focus away (`false`) or give it back (`true`)
    focus_bounce: Option<(Instant, bool)>,
    /// The stick clicks that switched the mode on stay out of the game until released
    hide_stick_clicks: bool,
    last_velocity: Option<(Vec3, i64)>,
    linear_accel: Vec3,
    last_rumble: Ds5Rumble,
    /// Last logged (active, pressed) of both stick clicks, for diagnostics
    logged_clicks: Option<[(bool, bool); 2]>,
}

impl GamepadMode {
    pub(super) const fn new(actions: GamepadActions) -> Self {
        Self {
            actions,
            aim_spaces: None,
            device: None,
            hold_started: None,
            hold_seen: None,
            hold_handled: false,
            pending_buzz: false,
            focus_bounce: None,
            hide_stick_clicks: false,
            last_velocity: None,
            linear_accel: Vec3::ZERO,
            last_rumble: Ds5Rumble {
                left: 0.0,
                right: 0.0,
            },
            logged_clicks: None,
        }
    }

    pub(super) const fn active(&self) -> bool {
        self.device.is_some()
    }

    /// Call after `sync_actions`
    pub(super) fn update(&mut self, xr: &XrState, app: &mut AppState) -> anyhow::Result<()> {
        let config = &app.session.config;
        if !config.gamepad_mode_enabled {
            if self.device.take().is_some() {
                log::info!("Gamepad mode off (disabled in the settings)");
            }
            return Ok(());
        }
        let hold = Duration::from_millis(u64::from(config.gamepad_mode_hold_ms));

        let raw = [
            self.actions
                .stick_click
                .state(&xr.session, self.actions.hands[LEFT])?,
            self.actions
                .stick_click
                .state(&xr.session, self.actions.hands[RIGHT])?,
        ];
        let seen = raw.map(|state| (state.is_active, state.current_state));
        if self.logged_clicks != Some(seen) {
            log::debug!(
                "Gamepad mode: stick clicks (active, pressed) left {:?} right {:?}",
                seen[LEFT],
                seen[RIGHT]
            );
            self.logged_clicks = Some(seen);
        }
        let clicks = seen.map(|(active, pressed)| active && pressed);
        let now = Instant::now();
        if clicks[LEFT] && clicks[RIGHT] {
            self.hold_seen = Some(now);
            let started = *self.hold_started.get_or_insert(now);
            if !self.hold_handled && now - started >= hold {
                self.hold_handled = true;
                self.pending_buzz = true;
                self.toggle(app);
            }
        } else if self.hold_seen.is_none_or(|seen| now - seen > HOLD_GRACE) {
            self.hold_started = None;
            self.hold_seen = None;
            self.hold_handled = false;
        }
        if !clicks[LEFT] && !clicks[RIGHT] && self.hold_started.is_none() {
            self.hide_stick_clicks = false;
        }

        if let Some((due, restore)) = self.focus_bounce
            && now >= due
        {
            if let Some(server) = app.wvr_server.as_mut() {
                server.bounce_keyboard_focus(restore);
            }
            self.focus_bounce = (!restore).then(|| (now + FOCUS_BOUNCE_GAP, true));
        }

        if self.device.is_some() {
            let state = self.read_state(xr, app, clicks)?;
            if let Some(device) = &mut self.device
                && let Err(error) = device.send(&state)
            {
                log::warn!("Gamepad mode: sending to the virtual DualSense failed: {error}");
            }
        }
        Ok(())
    }

    fn toggle(&mut self, app: &mut AppState) {
        if self.device.take().is_some() {
            // Give focus back if a bounce is half done
            if matches!(self.focus_bounce.take(), Some((_, true)))
                && let Some(server) = app.wvr_server.as_mut()
            {
                server.bounce_keyboard_focus(true);
            }
            log::info!("Gamepad mode off");
            self.show(
                app,
                "Gamepad mode off",
                "The controllers point and click again.",
            );
            return;
        }
        match VirtualDs5::create(DEVICE_NAME) {
            Ok(device) => {
                log::info!("Gamepad mode on: virtual DualSense created");
                self.device = Some(device);
                self.focus_bounce = Some((Instant::now() + FOCUS_BOUNCE_AFTER, false));
                self.hide_stick_clicks = true;
                self.last_velocity = None;
                self.linear_accel = Vec3::ZERO;
                self.show(
                    app,
                    "Gamepad mode on",
                    "The controllers are a DualSense now. Hold both stick clicks to go back.",
                );
            }
            Err(error) => {
                log::error!(
                    "Gamepad mode: could not create the virtual DualSense on /dev/uhid: {error}"
                );
                Toast::new(
                    ToastTopic::Error,
                    Some(Translation::from_raw_text("Gamepad mode unavailable")),
                    Translation::from_raw_text_string(format!(
                        "No access to /dev/uhid ({error}). Reinstall the frame shell to add its udev rule."
                    )),
                )
                .with_timeout(6.0)
                .submit(app);
            }
        }
    }

    #[allow(clippy::unused_self)]
    fn show(&self, app: &mut AppState, title: &str, body: &str) {
        Toast::new(
            ToastTopic::System,
            Some(Translation::from_raw_text(title)),
            Translation::from_raw_text(body),
        )
        .submit(app);
    }

    /// Only the grips keep acting on the shell while gamepad mode is on
    pub(super) fn mask_pointer(state: &mut PointerState) {
        *state = PointerState {
            grab: state.grab,
            grab_float: state.grab_float,
            ..PointerState::default()
        };
    }

    /// The rumble the game asked for, if it changed or is still running (or the switch's buzz)
    pub(super) fn rumble(&mut self, app: &AppState) -> Option<Ds5Rumble> {
        if std::mem::take(&mut self.pending_buzz) {
            self.last_rumble = Ds5Rumble::default();
            return Some(Ds5Rumble {
                left: 0.6,
                right: 0.6,
            });
        }
        let device = self.device.as_ref()?;
        if !app.session.config.gamepad_mode_rumble {
            return None;
        }
        let rumble = device.rumble();
        let changed = rumble != self.last_rumble;
        self.last_rumble = rumble;
        (changed || rumble.left > 0.0 || rumble.right > 0.0).then_some(rumble)
    }

    fn bool_state(
        &self,
        xr: &XrState,
        action: &xr::Action<bool>,
        hand: usize,
    ) -> anyhow::Result<bool> {
        let state = action.state(&xr.session, self.actions.hands[hand])?;
        Ok(state.is_active && state.current_state)
    }

    fn read_state(
        &mut self,
        xr: &XrState,
        app: &AppState,
        clicks: [bool; 2],
    ) -> anyhow::Result<Ds5State> {
        let config = &app.session.config;
        let gyro_hand = if config.gamepad_mode_gyro_left_hand {
            LEFT
        } else {
            RIGHT
        };
        let (gyro, accel) = self
            .motion(xr, gyro_hand, config.gamepad_mode_gyro_invert_pitch)?
            .unwrap_or(([0.0; 3], [0.0, STANDARD_GRAVITY, 0.0]));

        let actions = &self.actions;
        let stick = |hand: usize| -> anyhow::Result<[f32; 2]> {
            let state = actions.stick.state(&xr.session, actions.hands[hand])?;
            Ok(if state.is_active {
                [state.current_state.x, state.current_state.y]
            } else {
                [0.0; 2]
            })
        };
        let trigger = |hand: usize| -> anyhow::Result<f32> {
            let state = actions.trigger.state(&xr.session, actions.hands[hand])?;
            Ok(if state.is_active {
                state.current_state
            } else {
                0.0
            })
        };

        let mut buttons = 0;
        for (action, hand, flag) in [
            // Frame layout 1:1: A/B/X/Y on the right controller, D-pad and View on the left
            (&actions.a, RIGHT, button::CROSS),
            (&actions.b, RIGHT, button::CIRCLE),
            (&actions.x, RIGHT, button::SQUARE),
            (&actions.y, RIGHT, button::TRIANGLE),
            (&actions.bumper, LEFT, button::L1),
            (&actions.bumper, RIGHT, button::R1),
            (&actions.view, LEFT, button::CREATE),
            (&actions.menu, RIGHT, button::OPTIONS),
            (&actions.dpad_up, LEFT, button::DPAD_UP),
            (&actions.dpad_down, LEFT, button::DPAD_DOWN),
            (&actions.dpad_left, LEFT, button::DPAD_LEFT),
            (&actions.dpad_right, LEFT, button::DPAD_RIGHT),
        ] {
            if self.bool_state(xr, action, hand)? {
                buttons |= flag;
            }
        }
        if !self.hide_stick_clicks {
            if clicks[LEFT] {
                buttons |= button::L3;
            }
            if clicks[RIGHT] {
                buttons |= button::R3;
            }
        }

        Ok(Ds5State {
            left_stick: stick(LEFT)?,
            right_stick: stick(RIGHT)?,
            l2: trigger(LEFT)?,
            r2: trigger(RIGHT)?,
            buttons,
            gyro,
            accel,
        })
    }

    /// Gyro (deg/s) and accelerometer (m/s^2) from the controller's tracking, in its own frame. The aim pose has
    /// +X right, +Y up, +Z back: SDL's gamepad axes. Games fuse both, so the accelerometer must include the reaction
    /// to gravity, and inverting the pitch only touches the gyro.
    fn motion(
        &mut self,
        xr: &XrState,
        hand: usize,
        invert_pitch: bool,
    ) -> anyhow::Result<Option<([f32; 3], [f32; 3])>> {
        if self.aim_spaces.is_none() {
            self.aim_spaces = Some([
                self.actions.aim.create_space(
                    &xr.session,
                    self.actions.hands[LEFT],
                    xr::Posef::IDENTITY,
                )?,
                self.actions.aim.create_space(
                    &xr.session,
                    self.actions.hands[RIGHT],
                    xr::Posef::IDENTITY,
                )?,
            ]);
        }
        let Some(spaces) = &self.aim_spaces else {
            return Ok(None);
        };
        let (location, velocity) = spaces[hand].relate(&xr.stage, xr.predicted_display_time)?;
        if !location
            .location_flags
            .contains(xr::SpaceLocationFlags::ORIENTATION_VALID)
        {
            return Ok(None);
        }
        let orientation = location.pose.orientation;
        let to_local =
            Quat::from_xyzw(orientation.x, orientation.y, orientation.z, orientation.w).inverse();

        let mut gyro = Vec3::ZERO;
        if velocity
            .velocity_flags
            .contains(xr::SpaceVelocityFlags::ANGULAR_VALID)
        {
            let angular = velocity.angular_velocity;
            gyro = to_local * Vec3::new(angular.x, angular.y, angular.z);
            gyro = Vec3::new(
                gyro.x.to_degrees(),
                gyro.y.to_degrees(),
                gyro.z.to_degrees(),
            );
            if invert_pitch {
                gyro.x = -gyro.x;
            }
        }

        // Linear acceleration from the change in velocity, smoothed (tracking velocities are noisy)
        let now = xr.predicted_display_time.as_nanos();
        if velocity
            .velocity_flags
            .contains(xr::SpaceVelocityFlags::LINEAR_VALID)
        {
            let linear = velocity.linear_velocity;
            let linear = Vec3::new(linear.x, linear.y, linear.z);
            if let Some((previous, time)) = self.last_velocity {
                let dt = (now - time) as f32 / 1e9;
                if dt > 0.0 && dt < 0.1 {
                    self.linear_accel = self.linear_accel.lerp((linear - previous) / dt, 0.3);
                }
            }
            self.last_velocity = Some((linear, now));
        }
        let accel = to_local * (self.linear_accel + Vec3::new(0.0, STANDARD_GRAVITY, 0.0));

        Ok(Some((gyro.to_array(), accel.to_array())))
    }
}
