use slotmap::Key;

use crate::{
	color::WguiColor,
	drawing::{self, GradientMode, PrimitiveExtent},
	event::CallbackDataCommon,
	globals::Globals,
	layout::WidgetID,
	widget::{WidgetStateFlags, util::WLength},
};

use super::{WidgetObj, WidgetState};

#[derive(Debug, Default)]
pub struct WidgetRectangleParams {
	pub color: WguiColor,
	pub color2: WguiColor,
	pub gradient: GradientMode,

	pub border: f32,
	pub border_color: WguiColor,

	pub round: WLength,

	/// Instead of itself, draws its color over the panels drawn before it, in their shapes: dims what's behind a popup
	/// without covering the layout's transparent cut-outs
	pub dim_below: bool,
}

pub struct WidgetRectangle {
	pub params: WidgetRectangleParams,
	id: WidgetID,
}

impl WidgetRectangle {
	pub fn create(params: WidgetRectangleParams) -> WidgetState {
		WidgetState::new(
			WidgetStateFlags::default(),
			Box::new(Self {
				params,
				id: WidgetID::null(),
			}),
		)
	}

	pub fn set_color(&mut self, common: &mut CallbackDataCommon, color: WguiColor) {
		self.params.color = color;
		common.mark_widget_dirty(self.id);
	}

	pub const fn get_color(&self) -> WguiColor {
		self.params.color
	}

	pub fn set_border_color(&mut self, common: &mut CallbackDataCommon, color: WguiColor) {
		self.params.border_color = color;
		common.mark_widget_dirty(self.id);
	}

	pub const fn get_border_color(&self) -> WguiColor {
		self.params.border_color
	}
}

impl WidgetObj for WidgetRectangle {
	fn draw(&mut self, state: &mut super::DrawState, _params: &super::DrawParams) {
		if self.params.dim_below {
			let color = self.params.color.resolve(&state.globals.palette);
			dim_below(state, color);
			return;
		}
		let boundary = drawing::Boundary::construct_relative(state.transform_stack);

		let round_units = match self.params.round {
			WLength::Units(units) => (f32::min(boundary.size.x, boundary.size.y) as u8 / 2).min(units as u8),
			WLength::Percent(percent) => (f32::min(boundary.size.x, boundary.size.y) * percent / 2.0) as u8,
		};

		let color = self.params.color.resolve(&state.globals.palette);
		let color2 = self.params.color2.resolve(&state.globals.palette);
		let border_color = self.params.border_color.resolve(&state.globals.palette);

		state.primitives.push(drawing::RenderPrimitive::Rectangle(
			PrimitiveExtent {
				boundary,
				transform: state.transform_stack.get().transform,
			},
			drawing::Rectangle {
				color,
				color2,
				border_color,
				gradient: self.params.gradient,
				border: self.params.border,
				round_units,
			},
		));
	}

	fn get_id(&self) -> WidgetID {
		self.id
	}

	fn set_id(&mut self, id: WidgetID) {
		self.id = id;
	}

	fn get_type(&self) -> super::WidgetType {
		super::WidgetType::Rectangle
	}

	fn debug_print(&self, globals: &Globals) -> String {
		format!(
			"[color: {}][color2: {}][gradient: {:?}]",
			self.params.color.resolve(&globals.palette).debug_ansi_block(),
			self.params.color2.resolve(&globals.palette).debug_ansi_block(),
			self.params.gradient,
		)
	}
}

/// The screen-space bounds of a primitive
fn screen_bounds(extent: &drawing::PrimitiveExtent) -> (glam::Vec2, glam::Vec2) {
	let a = extent.transform.transform_point3(extent.boundary.pos.extend(0.0)).truncate();
	let b = extent
		.transform
		.transform_point3((extent.boundary.pos + extent.boundary.size).extend(0.0))
		.truncate();
	(a.min(b), a.max(b))
}

/// Covers the outermost visible rectangles drawn so far (the panels; ones inside them would be dimmed twice)
fn dim_below(state: &mut super::DrawState, color: drawing::Color) {
	let mut outer: Vec<(glam::Vec2, glam::Vec2)> = Vec::new();
	let mut overlays = Vec::new();
	for primitive in state.primitives.iter() {
		let drawing::RenderPrimitive::Rectangle(extent, rectangle) = primitive else {
			continue;
		};
		if rectangle.color.a <= 0.0 && rectangle.color2.a <= 0.0 && (rectangle.border <= 0.0 || rectangle.border_color.a <= 0.0)
		{
			continue;
		}
		let (min, max) = screen_bounds(extent);
		if outer
			.iter()
			.any(|(outer_min, outer_max)| min.cmpge(*outer_min).all() && max.cmple(*outer_max).all())
		{
			continue;
		}
		outer.push((min, max));
		overlays.push(drawing::RenderPrimitive::Rectangle(
			extent.clone(),
			drawing::Rectangle {
				color,
				color2: color,
				round_units: rectangle.round_units,
				..Default::default()
			},
		));
	}
	state.primitives.extend(overlays);
}
