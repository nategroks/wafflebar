//! GTK4 + layer-shell host: builds the bar, instantiates modules, renders their `View`s, and
//! drives the event loop (dwl backend fd + timers) on GLib's main loop.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, CssProvider, Grid, Orientation};
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use tracing::{debug, info, warn};
use wafflebar_core::{Align, Config, Event, GridEngine, Position, WindowManager, WmCommand};

use crate::plugins;
use crate::render::{Host, PluginSlot};
use crate::wm::DwlBackend;

/// Built-in Nord theme used when the config doesn't point at a CSS file.
const DEFAULT_CSS: &str = include_str!("../../../themes/nord.css");

/// Build the bar(s) on the selected monitor(s), wiring each to the shared dwl backend.
pub fn build_bars(app: &Application, config: &Config, engine: &GridEngine) {
    let Some(display) = gdk::Display::default() else {
        warn!("no GDK display; cannot create bars");
        return;
    };
    load_css(&display, config.bar.theme.as_deref());

    // One Wayland backend connection, shared across bars. None on non-dwl sessions (the bar
    // still runs; WM-driven modules just stay empty).
    let backend = match DwlBackend::connect() {
        Ok(b) => {
            info!("dwl backend connected");
            Some(Rc::new(RefCell::new(b)))
        }
        Err(e) => {
            warn!(error = %e, "dwl backend unavailable; tags/window will be empty");
            None
        }
    };

    let monitors = display.monitors();
    let mut mons: Vec<gdk::Monitor> = Vec::new();
    for i in 0..monitors.n_items() {
        if let Some(m) = monitors
            .item(i)
            .and_then(|o| o.downcast::<gdk::Monitor>().ok())
        {
            mons.push(m);
        }
    }
    if mons.is_empty() {
        warn!("no monitors reported; creating a single unanchored bar");
        present_bar(app, None, config, engine, &backend);
        return;
    }

    let targets = select_monitors(&mons, &config.bar.monitor);
    info!(want = config.bar.monitor, selected = targets.len(), total = mons.len(), "monitor selection");
    for mon in targets {
        present_bar(app, Some(&mon), config, engine, &backend);
    }
}

/// Resolve `bar.monitor` to the monitor(s) to use (by layout position — robust to NVIDIA
/// connector renaming — or by exact connector/model; unknown falls back to primary/center).
fn select_monitors(mons: &[gdk::Monitor], want: &str) -> Vec<gdk::Monitor> {
    let want = want.trim();
    if want.eq_ignore_ascii_case("all") {
        return mons.to_vec();
    }
    let mut by_x: Vec<gdk::Monitor> = mons.to_vec();
    by_x.sort_by_key(|m| m.geometry().x());
    let center = by_x.get(by_x.len() / 2).cloned();
    match want.to_ascii_lowercase().as_str() {
        "left" => by_x.first().cloned().into_iter().collect(),
        "right" => by_x.last().cloned().into_iter().collect(),
        "primary" | "center" => center.into_iter().collect(),
        _ => {
            let exact = mons.iter().find(|m| {
                m.connector().map(|c| c == want).unwrap_or(false)
                    || m.model().map(|c| c == want).unwrap_or(false)
            });
            match exact {
                Some(m) => vec![m.clone()],
                None => {
                    warn!(want, "no monitor matched; using primary (center)");
                    center.into_iter().collect()
                }
            }
        }
    }
}

