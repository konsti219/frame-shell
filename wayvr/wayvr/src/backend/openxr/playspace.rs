use glam::{Affine3A, Mat3A, Quat, Vec3, Vec3A, vec3a};
use libmonado::{Monado, Pose, ReferenceSpaceType};
use wgui::log::LogErr;

use crate::{
    backend::{
        playspace_common::{self, SpaceGravity, SpaceGravityUpdateParams},
        task::{OverlayTask, PlayspaceTask, TaskType},
    },
    state::{AppState, PlayspaceState, load_playspace_state, save_playspace_state},
    windowing::manager::OverlayWindowManager,
};

use super::overlay::OpenXrOverlayData;

struct MoverData<T> {
    pose: Affine3A,
    hand: usize,
    hand_pose: T,
}

pub(super) struct PlayspaceMover {
    drag: Option<MoverData<Vec3A>>,
    rotate: Option<MoverData<Affine3A>>,
    gravity: SpaceGravity,
    playspace_state: PlayspaceState,
    /// When the recenter action started being held, and whether it already recentered
    recenter_hold: Option<(std::time::Instant, bool)>,
    drag_offset: Affine3A,
    initialized: bool,
    saved_calibration: bool,
}

impl PlayspaceMover {
    pub fn new() -> Self {
        log::info!("Monado: using space offset API");

        let saved = load_playspace_state().ok();
        let saved_calibration = saved.is_some();
        Self {
            drag: None,
            rotate: None,
            gravity: SpaceGravity::new(),
            playspace_state: saved.unwrap_or_default(),
            recenter_hold: None,
            drag_offset: Affine3A::IDENTITY,
            initialized: false,
            saved_calibration,
        }
    }

    pub fn handle_task(
        &mut self,
        app: &mut AppState,
        overlays: &mut OverlayWindowManager<OpenXrOverlayData>,
        task: PlayspaceTask,
    ) {
        let Some(monado) = &mut app.monado_state else {
            return; // monado not available
        };

        self.initialize(&mut monado.ipc);

        match task {
            PlayspaceTask::FixFloor => {
                self.fix_floor(app, overlays);
            }
            PlayspaceTask::Reset => {
                self.reset_offset(app, overlays);
            }
            PlayspaceTask::Recenter => {
                self.recenter(app, overlays);
            }
            PlayspaceTask::SaveCenter => {
                self.save_center();
            }
            PlayspaceTask::ResetCenter => {
                app.floor_calibration.start(10);
                self.set_calibration(app, overlays, Affine3A::IDENTITY);
            }
        }
    }

