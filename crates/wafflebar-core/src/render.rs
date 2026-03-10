use anyhow::Result;
use cosmic_text::{Attrs, Buffer, FontSystem, Metrics, Shaping, SwashCache};
use thiserror::Error;

/// RGBA color.
#[derive(Debug, Clone, Copy)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Parse a hex color string like "#2e3440" or "#2e3440ff".
    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.strip_prefix('#')?;
        match s.len() {
            6 => {
                let r = u8::from_str_radix(&s[0..2], 16).ok()?;
                let g = u8::from_str_radix(&s[2..4], 16).ok()?;
                let b = u8::from_str_radix(&s[4..6], 16).ok()?;
                Some(Self::rgb(r, g, b))
            }
            8 => {
                let r = u8::from_str_radix(&s[0..2], 16).ok()?;
                let g = u8::from_str_radix(&s[2..4], 16).ok()?;
                let b = u8::from_str_radix(&s[4..6], 16).ok()?;
                let a = u8::from_str_radix(&s[6..8], 16).ok()?;
                Some(Self::rgba(r, g, b, a))
            }
            _ => None,
        }
    }

    fn to_tiny_skia(self) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(self.r, self.g, self.b, self.a)
    }
}

/// Axis-aligned rectangle.
#[derive(Debug, Clone, Copy)]
pub struct RenderRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("failed to create pixmap: {0}")]
    PixmapCreation(String),
    #[error("gpu error: {0}")]
    Gpu(String),
}

/// The renderer backend selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererBackend {
    Cpu,
    Gpu,
}

/// Unified rendering context that dispatches to CPU or GPU backend.
pub struct RenderContext {
    backend: RenderBackendState,
    font_system: FontSystem,
    swash_cache: SwashCache,
}

enum RenderBackendState {
    Cpu {
        pixmap: tiny_skia::Pixmap,
    },
    Gpu {
        #[allow(dead_code)]
        device: wgpu::Device,
        #[allow(dead_code)]
        queue: wgpu::Queue,
        width: u32,
        height: u32,
        /// CPU-side buffer for readback / compositing
        staging: Vec<u8>,
    },
}

/// Public rendering API — all drawing commands go through here.
/// The backend is selected at construction time.
pub struct Renderer;

impl Renderer {
    /// Create a CPU-backed render context.
    pub fn new_cpu(width: u32, height: u32) -> Result<RenderContext> {
        let pixmap = tiny_skia::Pixmap::new(width, height)
            .ok_or_else(|| RenderError::PixmapCreation("invalid dimensions".into()))?;
        Ok(RenderContext {
            backend: RenderBackendState::Cpu { pixmap },
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
        })
    }

    /// Create a GPU-backed render context using wgpu.
    pub fn new_gpu(width: u32, height: u32) -> Result<RenderContext> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .map_err(|e| RenderError::Gpu(format!("no suitable GPU adapter found: {e}")))?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("wafflebar"),
            ..Default::default()
        }))
        .map_err(|e| RenderError::Gpu(format!("device request failed: {e}")))?;

        let staging = vec![0u8; (width * height * 4) as usize];

        Ok(RenderContext {
            backend: RenderBackendState::Gpu {
                device,
                queue,
                width,
                height,
                staging,
            },
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
        })
    }

    /// Select backend based on string preference ("auto", "cpu", "gpu").
    pub fn create(backend: &str, width: u32, height: u32) -> Result<RenderContext> {
        match backend {
            "gpu" => Self::new_gpu(width, height),
            "cpu" => Self::new_cpu(width, height),
            _ => {
                // "auto": try GPU, fallback to CPU
                match Self::new_gpu(width, height) {
                    Ok(ctx) => {
                        tracing::info!("using GPU renderer");
                        Ok(ctx)
                    }
                    Err(e) => {
                        tracing::warn!("GPU renderer unavailable ({e}), falling back to CPU");
                        Self::new_cpu(width, height)
                    }
                }
            }
        }
    }
}

impl RenderContext {
    /// Get the current width.
    pub fn width(&self) -> u32 {
        match &self.backend {
            RenderBackendState::Cpu { pixmap } => pixmap.width(),
            RenderBackendState::Gpu { width, .. } => *width,
        }
    }

