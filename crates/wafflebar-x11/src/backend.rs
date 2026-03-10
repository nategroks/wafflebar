//! X11 backend for wafflebar.
//!
//! Creates a dock-type window using EWMH properties, reserves screen space
//! via struts, and renders the panel using the chosen render backend.

use anyhow::{Context, Result};
use std::time::{Duration, Instant};
use wafflebar_core::panel::Panel;
use wafflebar_core::render::Renderer;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::COPY_DEPTH_FROM_PARENT;

/// X11 backend for wafflebar.
pub struct X11Backend;

impl X11Backend {
    /// Run the X11 panel event loop. This function blocks until the panel exits.
    pub fn run(mut panel: Panel) -> Result<()> {
        let (conn, screen_num) =
            RustConnection::connect(None).context("failed to connect to X11 display")?;

        let screen = &conn.setup().roots[screen_num];
        let screen_width = screen.width_in_pixels as u32;
        let screen_height = screen.height_in_pixels as u32;
        let root = screen.root;
        let depth = screen.root_depth;

        let panel_height = panel.height;
        let panel_width = screen_width;
        panel.width = panel_width;

        let is_top = panel.config.panel.position != "bottom";

        let panel_y: i16 = if is_top {
            0
        } else {
            (screen_height - panel_height) as i16
        };

        tracing::info!(
            "creating X11 dock window: {}x{} at y={}, screen={}x{}",
            panel_width,
            panel_height,
            panel_y,
            screen_width,
            screen_height
        );

        // Create the window
        let win = conn.generate_id()?;
        let values = CreateWindowAux::new()
            .background_pixel(screen.black_pixel)
            .event_mask(
                EventMask::EXPOSURE
                    | EventMask::STRUCTURE_NOTIFY
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::POINTER_MOTION
                    | EventMask::ENTER_WINDOW
                    | EventMask::LEAVE_WINDOW,
            )
            .override_redirect(0u32);

        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            win,
            root,
            0,
            panel_y,
            panel_width as u16,
            panel_height as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &values,
        )?;

        // Set EWMH properties
        Self::set_ewmh_properties(
            &conn,
            win,
            root,
            panel_width,
            panel_height,
            screen_height,
            is_top,
        )?;

        // Create a GC for drawing
        let gc = conn.generate_id()?;
        conn.create_gc(gc, win, &CreateGCAux::new())?;

        // Map the window
        conn.map_window(win)?;
        conn.flush()?;

        tracing::info!("X11 panel window mapped");

        // Create renderer
        let renderer_backend = &panel.config.theme.renderer;
        let mut render_ctx = Renderer::create(renderer_backend, panel_width, panel_height)
            .context("failed to create renderer")?;

        tracing::info!("renderer backend: {:?}", render_ctx.backend());

        // Initial render
        panel.update();
        panel.render(&mut render_ctx);
        Self::put_image(
            &conn,
            win,
            gc,
            &render_ctx,
            panel_width,
            panel_height,
            depth,
        )?;

        // Event loop
        let tick_interval = Duration::from_secs(1);
        let mut last_tick = Instant::now();
        let mut needs_redraw = false;

        loop {
            // Check for pending events with a short timeout
            let event = if let Some(event) = conn.poll_for_event()? {
                Some(event)
            } else {
                // No pending event; check if it's time for a tick
                let now = Instant::now();
                if now.duration_since(last_tick) >= tick_interval {
                    last_tick = now;
                    if panel.update() {
                        needs_redraw = true;
                    }
                    if needs_redraw {
                        panel.render(&mut render_ctx);
                        Self::put_image(
                            &conn,
                            win,
                            gc,
                            &render_ctx,
                            panel_width,
                            panel_height,
                            depth,
                        )?;
                        needs_redraw = false;
                    }
                }
                // Sleep briefly to avoid busy-waiting
                std::thread::sleep(Duration::from_millis(50));
                continue;
            };

            match event {
                Some(Event::Expose(ev)) => {
                    if ev.count == 0 {
                        // Full redraw
                        panel.render(&mut render_ctx);
                        Self::put_image(
                            &conn,
                            win,
                            gc,
                            &render_ctx,
                            panel_width,
                            panel_height,
                            depth,
                        )?;
                    }
                }
                Some(Event::ConfigureNotify(ev)) => {
                    let new_w = ev.width as u32;
                    let new_h = ev.height as u32;
                    if new_w != panel_width || new_h != panel_height {
                        tracing::info!("window resized to {}x{}", new_w, new_h);
                        // Panel dimensions are fixed by design, but handle gracefully
                    }
                }
                Some(Event::ButtonPress(_ev)) => {
                    // Future: dispatch click to widgets
                    tracing::debug!("button press at ({}, {})", _ev.event_x, _ev.event_y);
                }
                Some(Event::ClientMessage(ev)) => {
                    // Check for WM_DELETE_WINDOW
                    let wm_protocols = intern_atom(&conn, false, b"WM_PROTOCOLS")?.reply()?.atom;
                    let wm_delete = intern_atom(&conn, false, b"WM_DELETE_WINDOW")?
                        .reply()?
                        .atom;
                    if ev.type_ == wm_protocols && ev.data.as_data32()[0] == wm_delete {
                        tracing::info!("received WM_DELETE_WINDOW, exiting");
                        break;
                    }
                }
                Some(Event::DestroyNotify(_)) => {
                    tracing::info!("window destroyed, exiting");
                    break;
                }
                _ => {}
            }
        }

