//! GTK4 + layer-shell front-end: turns a validated [`GridEngine`] into on-screen bars.
//!
//! One layer-shell surface is created per monitor (subject to the `bar.monitor` setting).
//! Module placement is driven entirely by the core grid engine — this file only translates
//! [`Placement`]s into `gtk::Grid` attachments and builds the per-module widgets.

use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, CssProvider, Grid};
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use tracing::{debug, warn};
use wafflebar_core::{Align, Config, GridEngine, Position};

use crate::modules;

/// Built-in Nord theme used when the config doesn't point at a CSS file.
const DEFAULT_CSS: &str = include_str!("../../../themes/nord.css");

/// Build one bar per monitor and present them.
pub fn build_bars(app: &Application, config: &Config, engine: &GridEngine) {
    let Some(display) = gdk::Display::default() else {
        warn!("no GDK display; cannot create bars");
        return;
    };

    load_css(&display, config.bar.theme.as_deref());

    let monitors = display.monitors();
    let n = monitors.n_items();
    if n == 0 {
        warn!("no monitors reported; creating a single unanchored bar");
        present_bar(app, None, config, engine);
        return;
    }

    for i in 0..n {
        let Some(obj) = monitors.item(i) else { continue };
        let Ok(monitor) = obj.downcast::<gdk::Monitor>() else {
            continue;
        };
        if !monitor_selected(&monitor, &config.bar.monitor) {
            debug!(connector = ?monitor.connector(), "skipping monitor (not selected)");
            continue;
        }
        present_bar(app, Some(&monitor), config, engine);
    }
}

/// `bar.monitor` is `"all"`, or matches the monitor's connector name or model.
fn monitor_selected(monitor: &gdk::Monitor, want: &str) -> bool {
    if want.eq_ignore_ascii_case("all") {
        return true;
    }
    let matches = |s: Option<glib::GString>| s.map(|v| v == want).unwrap_or(false);
    matches(monitor.connector()) || matches(monitor.model())
}

fn present_bar(
    app: &Application,
    monitor: Option<&gdk::Monitor>,
    config: &Config,
    engine: &GridEngine,
) {
    let window = ApplicationWindow::builder()
        .application(app)
        .default_height(config.bar.height as i32)
        .build();

    // Layer-shell: dock the window to a screen edge as a panel.
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
    // Reserve space so tiled windows don't draw under the bar.
    window.auto_exclusive_zone_enable();

    window.set_widget_name("wafflebar");
    window.set_child(Some(&build_grid(config, engine)));
    debug!(
        monitor = ?monitor.and_then(gdk::Monitor::connector),
        height = config.bar.height,
        position = if top { "top" } else { "bottom" },
        "bar created"
    );
    window.present();
}

/// Translate the validated placements into a `gtk::Grid` of module widgets.
fn build_grid(config: &Config, engine: &GridEngine) -> Grid {
    let grid = Grid::builder()
        .hexpand(true)
        .column_homogeneous(true)
        .build();
    grid.set_widget_name("grid");

    for placement in &engine.placements {
        let mcfg = &config.modules[placement.index];
        let widget = modules::build(&placement.kind, mcfg);
        apply_align(&widget, placement.align);
        grid.attach(
            &widget,
            placement.col as i32,
            placement.row as i32,
            placement.colspan as i32,
            placement.rowspan as i32,
        );
    }
    grid
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
    if matches!(align, Align::Fill) {
        widget.set_hexpand(true);
    }
}

/// Load CSS: the user's theme file if set and readable, otherwise the built-in Nord theme.
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