    /// Get the current height.
    pub fn height(&self) -> u32 {
        match &self.backend {
            RenderBackendState::Cpu { pixmap } => pixmap.height(),
            RenderBackendState::Gpu { height, .. } => *height,
        }
    }

    /// Resize the render target.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        match &mut self.backend {
            RenderBackendState::Cpu { pixmap } => {
                *pixmap = tiny_skia::Pixmap::new(width, height)
                    .ok_or_else(|| RenderError::PixmapCreation("invalid dimensions".into()))?;
            }
            RenderBackendState::Gpu {
                width: w,
                height: h,
                staging,
                ..
            } => {
                *w = width;
                *h = height;
                staging.resize((width * height * 4) as usize, 0);
            }
        }
        Ok(())
    }

    /// Clear the entire surface with a color.
    pub fn clear(&mut self, color: Color) {
        match &mut self.backend {
            RenderBackendState::Cpu { pixmap } => {
                pixmap.fill(color.to_tiny_skia());
            }
            RenderBackendState::Gpu { staging, .. } => {
                let a = color.a as f32 / 255.0;
                let pr = (color.r as f32 * a) as u8;
                let pg = (color.g as f32 * a) as u8;
                let pb = (color.b as f32 * a) as u8;
                for chunk in staging.chunks_exact_mut(4) {
                    chunk[0] = pr;
                    chunk[1] = pg;
                    chunk[2] = pb;
                    chunk[3] = color.a;
                }
            }
        }
    }

    /// Fill a rectangle with a solid color.
    pub fn fill_rect(&mut self, rect: RenderRect, color: Color) {
        match &mut self.backend {
            RenderBackendState::Cpu { pixmap } => {
                let mut paint = tiny_skia::Paint::default();
                paint.set_color(color.to_tiny_skia());
                paint.anti_alias = false;

                let ts_rect = tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.width, rect.height);
                if let Some(ts_rect) = ts_rect {
                    pixmap.fill_rect(ts_rect, &paint, tiny_skia::Transform::identity(), None);
                }
            }
            RenderBackendState::Gpu {
                staging,
                width,
                height,
                ..
            } => {
                let x0 = (rect.x as u32).min(*width);
                let y0 = (rect.y as u32).min(*height);
                let x1 = ((rect.x + rect.width) as u32).min(*width);
                let y1 = ((rect.y + rect.height) as u32).min(*height);
                let a = color.a as f32 / 255.0;
                let pr = (color.r as f32 * a) as u8;
                let pg = (color.g as f32 * a) as u8;
                let pb = (color.b as f32 * a) as u8;
                for y in y0..y1 {
                    for x in x0..x1 {
                        let idx = ((y * *width + x) * 4) as usize;
                        if idx + 3 < staging.len() {
                            staging[idx] = pr;
                            staging[idx + 1] = pg;
                            staging[idx + 2] = pb;
                            staging[idx + 3] = color.a;
                        }
                    }
                }
            }
        }
    }

    /// Draw text at the given position.
    pub fn draw_text(&mut self, text: &str, x: f32, y: f32, size: f32, color: Color) {
        let metrics = Metrics::new(size, size * 1.2);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        let w = self.width() as f32;
        let h = self.height() as f32;
        buffer.set_size(&mut self.font_system, Some(w), Some(h));
        buffer.set_text(
            &mut self.font_system,
            text,
            &Attrs::new(),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut self.font_system, false);

        let text_color = cosmic_text::Color::rgba(color.r, color.g, color.b, color.a);

        match &mut self.backend {
            RenderBackendState::Cpu { pixmap } => {
                let pw = pixmap.width();
                let ph = pixmap.height();
                let data = pixmap.data_mut();
                buffer.draw(
                    &mut self.font_system,
                    &mut self.swash_cache,
                    text_color,
                    |gx, gy, _w, _h, gcolor| {
                        let px = gx + x as i32;
                        let py = gy + y as i32;
                        if px >= 0 && py >= 0 && (px as u32) < pw && (py as u32) < ph {
                            let alpha = gcolor.a() as f32 / 255.0;
                            if alpha > 0.0 {
                                let idx = ((py as u32 * pw + px as u32) * 4) as usize;
                                if idx + 3 < data.len() {
                                    blend_pixel(data, idx, gcolor, alpha);
                                }
                            }
                        }
                    },
                );
            }
            RenderBackendState::Gpu {
                staging,
                width,
                height,
                ..
            } => {
                let w = *width;
                let h = *height;
                buffer.draw(
                    &mut self.font_system,
                    &mut self.swash_cache,
                    text_color,
                    |gx, gy, _w, _h, gcolor| {
                        let px = gx + x as i32;
                        let py = gy + y as i32;
                        if px >= 0 && py >= 0 && (px as u32) < w && (py as u32) < h {
                            let alpha = gcolor.a() as f32 / 255.0;
                            if alpha > 0.0 {
                                let idx = ((py as u32 * w + px as u32) * 4) as usize;
                                if idx + 3 < staging.len() {
                                    blend_pixel(staging, idx, gcolor, alpha);
                                }
                            }
                        }
                    },
                );
            }
        }
    }

    /// Draw a pre-rasterized RGBA image at the given position and size.
    pub fn draw_image(&mut self, data: &[u8], img_width: u32, img_height: u32, dest: RenderRect) {
        let target_w = self.width();
        let target_h = self.height();
        let dx = dest.x as u32;
        let dy = dest.y as u32;
        let dw = dest.width as u32;
        let dh = dest.height as u32;

        let buf = self.pixel_data_mut();

        for py in 0..dh.min(img_height) {
            for px in 0..dw.min(img_width) {
                let tx = dx + px;
                let ty = dy + py;
                if tx < target_w && ty < target_h {
                    let src_idx = ((py * img_width + px) * 4) as usize;
                    let dst_idx = ((ty * target_w + tx) * 4) as usize;
                    if src_idx + 3 < data.len() && dst_idx + 3 < buf.len() {
                        let sa = data[src_idx + 3] as f32 / 255.0;
                        if sa > 0.99 {
                            buf[dst_idx..dst_idx + 4].copy_from_slice(&data[src_idx..src_idx + 4]);
                        } else if sa > 0.0 {
                            let da = buf[dst_idx + 3] as f32 / 255.0;
                            let out_a = sa + da * (1.0 - sa);
                            if out_a > 0.0 {
                                for c in 0..3 {
                                    buf[dst_idx + c] = ((data[src_idx + c] as f32 * sa
                                        + buf[dst_idx + c] as f32 * da * (1.0 - sa))
                                        / out_a)
                                        as u8;
                                }
                                buf[dst_idx + 3] = (out_a * 255.0) as u8;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Get the raw pixel data (RGBA, premultiplied for CPU path).
    pub fn pixel_data(&self) -> &[u8] {
        match &self.backend {
            RenderBackendState::Cpu { pixmap } => pixmap.data(),
            RenderBackendState::Gpu { staging, .. } => staging,
        }
    }

    /// Get mutable raw pixel data.
    fn pixel_data_mut(&mut self) -> &mut [u8] {
        match &mut self.backend {
            RenderBackendState::Cpu { pixmap } => pixmap.data_mut(),
            RenderBackendState::Gpu { staging, .. } => staging,
        }
    }

    /// Which backend is active?
    pub fn backend(&self) -> RendererBackend {
        match &self.backend {
            RenderBackendState::Cpu { .. } => RendererBackend::Cpu,
            RenderBackendState::Gpu { .. } => RendererBackend::Gpu,
        }
    }
}

/// Alpha-blend a glyph pixel onto the destination buffer.
fn blend_pixel(buf: &mut [u8], idx: usize, gcolor: cosmic_text::Color, alpha: f32) {
    let src_r = gcolor.r() as f32 * alpha;
    let src_g = gcolor.g() as f32 * alpha;
    let src_b = gcolor.b() as f32 * alpha;
    let dst_a = buf[idx + 3] as f32 / 255.0;
    let out_a = alpha + dst_a * (1.0 - alpha);
    if out_a > 0.0 {
        buf[idx] = ((src_r + buf[idx] as f32 * dst_a * (1.0 - alpha)) / out_a) as u8;
        buf[idx + 1] = ((src_g + buf[idx + 1] as f32 * dst_a * (1.0 - alpha)) / out_a) as u8;
        buf[idx + 2] = ((src_b + buf[idx + 2] as f32 * dst_a * (1.0 - alpha)) / out_a) as u8;
        buf[idx + 3] = (out_a * 255.0) as u8;
    }
}