        conn.destroy_window(win)?;
        conn.flush()?;
        Ok(())
    }

    /// Set EWMH properties on the panel window to make it behave as a dock.
    fn set_ewmh_properties(
        conn: &RustConnection,
        win: Window,
        _root: Window,
        width: u32,
        height: u32,
        _screen_height: u32,
        is_top: bool,
    ) -> Result<()> {
        // _NET_WM_WINDOW_TYPE = _NET_WM_WINDOW_TYPE_DOCK
        let wm_type = intern_atom(conn, false, b"_NET_WM_WINDOW_TYPE")?
            .reply()?
            .atom;
        let wm_type_dock = intern_atom(conn, false, b"_NET_WM_WINDOW_TYPE_DOCK")?
            .reply()?
            .atom;
        conn.change_property32(
            PropMode::REPLACE,
            win,
            wm_type,
            AtomEnum::ATOM,
            &[wm_type_dock],
        )?;

        // _NET_WM_STATE = _NET_WM_STATE_STICKY | _NET_WM_STATE_ABOVE
        let wm_state = intern_atom(conn, false, b"_NET_WM_STATE")?.reply()?.atom;
        let sticky = intern_atom(conn, false, b"_NET_WM_STATE_STICKY")?
            .reply()?
            .atom;
        let above = intern_atom(conn, false, b"_NET_WM_STATE_ABOVE")?
            .reply()?
            .atom;
        conn.change_property32(
            PropMode::REPLACE,
            win,
            wm_state,
            AtomEnum::ATOM,
            &[sticky, above],
        )?;

        // _NET_WM_STRUT_PARTIAL: reserve screen space
        // Format: left, right, top, bottom, left_start_y, left_end_y,
        //         right_start_y, right_end_y, top_start_x, top_end_x,
        //         bottom_start_x, bottom_end_x
        let strut_partial = intern_atom(conn, false, b"_NET_WM_STRUT_PARTIAL")?
            .reply()?
            .atom;
        let strut_data: [u32; 12] = if is_top {
            [0, 0, height, 0, 0, 0, 0, 0, 0, width - 1, 0, 0]
        } else {
            [0, 0, 0, height, 0, 0, 0, 0, 0, 0, 0, width - 1]
        };
        conn.change_property32(
            PropMode::REPLACE,
            win,
            strut_partial,
            AtomEnum::CARDINAL,
            &strut_data,
        )?;

        // _NET_WM_STRUT (legacy, simpler version)
        let strut = intern_atom(conn, false, b"_NET_WM_STRUT")?.reply()?.atom;
        let strut_simple: [u32; 4] = if is_top {
            [0, 0, height, 0]
        } else {
            [0, 0, 0, height]
        };
        conn.change_property32(
            PropMode::REPLACE,
            win,
            strut,
            AtomEnum::CARDINAL,
            &strut_simple,
        )?;

        // _NET_WM_DESKTOP = 0xFFFFFFFF (all desktops)
        let wm_desktop = intern_atom(conn, false, b"_NET_WM_DESKTOP")?.reply()?.atom;
        conn.change_property32(
            PropMode::REPLACE,
            win,
            wm_desktop,
            AtomEnum::CARDINAL,
            &[0xFFFFFFFFu32],
        )?;

        // WM_NAME
        conn.change_property8(
            PropMode::REPLACE,
            win,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            b"wafflebar",
        )?;

        // _NET_WM_PID
        let wm_pid = intern_atom(conn, false, b"_NET_WM_PID")?.reply()?.atom;
        let pid = std::process::id();
        conn.change_property32(PropMode::REPLACE, win, wm_pid, AtomEnum::CARDINAL, &[pid])?;

        conn.flush()?;
        Ok(())
    }

    /// Blit the render context's pixel data to the X11 window via PutImage.
    fn put_image(
        conn: &RustConnection,
        win: Window,
        gc: Gcontext,
        render_ctx: &wafflebar_core::render::RenderContext,
        width: u32,
        height: u32,
        depth: u8,
    ) -> Result<()> {
        let pixel_data = render_ctx.pixel_data();

        // Convert RGBA to the format X11 expects (BGRA for 24/32-bit depth)
        let mut bgra = Vec::with_capacity(pixel_data.len());
        for chunk in pixel_data.chunks_exact(4) {
            bgra.push(chunk[2]); // B
            bgra.push(chunk[1]); // G
            bgra.push(chunk[0]); // R
            bgra.push(chunk[3]); // A
        }

        // PutImage with ZPixmap format
        // For large images, we may need to chunk the data to fit within
        // the X11 maximum request size
        let max_req_len = conn.maximum_request_bytes();
        let row_bytes = (width * 4) as usize;
        let header_size = 28; // PutImage request header
        let max_rows = (max_req_len - header_size) / row_bytes;

        let mut y_offset = 0u32;
        while y_offset < height {
            let rows = (height - y_offset).min(max_rows as u32);
            let start = (y_offset as usize) * row_bytes;
            let end = start + (rows as usize) * row_bytes;
            let chunk = &bgra[start..end];

            conn.put_image(
                ImageFormat::Z_PIXMAP,
                win,
                gc,
                width as u16,
                rows as u16,
                0,
                y_offset as i16,
                0,
                depth,
                chunk,
            )?;

            y_offset += rows;
        }

        conn.flush()?;
        Ok(())
    }
}
