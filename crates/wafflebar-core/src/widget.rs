use crate::render::{Color, RenderContext};

/// Axis-aligned bounding box for widget layout.
#[derive(Debug, Clone, Copy)]
pub struct WidgetBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Trait that all panel widgets must implement.
pub trait Widget: Send {
    /// Unique identifier for this widget instance.
    fn id(&self) -> &str;

    /// How much horizontal space this widget wants, given the panel height.
    fn desired_width(&self, panel_height: u32) -> u32;

    /// Called periodically to update internal state (e.g., clock tick).
    /// Returns true if the widget needs to be redrawn.
    fn update(&mut self) -> bool;

    /// Render the widget into the given context within the specified bounds.
    fn render(&self, ctx: &mut RenderContext, bounds: WidgetBounds, fg: Color, font_size: f32);
}