    pub fn update(
        &mut self,
        overlays: &mut OverlayWindowManager<OpenXrOverlayData>,
        app: &mut AppState,
    ) {
        if let Some(monado) = &mut app.monado_state {
            self.initialize(&mut monado.ipc);
        }

        self.update_floor_estimate(app, overlays);

        // Recenter on a held button (the Frame's Steam button), like SteamVR
        if app
            .input_state
            .pointers
            .iter()
            .any(|pointer| pointer.now.recenter)
        {
            let (since, done) = self
                .recenter_hold
                .get_or_insert((std::time::Instant::now(), false));
            if !*done && since.elapsed() >= std::time::Duration::from_millis(1500) {
                *done = true;
                log::info!("Recenter (held)");
                self.recenter(app, overlays);
                return;
            }
        } else if let Some((_, recentered)) = self.recenter_hold.take() {
            if !recentered {
                app.tasks
                    .enqueue(TaskType::Overlay(OverlayTask::ToggleDashboard));
            }
        }

        let Some(monado) = &mut app.monado_state else {
            return; // monado not available
        };

        for pointer in &app.input_state.pointers {
            if pointer.now.space_reset {
                if !pointer.before.space_reset {
                    log::info!("Space reset");
                    self.reset_offset(app, overlays);
                }
                return;
            }
        }

        if let Some(mut data) = self.rotate.take() {
            let pointer = &app.input_state.pointers[data.hand];
            if !pointer.now.space_rotate {
                log::info!("End space rotate");
                return;
            }

            let new_hand = data.pose * app.input_state.pointers[data.hand].raw_pose;
            let before_pose = data.pose;

            if app.session.config.space_rotate_unlocked {
                // pivot on the hand so the body orbits it
                let pose = new_hand * data.hand_pose.inverse() * data.pose;
                data.pose = Affine3A::from_rotation_translation(
                    Quat::from_affine3(&pose).normalize(),
                    pose.translation.into(),
                );
            } else {
                let pose_quat = Quat::from_affine3(&data.pose).normalize();
                let dq = (pose_quat.conjugate()
                    * Quat::from_affine3(&(new_hand * data.hand_pose.inverse()))
                    * pose_quat)
                    .normalize();
                let rel_y = f32::atan2(
                    2.0 * dq.y.mul_add(dq.w, dq.x * dq.z),
                    2.0f32.mul_add(dq.w.mul_add(dq.w, dq.x * dq.x), -1.0),
                );

                let mut space_transform = Affine3A::from_rotation_y(rel_y);
                space_transform.translation = (space_transform
                    .transform_vector3a(app.input_state.hmd.translation)
                    - app.input_state.hmd.translation)
                    * -1.0;

                data.pose *= space_transform;
                data.pose.matrix3 = Mat3A::from_quat(Quat::from_affine3(&data.pose).normalize());
            }

            if !app.session.config.space_drag_affects_world {
                playspace_common::shift_world(overlays, &mut app.anchor, &before_pose, &data.pose);
            }
            data.hand_pose = new_hand;

            self.drag_offset = self.playspace_state.openxr_space_center.inverse() * data.pose;
            apply_offset(data.pose, &mut monado.ipc);
            self.rotate = Some(data);
            // drag and rotate can share a button
            return;
        } else {
            // start space rotate
            for (i, pointer) in app.input_state.pointers.iter().enumerate() {
                if pointer.now.space_rotate {
                    let Ok(pose) = monado
                        .ipc
                        .get_reference_space_offset(ReferenceSpaceType::Stage)
                        .log_err("Could not get reference space offset.")
                        .map(|p| {
                            Affine3A::from_rotation_translation(
                                p.orientation.into(),
                                p.position.into(),
                            )
                        })
                    else {
                        return;
                    };

                    let hand_pose = pose * pointer.raw_pose;
                    self.rotate = Some(MoverData {
                        pose,
                        hand: i,
                        hand_pose,
                    });
                    self.drag = None;
                    log::info!("Start space rotate");
                    return;
                }
            }
        }

        if let Some(mut data) = self.drag.take() {
            let new_hand = data
                .pose
                .transform_point3a(app.input_state.pointers[data.hand].raw_pose.translation);
            let relative_pos = if app.session.config.space_drag_unlocked {
                new_hand - data.hand_pose
            } else {
                vec3a(0., new_hand.y - data.hand_pose.y, 0.)
            } * app.session.config.space_drag_multiplier;
            let pointer = &app.input_state.pointers[data.hand];

            if !pointer.now.space_drag {
                self.gravity.mark_end_drag(
                    &app.session.config,
                    self.playspace_state
                        .openxr_space_center
                        .inverse()
                        .transform_vector3a(relative_pos),
                    self.drag_offset.translation,
                    app.delta_time,
                );

                log::info!("End space drag");
                return;
            }

            if relative_pos.length_squared() > 1000.0 {
                log::warn!("Space drag too fast, ignoring");
                return;
            }

            let before_pose = data.pose;
            data.pose.translation += relative_pos;
            if !app.session.config.space_drag_affects_world {
                playspace_common::shift_world(overlays, &mut app.anchor, &before_pose, &data.pose);
            }
            data.hand_pose = new_hand;

            self.drag_offset = self.playspace_state.openxr_space_center.inverse() * data.pose;
            apply_offset(data.pose, &mut monado.ipc);
            self.drag = Some(data);
        } else {
            // start space drag
            for (i, pointer) in app.input_state.pointers.iter().enumerate() {
                if pointer.now.space_drag {
                    let Ok(pose) = monado
                        .ipc
                        .get_reference_space_offset(ReferenceSpaceType::Stage)
                        .log_err("Could not get reference space offset.")
                        .map(|p| {
                            Affine3A::from_rotation_translation(
                                p.orientation.into(),
                                p.position.into(),
                            )
                        })
                    else {
                        return;
                    };

                    let hand_pos = pose.transform_point3a(pointer.raw_pose.translation);
                    self.drag = Some(MoverData {
                        pose,
                        hand: i,
                        hand_pose: hand_pos,
                    });
                    log::info!("Start space drag");
                    return;
                }
            }
        }

        if let Some(res) = self.gravity.update(SpaceGravityUpdateParams {
            dt: app.delta_time,
            dragging: self.drag.is_some(),
            config: &app.session.config,
            floor_height: app.session.config.space_gravity_floor_height,
        }) {
            let before_pose = self.playspace_state.openxr_space_center * self.drag_offset;
            self.drag_offset.translation = res.playspace_pos;
            let after_pose = self.playspace_state.openxr_space_center * self.drag_offset;
            apply_offset(after_pose, &mut monado.ipc);

            if !app.session.config.space_drag_affects_world {
                playspace_common::shift_world(overlays, &mut app.anchor, &before_pose, &after_pose);
            }
        }
    }

    pub(super) fn floor_from_stage(&self) -> Affine3A {
        self.drag_offset
    }

    pub(super) fn floor_from_raw(&self) -> Affine3A {
        self.playspace_state.openxr_space_center.inverse()
    }

    fn initialize(&mut self, monado: &mut Monado) {
        if self.initialized {
            return;
        }
        if !self.saved_calibration {
            let Ok(pose) = monado
                .get_reference_space_offset(ReferenceSpaceType::Stage)
                .log_err("Could not read playspace calibration")
            else {
                return;
            };
            self.playspace_state.openxr_space_center =
                Affine3A::from_rotation_translation(pose.orientation.into(), pose.position.into());
        }
        self.initialized = apply_offset(self.playspace_state.openxr_space_center, monado);
    }

