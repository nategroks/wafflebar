//! Clock widget for wafflebar.

use chrono::Local;
use wafflebar_core::render::{Color, RenderContext};
use wafflebar_core::widget::{Widget, WidgetBounds};

/// A simple clock widget that displays the current time.
pub struct ClockWidget {
    id: String,
    format: String,
    cached_text: String,
}

impl ClockWidget {
    pub fn new(id: String, format: Option<String>) -> Self {
        let format = format.unwrap_or_else(|| "%H:%M".to_string());
        let cached_text = Local::now().format(&format).to_string();
        Self {
            id,
            format,
            cached_text,
        }
    }
}

impl Widget for ClockWidget {
    fn id(&self) -> &str {
        &self.id
    }

    fn desired_width(&self, panel_height: u32) -> u32 {
        // Estimate width based on character count and font size
        // Approximate: each character is ~0.6 * panel_height wide
        let char_width = (panel_height as f32 * 0.45) as u32;
        (self.cached_text.len() as u32) * char_width + 16 // padding
    }

    fn update(&mut self) -> bool {
        let new_text = Local::now().format(&self.format).to_string();
        if new_text != self.cached_text {
            self.cached_text = new_text;
            true
        } else {
            false
        }
    }

    fn render(&self, ctx: &mut RenderContext, bounds: WidgetBounds, fg: Color, font_size: f32) {
        // Center text vertically in the bounds
        let text_y = bounds.y + (bounds.height - font_size) / 2.0;
        ctx.draw_text(&self.cached_text, bounds.x + 4.0, text_y, font_size, fg);
    }
}
