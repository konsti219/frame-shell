use std::sync::Arc;

use glam::Affine2;
use wgui::gfx::cmd::WGfxClearMode;
use wgui::gfx::{Scissor, Vert2Uv, WGfxPipeline, WPipelineCreateInfo};
use wlx_common::overlays::{BackendAttrib, BackendAttribValue};

use crate::{
    backend::input::{HoverResult, PointerHit},
    state::AppState,
    subsystem::hid::WheelDelta,
    windowing::backend::{
        FrameMeta, OverlayBackend, OverlayEventData, RenderResources, ShouldRender,
    },
};

#[derive(Default)]
pub struct FloorGrid {
    pipeline: Option<Arc<WGfxPipeline<Vert2Uv>>>,
    rendered: bool,
    timestamp: u64,
    samples: bool,
}

impl FloorGrid {
    pub fn samples() -> Self {
        Self {
            samples: true,
            ..Self::default()
        }
    }
}

impl OverlayBackend for FloorGrid {
    fn init(&mut self, app: &mut AppState) -> anyhow::Result<()> {
        let (vertex, fragment) = if self.samples {
            ("vert_floor_samples", "frag_floor_samples")
        } else {
            ("vert_quad", "frag_floor_grid")
        };
        self.pipeline = Some(app.gfx.create_pipeline(
            &app.gfx_extras.shaders[vertex],
            &app.gfx_extras.shaders[fragment],
            WPipelineCreateInfo::new(FrameMeta::default().format),
        )?);
        Ok(())
    }

    fn pause(&mut self, _: &mut AppState) -> anyhow::Result<()> {
        Ok(())
    }
    fn resume(&mut self, _: &mut AppState) -> anyhow::Result<()> {
        Ok(())
    }

    fn should_render(&mut self, _: &mut AppState) -> anyhow::Result<ShouldRender> {
        let unchanged = self.rendered
            && (!self.samples || self.timestamp == crate::floor_estimate::snapshot().timestamp);
        Ok(if unchanged {
            ShouldRender::Can
        } else {
            ShouldRender::Should
        })
    }

    fn render(
        &mut self,
        app: &mut AppState,
        resources: &mut RenderResources,
    ) -> anyhow::Result<()> {
        let extent = resources.extent;
        if !self.samples {
            let pass = self.pipeline.as_ref().unwrap().create_pass(
                [extent[0] as f32, extent[1] as f32],
                [0.0, 0.0],
                app.gfx_extras.quad_verts.clone(),
                0..4,
                0..1,
                vec![],
                Scissor::new([0, 0], extent),
            )?;
            for cmd in &mut resources.cmd_bufs {
                cmd.run_ref(&pass)?;
            }
            self.rendered = true;
            return Ok(());
        }
        let estimate = crate::floor_estimate::snapshot();
        let floor_from_raw = app.floor_from_raw;
        let head = app
            .floor_from_stage
            .transform_point3a(app.input_state.hmd.translation);
        let centre_x = (head.x / 0.5).round() * 0.5;
        let centre_z = (head.z / 0.5).round() * 0.5;
        let mut points = [0.0f32; 256 * 4];
        for (i, (raw, horizontal)) in estimate.points.iter().take(256).enumerate() {
            let point = floor_from_raw.transform_point3(*raw);
            let selected = *horizontal
                && estimate
                    .height
                    .is_some_and(|height| (raw.y - height).abs() < 0.06);
            points[i * 4] = point.x - centre_x;
            // Positive Vulkan viewport Y maps to positive floor Z after the quad rotation.
            points[i * 4 + 1] = point.z - centre_z;
            points[i * 4 + 2] = if selected {
                2.0
            } else if *horizontal {
                1.0
            } else {
                0.0
            };
        }
        if !estimate.points.is_empty() {
            let pipeline = self.pipeline.as_ref().unwrap();
            let set = pipeline.uniform_buffer_upload(0, &points)?;
            let pass = pipeline.create_pass(
                [extent[0] as f32, extent[1] as f32],
                [0.0, 0.0],
                app.gfx_extras.quad_verts.clone(),
                0..4,
                0..estimate.points.len().min(256) as u32,
                vec![set],
                Scissor::new([0, 0], extent),
            )?;
            for cmd in &mut resources.cmd_bufs {
                cmd.run_ref(&pass)?;
            }
        }
        self.timestamp = estimate.timestamp;
        self.rendered = true;
        Ok(())
    }

    fn frame_meta(&mut self) -> Option<FrameMeta> {
        Some(FrameMeta {
            extent: if self.samples {
                [1024, 1024]
            } else {
                [4096, 4096]
            },
            clear: WGfxClearMode::Clear([0.0; 4]),
            ..FrameMeta::default()
        })
    }

    fn notify(&mut self, _: &mut AppState, _: OverlayEventData) -> anyhow::Result<()> {
        Ok(())
    }
    fn on_hover(&mut self, _: &mut AppState, _: &PointerHit) -> HoverResult {
        HoverResult::default()
    }
    fn on_left(&mut self, _: &mut AppState, _: usize) {}
    fn on_pointer(&mut self, _: &mut AppState, _: &PointerHit, _: bool) {}
    fn on_scroll(&mut self, _: &mut AppState, _: &PointerHit, _: WheelDelta) {}
    fn get_interaction_transform(&mut self) -> Option<Affine2> {
        None
    }
    fn get_attrib(&self, _: BackendAttrib) -> Option<BackendAttribValue> {
        None
    }
    fn set_attrib(&mut self, _: &mut AppState, _: BackendAttribValue) -> bool {
        false
    }
}