    fn update_floor_estimate(
        &mut self,
        app: &mut AppState,
        overlays: &mut OverlayWindowManager<OpenXrOverlayData>,
    ) {
        crate::floor_estimate::request(crate::frame_shell::floor_debug_visible(app));
        let estimate = crate::floor_estimate::snapshot();
        app.floor_calibration.update(&estimate);
        if !self.initialized || self.drag.is_some() || self.rotate.is_some() {
            return;
        }
        let Some(height) = app.floor_calibration.height else {
            return;
        };
        let Some(monado) = &mut app.monado_state else {
            return;
        };
        let mut calibration = self.playspace_state.openxr_space_center;
        if (calibration.translation.y - height).abs() < 0.001 && !app.floor_calibration.needs_save {
            return;
        }
        let before = calibration * self.drag_offset;
        calibration.translation.y = height;
        let after = calibration * self.drag_offset;
        if !apply_offset(after, &mut monado.ipc) {
            return;
        }
        self.playspace_state.openxr_space_center = calibration;
        if app.floor_calibration.needs_save {
            self.save_center();
            app.floor_calibration.needs_save = false;
        }
        if !app.session.config.space_drag_affects_world {
            playspace_common::shift_world(overlays, &mut app.anchor, &before, &after);
        }
    }

    fn stop_moving(&mut self) {
        self.drag = None;
        self.rotate = None;
        self.gravity.reset();
    }

    fn set_calibration(
        &mut self,
        app: &mut AppState,
        overlays: &mut OverlayWindowManager<OpenXrOverlayData>,
        calibration: Affine3A,
    ) {
        let Some(monado) = &mut app.monado_state else {
            return;
        };
        let before = self.playspace_state.openxr_space_center * self.drag_offset;
        let after = calibration * self.drag_offset;
        if !apply_offset(after, &mut monado.ipc) {
            return;
        }
        self.stop_moving();
        self.playspace_state.openxr_space_center = calibration;
        self.save_center();
        if !app.session.config.space_drag_affects_world {
            playspace_common::shift_world(overlays, &mut app.anchor, &before, &after);
        }
        crate::frame_shell::show_floor();
    }

    pub fn recenter(
        &mut self,
        app: &mut AppState,
        overlays: &mut OverlayWindowManager<OpenXrOverlayData>,
    ) {
        // Centred on the head in the current (dragged) stage, keeping the drag: stage = calibration * drag
        let head = app.input_state.hmd;
        let forward = head.transform_vector3a(Vec3A::NEG_Z);
        if forward.x * forward.x + forward.z * forward.z <= f32::EPSILON {
            return;
        }
        let yaw = (-forward.x).atan2(-forward.z);
        let horizontal = Vec3::new(head.translation.x, 0.0, head.translation.z);
        let stage = self.playspace_state.openxr_space_center
            * self.drag_offset
            * Affine3A::from_rotation_translation(Quat::from_rotation_y(yaw), horizontal);
        let calibration = stage * self.drag_offset.inverse();
        self.set_calibration(app, overlays, calibration);
    }

    pub fn reset_offset(
        &mut self,
        app: &mut AppState,
        overlays: &mut OverlayWindowManager<OpenXrOverlayData>,
    ) {
        let Some(monado) = &mut app.monado_state else {
            return;
        };
        let calibration = self.playspace_state.openxr_space_center;
        let before = calibration * self.drag_offset;
        if !apply_offset(calibration, &mut monado.ipc) {
            return;
        }
        self.stop_moving();
        self.drag_offset = Affine3A::IDENTITY;
        if !app.session.config.space_drag_affects_world {
            playspace_common::shift_world(overlays, &mut app.anchor, &before, &calibration);
        }
    }

    pub fn fix_floor(
        &mut self,
        app: &mut AppState,
        overlays: &mut OverlayWindowManager<OpenXrOverlayData>,
    ) {
        let mut calibration = self.playspace_state.openxr_space_center;
        let floor = app
            .input_state
            .pointers
            .iter()
            .map(|hand| {
                self.drag_offset
                    .transform_point3a(hand.raw_pose.translation)
                    .y
            })
            .fold(f32::INFINITY, f32::min)
            - 0.05;
        if !floor.is_finite() {
            return;
        }
        calibration.translation.y += floor;
        app.floor_calibration.stop();
        self.set_calibration(app, overlays, calibration);
    }

    pub fn save_center(&mut self) {
        let _ = save_playspace_state(&self.playspace_state)
            .log_err("Could not save playspace calibration");
    }
}

fn apply_offset(transform: Affine3A, monado: &mut Monado) -> bool {
    let pose = Pose {
        position: transform.translation.into(),
        orientation: Quat::from_affine3(&transform).into(),
    };
    monado
        .set_reference_space_offset(ReferenceSpaceType::Stage, pose)
        .log_err("Could not apply playspace offset")
        .is_ok()
}
