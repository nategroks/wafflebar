use crate::config::PanelConfig;
use crate::render::{Color, RenderContext};
use crate::widget::{Widget, WidgetBounds};

/// Panel position on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelPosition {
    Top,
    Bottom,
}

impl PanelPosition {
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "bottom" => Self::Bottom,
            _ => Self::Top,
        }
    }
}

/// Segment alignment in the panel.
#[derive(Debug, Clone, Copy)]
pub enum SegmentAlign {
    Left,
    Center,
    Right,
}

/// A named segment containing widgets.
pub struct Segment {
    pub align: SegmentAlign,
    pub widgets: Vec<Box<dyn Widget>>,
}

/// Simple rectangle type for layout.
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The panel state: owns config, segments, and handles layout/render.
pub struct Panel {
    pub config: PanelConfig,
    pub segments: Vec<Segment>,
    pub width: u32,
    pub height: u32,
}

impl Panel {
    pub fn new(config: PanelConfig, segments: Vec<Segment>) -> Self {
        Self {
            height: config.panel.height,
            width: 0, // set by backend based on screen
            config,
            segments,
        }
    }

    /// Update all widgets. Returns true if any widget needs redraw.
    pub fn update(&mut self) -> bool {
        let mut dirty = false;
        for seg in &mut self.segments {
            for w in &mut seg.widgets {
                if w.update() {
                    dirty = true;
                }
            }
        }
        dirty
    }

    /// Render the entire panel into the given context.
    pub fn render(&self, ctx: &mut RenderContext) {
        let bg = Color::from_hex(&self.config.theme.background).unwrap_or(Color::rgb(46, 52, 64));
        let fg =
            Color::from_hex(&self.config.theme.foreground).unwrap_or(Color::rgb(236, 239, 244));
        let font_size = self.config.theme.font_size;

        // Clear background
        ctx.clear(bg);

        let panel_h = self.height;
        let panel_w = self.width;
        let padding = 8u32;

        for seg in &self.segments {
            // Calculate total width needed by this segment
            let total_width: u32 = seg
                .widgets
                .iter()
                .map(|w| w.desired_width(panel_h) + padding)
                .sum::<u32>()
                .saturating_sub(padding);

            // Calculate starting X based on alignment
            let mut x = match seg.align {
                SegmentAlign::Left => padding as f32,
                SegmentAlign::Center => (panel_w as f32 - total_width as f32) / 2.0,
                SegmentAlign::Right => panel_w as f32 - total_width as f32 - padding as f32,
            };

            for widget in &seg.widgets {
                let w = widget.desired_width(panel_h);
                let bounds = WidgetBounds {
                    x,
                    y: 0.0,
                    width: w as f32,
                    height: panel_h as f32,
                };
                widget.render(ctx, bounds, fg, font_size);
                x += w as f32 + padding as f32;
            }
        }
    }
}
