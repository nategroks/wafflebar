//! GTK4 + layer-shell front-end: turns a validated [`GridEngine`] into on-screen bars.
//!
//! One layer-shell surface is created per monitor (subject to the `bar.monitor` setting).
//! Module placement is driven entirely by the core grid engine — this file only translates
//! [`Placement`]s into `gtk::Grid` attachments and builds the per-module widgets.

use std::collections::HashSet;

use gtk4::gdk;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, CssProvider, Grid};
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use tracing::{debug, info, warn};
use wafflebar_core::{Align, Config, GridEngine, Position};

use crate::modules;

/// Built-in Nord theme used when the config doesn't point at a CSS file.
const DEFAULT_CSS: &str = include_str!("../../../themes/nord.css");

/// Build the bar(s) on the monitor(s) selected by `bar.monitor`.
pub fn build_bars(app: &Application, config: &Config, engine: &GridEngine) {
    let Some(display) = gdk::Display::default() else {
        warn!("no GDK display; cannot create bars");
        return;
    };

    load_css(&display, config.bar.theme.as_deref());

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
        present_bar(app, None, config, engine);
        return;
    }

    let targets = select_monitors(&mons, &config.bar.monitor);
    info!(
        want = config.bar.monitor,
        selected = targets.len(),
        total = mons.len(),
        "monitor selection"
    );
    for mon in targets {
        present_bar(app, Some(&mon), config, engine);
    }
}

/// Resolve `bar.monitor` to the monitor(s) the bar should appear on.
///
/// Accepts (case-insensitive): `all`; `left` / `center` / `right` / `primary` selected by
/// **layout x-position** (robust to NVIDIA renaming connectors across boots); or an exact
/// connector / model name. An unknown name falls back to the primary (center) monitor.
fn select_monitors(mons: &[gdk::Monitor], want: &str) -> Vec<gdk::Monitor> {
    let want = want.trim();
    if want.eq_ignore_ascii_case("all") {
        return mons.to_vec();
    }
    // Order monitors left→right by their layout x coordinate.
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
///
/// GTK4's `Grid` collapses columns that contain no children, which would wreck a fixed
/// N-column track (modules would bunch up instead of sitting at their column positions).
/// So we reserve every empty column with a hexpanding filler; combined with
/// `column-homogeneous`, that yields N equal columns spanning the full bar width.
fn build_grid(config: &Config, engine: &GridEngine) -> Grid {
    let grid = Grid::builder()
        .hexpand(true)
        .column_homogeneous(true)
        .build();
    grid.set_widget_name("grid");

    // Which columns are occupied by a module?
    let mut occupied: HashSet<u32> = HashSet::new();
    for p in &engine.placements {
        for col in p.col..p.col + p.colspan {
            occupied.insert(col);
        }
    }
    // Reserve the empty columns so the track keeps its shape and fills the width.
    for col in 0..engine.cols {
        if !occupied.contains(&col) {
            let filler = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
            filler.set_hexpand(true);
            grid.attach(&filler, col as i32, 0, 1, engine.rows as i32);
        }
    }

    for placement in &engine.placements {
        let mcfg = &config.modules[placement.index];
        let widget = modules::build(&placement.kind, mcfg);
        // Expand to fill the cell so the requested alignment positions the content
        // within the column rather than collapsing it to its natural width.
        widget.set_hexpand(true);
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
