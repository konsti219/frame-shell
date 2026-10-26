//! The floor grid and its estimate samples

use super::*;

pub fn floor_visible() -> bool {
    FLOOR_PINNED.load(Ordering::Relaxed)
}

pub fn set_floor_visible(visible: bool) {
    FLOOR_PINNED.store(visible, Ordering::Relaxed);
}

pub fn floor_debug_visible(app: &AppState) -> bool {
    floor_visible() || app.floor_calibration.remaining_seconds().is_some()
}

/// Shows the grid without enabling debug dots.
pub fn show_floor() {
    FLOOR_UNTIL_MS.store(START.elapsed().as_millis() as u64 + 5000, Ordering::Relaxed);
}

pub(crate) fn floor_overlay(_: &mut AppState) -> Option<OverlayWindowConfig> {
    floor_layer(false)
}
pub(crate) fn floor_samples_overlay(_: &mut AppState) -> Option<OverlayWindowConfig> {
    floor_layer(true)
}

pub(crate) fn floor_layer(samples: bool) -> Option<OverlayWindowConfig> {
    Some(OverlayWindowConfig {
        name: if samples {
            FLOOR_SAMPLES_NAME.clone()
        } else {
            FLOOR_NAME.clone()
        },
        default_state: OverlayWindowState {
            interactable: false,
            grabbable: false,
            positioning: wlx_common::windowing::Positioning::Static,
            ..OverlayWindowState::default()
        },
        global: true,
        ..OverlayWindowConfig::from_backend(Box::new(if samples {
            crate::overlays::floor::FloorGrid::samples()
        } else {
            crate::overlays::floor::FloorGrid::default()
        }))
    })
}

/// Keeps the static grid and small dynamic sample texture aligned on the calibrated floor.
pub(crate) fn update_floor(app: &mut AppState) {
    let debug = floor_debug_visible(app);
    let grid =
        debug || (START.elapsed().as_millis() as u64) < FLOOR_UNTIL_MS.load(Ordering::Relaxed);
    let head = app
        .floor_from_stage
        .transform_point3a(app.input_state.hmd.translation);
    let transform = app.floor_from_stage.inverse()
        * glam::Affine3A::from_scale_rotation_translation(
            glam::Vec3::splat(8.0),
            glam::Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
            glam::vec3(
                (head.x / 0.5).round() * 0.5,
                0.0,
                (head.z / 0.5).round() * 0.5,
            ),
        );
    type Factory = fn(&mut AppState) -> Option<OverlayWindowConfig>;
    for (name, spawned, want, factory) in [
        (
            FLOOR_NAME.clone(),
            &FLOOR_SPAWNED,
            grid,
            floor_overlay as Factory,
        ),
        (
            FLOOR_SAMPLES_NAME.clone(),
            &FLOOR_SAMPLES_SPAWNED,
            debug,
            floor_samples_overlay as Factory,
        ),
    ] {
        if !want {
            if spawned.swap(false, Ordering::Relaxed) {
                app.tasks
                    .enqueue(TaskType::Overlay(OverlayTask::Drop(OverlaySelector::Name(
                        name,
                    ))));
            }
            continue;
        }
        if !spawned.swap(true, Ordering::Relaxed) {
            app.tasks.enqueue(TaskType::Overlay(OverlayTask::Spawn(
                OverlaySelector::Name(name.clone()),
                SpawnPos::FixedNoRealign,
                Box::new(factory),
            )));
        }
        app.tasks.enqueue(TaskType::Overlay(OverlayTask::Modify(
            OverlaySelector::Name(name),
            Box::new(move |app, config| {
                if config.active_state.is_none() {
                    config.activate(app, false);
                }
                if let Some(state) = config.active_state.as_mut() {
                    state.transform = transform;
                    config.dirty = true;
                }
            }),
        )));
    }
}