fn present_bar(
    app: &Application,
    monitor: Option<&gdk::Monitor>,
    config: &Config,
    engine: &GridEngine,
    backend: &Option<Rc<RefCell<DwlBackend>>>,
) {
    let output_name = monitor
        .and_then(gdk::Monitor::connector)
        .map(|c| c.to_string())
        .unwrap_or_default();

    let window = ApplicationWindow::builder()
        .application(app)
        .default_height(config.bar.height as i32)
        .build();

    window.init_layer_shell();
    window.set_layer(Layer::Top);
    window.set_namespace(Some("wafflebar"));
    if let Some(mon) = monitor {
        window.set_monitor(Some(mon));
    }
    let top = config.bar.position == Position::Top;
    window.set_anchor(Edge::Left, true);
    window.set_anchor(Edge::Right, true);
    window.set_anchor(Edge::Top, top);
    window.set_anchor(Edge::Bottom, !top);
    window.auto_exclusive_zone_enable();
    window.set_widget_name("wafflebar");

    let host = build_grid_and_host(&window, config, engine, &output_name, backend.clone());
    debug!(monitor = output_name, height = config.bar.height, "bar created");
    window.present();

    // Initial render so the bar isn't blank before the first event/tick (clock shows now).
    host.render_all();

    // Feed the current WM snapshot, then drive live updates from the backend.
    if let Some(b) = backend {
        for ev in b.borrow().snapshot() {
            host.deliver_event(&Event::Wm(ev));
        }
        // v1 SHORTCUT: poll the backend at 20 Hz. glib 0.22 exposes no safe unix-fd watch
        // (`unix_fd_add_local`/`IOChannel` were dropped), so we can't wake on fd-readable here.
        // `dispatch()` is a cheap non-blocking drain (prepare_read + dispatch_pending), so idle
        // CPU stays ~0%. TODO: replace with true fd-readable integration via calloop or the
        // g_unix_fd_add ffi once we want sub-frame latency. `DwlBackend::fd()` is ready for it.
        let host_p = host.clone();
        let b_p = b.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            for ev in b_p.borrow_mut().dispatch() {
                host_p.deliver_event(&Event::Wm(ev));
            }
            glib::ControlFlow::Continue
        });
    }

    // One GLib timer per distinct subscribed interval; delivers a Tick to all modules.
    for secs in host.timer_intervals() {
        let host_t = host.clone();
        glib::timeout_add_seconds_local(secs, move || {
            host_t.deliver_event(&Event::Tick { secs });
            glib::ControlFlow::Continue
        });
    }
}

/// Build the grid + module slots, attach to the window, and return the host that owns them.
fn build_grid_and_host(
    window: &ApplicationWindow,
    config: &Config,
    engine: &GridEngine,
    output: &str,
    backend: Option<Rc<RefCell<DwlBackend>>>,
) -> Rc<Host> {
    let grid = Grid::builder()
        .hexpand(true)
        .column_homogeneous(true)
        .build();
    grid.set_widget_name("grid");

    // Reserve empty columns so the fixed track keeps its shape (GTK4 Grid collapses childless
    // columns); combined with column-homogeneous this fills the bar width evenly.
    let mut occupied: HashSet<u32> = HashSet::new();
    for p in &engine.placements {
        for col in p.col..p.col + p.colspan {
            occupied.insert(col);
        }
    }
    for col in 0..engine.cols {
        if !occupied.contains(&col) {
            let filler = gtk4::Box::new(Orientation::Horizontal, 0);
            filler.set_hexpand(true);
            grid.attach(&filler, col as i32, 0, 1, engine.rows as i32);
        }
    }

    // One host-owned container per placement; the module's View is rendered into it.
    let mut slots = Vec::with_capacity(engine.placements.len());
    for placement in &engine.placements {
        let mcfg = &config.modules[placement.index];
        let module = plugins::build(&placement.kind, output, mcfg);
        let container = gtk4::Box::new(Orientation::Horizontal, 0);
        container.set_hexpand(true);
        apply_align(&container, placement.align);
        grid.attach(
            &container,
            placement.col as i32,
            placement.row as i32,
            placement.colspan as i32,
            placement.rowspan as i32,
        );
        slots.push(PluginSlot {
            kind: placement.kind.clone(),
            module,
            container,
        });
    }

    window.set_child(Some(&grid));

    // Commands flow to the active backend's execute (no-op without a backend).
    let command_sink: Box<dyn Fn(&WmCommand)> = match backend {
        Some(b) => Box::new(move |cmd| b.borrow_mut().execute(cmd)),
        None => Box::new(|_| {}),
    };
    Host::new(slots, command_sink)
}

fn apply_align(widget: &impl IsA<gtk4::Widget>, align: Align) {
    let a = match align {
        Align::Start => gtk4::Align::Start,
        Align::Center => gtk4::Align::Center,
        Align::End => gtk4::Align::End,
        Align::Fill => gtk4::Align::Fill,
    };
    widget.set_halign(a);
    widget.set_valign(gtk4::Align::Center);
}

fn load_css(display: &gdk::Display, theme: Option<&str>) {
    let provider = CssProvider::new();
    match theme {
        Some(path) if std::path::Path::new(path).exists() => {
            provider.load_from_path(path);
            debug!(path, "loaded theme css");
        }
        Some(path) => {
            warn!(path, "theme css not found; using built-in Nord");
            provider.load_from_string(DEFAULT_CSS);
        }
        None => provider.load_from_string(DEFAULT_CSS),
    }
    gtk4::style_context_add_provider_for_display(
        display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
