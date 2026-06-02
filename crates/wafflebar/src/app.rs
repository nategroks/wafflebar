//! GTK4 + layer-shell host: builds the bar, instantiates modules, renders their `View`s, and
//! drives the event loop (dwl backend fd + timers) on GLib's main loop.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, CenterBox, Grid, Orientation};
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use tracing::{debug, info, warn};
use wafflebar_core::{
    Align, BluetoothCommand, Config, Event, GridEngine, Launch, Layout, Position, Topic,
    TrayCommand, VolumeCommand, WmCommand,
};

use crate::event_loop;
use crate::plugins;
use crate::plugins::cpu::backend::{CpuBackend, ProcStatBackend};
use crate::plugins::memory::backend::{MemoryBackend, ProcMemBackend};
use crate::plugins::bluetooth::backend::BtBackend;
use crate::plugins::network::backend::{NetworkBackend, NmBackend};
use crate::plugins::statustray::backend::SniBackend;
use crate::plugins::volume::backend::PulseBackend;
use crate::render::{Host, PluginSlot};
use crate::feeds::someblocks::SomeblocksIntake;
use crate::wm::{connect_backend, BackendSelect, WmConnection};
use wafflebar_core::FeedSocketConfig;

/// Build the bar(s) on the selected monitor(s), wiring each to the shared dwl backend.
/// `config_path` (when present) is watched for live reload.
pub fn build_bars(
    app: &Application,
    config: &Config,
    engine: &GridEngine,
    config_path: Option<&std::path::Path>,
    replace_notifications: bool,
    backend_select: BackendSelect,
    feed_socket: FeedSocketConfig,
) {
    let Some(display) = gdk::Display::default() else {
        warn!("no GDK display; cannot create bars");
        return;
    };
    crate::theme::install(&display, config_path);
    register_bundled_icons(&display);

    // Notifications (G) — session-wide, started once per process (not per monitor). The server owns
    // org.freedesktop.Notifications; the stack renders incoming notifications as top-right popups.
    let notify_server = crate::notify::NotifyServer::new();
    let notify_stack = crate::notify_ui::NotificationStack::new(app, notify_server.clone());
    notify_server.start(replace_notifications, move |op| match op {
        crate::notify::ServerOp::Post(n) => {
            crate::notify_ui::NotificationStack::post(&notify_stack, n)
        }
        crate::notify::ServerOp::Close(id) => {
            crate::notify_ui::NotificationStack::close_external(&notify_stack, id)
        }
    });

    // One compositor connection, shared across bars (dwl today; sway in PR-B). `None` on an
    // unsupported session — the bar still runs, WM-driven modules just stay empty.
    let backend = connect_backend(backend_select);

    // One someblocks intake (NATEWM_MODE channel 2), shared across bars. `None` on disabled
    // (no path) or refused bind (live owner / non-socket clobber); the bar still runs, the feed
    // plugins just stay empty. Lives behind Rc<RefCell<>> so each bar's dispatch callback can
    // mutably borrow it.
    //
    // **Two clocks (NATEWM_MODE flag 6, by construction).** The WM fd and the feed listener fd
    // each get their *own* `event_loop::add_fd_watch_local` source below in `present_bar`. Both
    // are GLib `g_unix_fd_add_full` sources at `G_PRIORITY_DEFAULT`; neither is tied to the
    // GTK render tick. Read-side back-pressure on the renderer cannot back-pressure either
    // producer — the reducer's coalesce-latest means the renderer reads the *current* snapshot
    // at its own pace and never queues work.
    let feed = match SomeblocksIntake::bind(&feed_socket, false) {
        Ok(Some(intake)) => Some(Rc::new(RefCell::new(intake))),
        Ok(None) => None,
        Err(e) => {
            warn!(error = %e, "feed disabled: someblocks bind failed");
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
        present_bar(app, None, config, engine, &backend, &feed, config_path);
        return;
    }

    let targets = select_monitors(&mons, &config.bar.monitor);
    info!(want = config.bar.monitor, selected = targets.len(), total = mons.len(), "monitor selection");
    for mon in targets {
        present_bar(app, Some(&mon), config, engine, &backend, &feed, config_path);
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
    backend: &Option<Rc<RefCell<dyn WmConnection>>>,
    feed: &Option<Rc<RefCell<SomeblocksIntake>>>,
    config_path: Option<&std::path::Path>,
) {
    let output_name = monitor
        .and_then(gdk::Monitor::connector)
        .map(|c| c.to_string())
        .unwrap_or_default();

    let window = ApplicationWindow::builder()
        .application(app)
        .default_height(config.bar.height as i32)
        .build();

    // Monitor width drives `length_percent` (a shorter-than-full bar); `None` (unknown monitor) →
    // apply_bar_layout falls back to full width.
    let monitor_width = monitor.map(|m| m.geometry().width());

    window.init_layer_shell();
    window.set_namespace(Some("wafflebar"));
    if let Some(mon) = monitor {
        window.set_monitor(Some(mon));
    }
    apply_bar_layout(&window, &config.bar, monitor_width); // sets the layer (Top/Bottom) too
    window.set_widget_name("wafflebar");

    // Right-click empty bar space → preferences window (F3). Clicks on plugin buttons are consumed
    // by their own handlers (launcher/tray menus); only unhandled right-clicks (fillers, gaps)
    // bubble up to this window-level gesture. Needs a config file to edit; no-op on defaults.
    if let Some(path) = config_path {
        let gesture = gtk4::GestureClick::new();
        gesture.set_button(gdk::BUTTON_SECONDARY);
        let path_c = path.to_path_buf();
        gesture.connect_pressed(move |_, _, _, _| crate::prefs::open(&path_c));
        window.add_controller(gesture);
    }

    let BuiltBar { host, timers, caps, backends } =
        build_grid_and_host(&window, config, engine, &output_name, backend.clone());
    debug!(monitor = output_name, height = config.bar.height, "bar created");
    window.present();

    // Initial render so the bar isn't blank before the first event/tick (clock shows now).
    host.render_all();

    // === Clock 1: WM fd. ===
    // Wake on the WM backend's readable fd, drain its dispatch(), deliver WmEvents to the host.
    // After every dispatch, check `closed()` — when dwl exits, the stdin backend flips this and
    // we route through the same shutdown path as SIGTERM/SIGINT (NATEWM_MODE flag 5).
    // NOT tied to the GTK render tick.
    if let Some(b) = backend {
        for ev in b.borrow().snapshot() {
            host.deliver_event(&Event::Wm(ev));
        }
        let fd = b.borrow().fd();
        let host_p = host.clone();
        let b_p = b.clone();
        let _fd_source = event_loop::add_fd_watch_local(fd, move || {
            for ev in b_p.borrow_mut().dispatch() {
                host_p.deliver_event(&Event::Wm(ev));
            }
            if b_p.borrow().closed() {
                tracing::info!(
                    "WM backend reported closed (compositor exited); requesting graceful shutdown"
                );
                crate::request_shutdown();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    // === Clock 2: someblocks feed fd. ===
    // Independent of the WM watch and the GTK render tick. The listener fd becomes readable when
    // a producer connects OR an already-connected producer writes; dispatch() handles both, and
    // the reducer's coalesce-latest guarantees ≤1 FeedEvent::Frame per wake. If the renderer is
    // slow, the producer is *not* back-pressured — the reducer overwrites in place. "Never
    // blocks dwl"-style guarantee, by construction.
    if let Some(f) = feed {
        let fd = f.borrow().fd();
        let host_p = host.clone();
        let f_p = f.clone();
        let _fd_source = event_loop::add_fd_watch_local(fd, move || {
            for ev in f_p.borrow_mut().dispatch() {
                host_p.deliver_event(&Event::Feed(ev));
            }
            if crate::shutdown_requested() {
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    // === Shutdown observer. ===
    // Polls the static SHUTDOWN_REQUESTED flag (set by signal handlers or the WM EOF observer
    // in the Clock-1 callback above). When the flag flips, we run the *one* idempotent
    // `SomeblocksIntake::cleanup()` explicitly — same function the `Drop` impl calls — so all
    // exit paths converge on a single unlink site. NATEWM_MODE flag 5 (amended): SIGTERM, EOF,
    // and Drop all route through this one cleanup function. A session restart cannot reach the
    // stale-socket branch of `SomeblocksIntake::bind`.
    {
        let app_p = app.clone();
        let feed_for_cleanup = feed.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            if !crate::shutdown_requested() {
                return glib::ControlFlow::Continue;
            }
            tracing::info!("shutdown observed; running socket cleanup + app.quit()");
            if let Some(f) = &feed_for_cleanup {
                f.borrow_mut().cleanup();
            }
            app_p.quit();
            glib::ControlFlow::Break
        });
    }

    // Timers (clock ticks + memory/CPU pollers) are owned and reconciled by `TimerSet`, set up
    // inside `build_grid_and_host` and re-run on every structural reload — so there's no separate
    // timer-start loop here anymore.

    // Live config reload (F2 + F2b). Option edits re-`configure()` the changed slots in place;
    // structural edits (modules added/removed/reordered, a kind/cell/align change, or a [bar]/[grid]
    // change) rebuild every plugin via `rebuild` below. The host persists across a structural
    // rebuild, so its wiring (sinks, fd watch, backends holding `Weak<Host>`) stays valid.
    if let Some(path) = config_path {
        let rebuild: Rc<dyn Fn(&Config)> = {
            let window = window.clone();
            let output = output_name.clone();
            let host = host.clone();
            let timers = timers.clone();
            let backends = backends.clone();
            // monitor_width is Copy — the move closure captures it directly.
            Rc::new(move |new_config: &Config| {
                let engine = match GridEngine::build(new_config) {
                    Ok(e) => e,
                    Err(e) => {
                        warn!(error = %e, "structural reload: invalid layout; keeping current bar");
                        return;
                    }
                };
                // Structural changes rebuild every plugin (tear down all, reconstruct from the new
                // config) rather than diffing — simple and correct, and structural edits are rare
                // enough that the rebuild cost is irrelevant. See docs/UPSTREAM.md (F2b).
                let (grid, slots) = populate_grid(new_config, &engine, &output, &caps);
                host.replace_slots(slots); // tears down outgoing plugins (teardown hook)
                window.set_child(Some(&grid)); // drops the old grid and its containers
                host.attach_volume_mixers(); // reattach to the fresh volume containers
                timers.borrow_mut().reconcile(&host, memory_interval_secs(new_config));

                // F2c: reconcile the backend-class backends (add-first starts, remove-last stops —
                // except tray, start-once-keep) and re-anchor the bar live for a `[bar]` reposition.
                reconcile_backends(
                    &backends,
                    &host,
                    host.subscribes(&Topic::Audio),
                    host.subscribes(&Topic::Network),
                    host.subscribes(&Topic::Tray),
                    host.subscribes(&Topic::Bluetooth),
                );
                apply_bar_layout(&window, &new_config.bar, monitor_width);
                host.set_position(new_config.bar.position);
                host.set_icon_size(new_config.bar.effective_icon_size());

                host.render_all();
                info!(modules = engine.placements.len(), "structural reload applied");
            })
        };
        crate::config_reload::watch_config(path.to_path_buf(), host.clone(), rebuild, config.clone());
    }
}

/// Register wafflebar's bundled icon search paths so the `wb-*` glyphs (Lucide, recolored as
/// symbolic) resolve regardless of the user's system icon theme. Unique `wb-*` names mean no
/// priority fight — real app icons still come from the system theme. Covers the dev checkout and
/// standard install dirs; `add_search_path` is harmless for dirs that don't exist.
fn register_bundled_icons(display: &gdk::Display) {
    let theme = gtk4::IconTheme::for_display(display);
    theme.add_search_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/icons"));
    if let Some(home) = std::env::var_os("HOME") {
        theme.add_search_path(std::path::Path::new(&home).join(".local/share/wafflebar/icons"));
    }
    theme.add_search_path("/usr/share/wafflebar/icons");
    theme.add_search_path("/usr/local/share/wafflebar/icons");
}

/// Attach a calendar popover to a clock slot's **container** (not its time label). The container
/// persists across the per-minute re-render, so the popover — and an open calendar — survive ticks;
/// only the inner label is reconciled. `show_calendar` (default on) gates it; `show_week_numbers`
/// toggles the week column. No-op for non-clock modules. (Host-rendered like the apps menu.)
fn attach_clock_calendar(
    container: &gtk4::Box,
    mcfg: &wafflebar_core::ModuleConfig,
    position: Position,
) {
    if mcfg.kind != "clock" || !mcfg.opt_bool("show_calendar").unwrap_or(true) {
        return;
    }
    let cal = gtk4::Calendar::new();
    cal.set_show_week_numbers(mcfg.opt_bool("show_week_numbers").unwrap_or(false));
    let popover = gtk4::Popover::new();
    popover.set_child(Some(&cal));
    popover.set_autohide(true);
    popover.set_position(match position {
        Position::Top => gtk4::PositionType::Bottom, // top bar → open downward
        Position::Bottom => gtk4::PositionType::Top,
    });
    popover.set_parent(container);
    let gesture = gtk4::GestureClick::new();
    gesture.set_button(gdk::BUTTON_PRIMARY);
    gesture.connect_released(move |_, _, _, _| popover.popup());
    container.add_controller(gesture);
}

/// Apply the `[bar]` layer-shell layout (edge anchors + exclusive zone + height). gtk4-layer-shell
/// reconfigures a *mapped* surface, so this works live for a reposition (F2c) — no window recreate.
fn apply_bar_layout(
    window: &ApplicationWindow,
    bar: &wafflebar_core::BarConfig,
    monitor_width: Option<i32>,
) {
    use wafflebar_core::Alignment;

    // Layer: below normal windows (Bottom) or above (Top, default).
    window.set_layer(if bar.keep_below { Layer::Bottom } else { Layer::Top });

    // Vertical edge.
    let top = bar.position == Position::Top;
    window.set_anchor(Edge::Top, top);
    window.set_anchor(Edge::Bottom, !top);

    // Horizontal extent. Full width = anchor both edges (compositor sizes it). A shorter bar anchors
    // to one side (or neither, for centered) and is sized by its own width request. Falls back to
    // full width if we don't know the monitor width.
    let pct = bar.length_percent.clamp(1, 100);
    match monitor_width {
        Some(w) if pct < 100 => {
            window.set_anchor(Edge::Left, bar.alignment == Alignment::Start);
            window.set_anchor(Edge::Right, bar.alignment == Alignment::End);
            let width = (i64::from(w) * i64::from(pct) / 100).max(1) as i32;
            window.set_size_request(width, bar.height as i32);
            window.set_default_width(width);
        }
        _ => {
            window.set_anchor(Edge::Left, true);
            window.set_anchor(Edge::Right, true);
            window.set_size_request(-1, bar.height as i32);
        }
    }

    window.set_default_height(bar.height as i32);

    // Reserve screen space (strut) so tiled windows avoid the bar, or not.
    if bar.reserve_space {
        window.auto_exclusive_zone_enable();
    } else {
        window.set_exclusive_zone(0);
    }
}

/// Build the grid widget and one host-owned container per placement, instantiating each module's
/// reducer. Shared by initial bring-up and structural reload (F2b) so both paths produce an
/// identical layout. Pure widget/plugin construction — no backend or timer wiring (the caller owns
/// that), which is what lets the rebuild path reuse it.
fn populate_grid(
    config: &Config,
    engine: &GridEngine,
    output: &str,
    caps: &plugins::Caps,
) -> (gtk4::Widget, Vec<PluginSlot>) {
    // Pack is the default and the single-row case; an explicit grid or any multi-row track keeps the
    // homogeneous-column layout (the grid model, retained as opt-in / for multi-row bars).
    if config.bar.layout == Layout::Pack && engine.rows <= 1 {
        populate_pack(config, engine, output, caps)
    } else {
        populate_grid_inner(config, engine, output, caps)
    }
}

/// xfce4-panel-style packing: one `gtk4::CenterBox` whose start/center/end groups collect modules by
/// [`Align`]. Modules are content-sized (no `hexpand`); CenterBox holds the start group flush-left,
/// the end group flush-right, the center group centered, and absorbs the slack between them — so a
/// long label grows its group inward instead of overflowing the bar. `cell`/`colspan` are ignored.
fn populate_pack(
    config: &Config,
    engine: &GridEngine,
    output: &str,
    caps: &plugins::Caps,
) -> (gtk4::Widget, Vec<PluginSlot>) {
    let root = CenterBox::new();
    root.set_hexpand(true);
    root.set_widget_name("grid"); // keep the `#grid` CSS selector stable across both layouts

    let spacing = config.bar.spacing as i32;
    let start = gtk4::Box::new(Orientation::Horizontal, spacing);
    let center = gtk4::Box::new(Orientation::Horizontal, spacing);
    let end = gtk4::Box::new(Orientation::Horizontal, spacing);
    start.set_halign(gtk4::Align::Start);
    center.set_halign(gtk4::Align::Center);
    end.set_halign(gtk4::Align::End);

    // `engine.placements` is in declaration order; bucket by align so order is preserved within each
    // group. `Fill` has no packed meaning, so it groups with `Start`.
    let mut slots = Vec::with_capacity(engine.placements.len());
    for placement in &engine.placements {
        let mcfg = &config.modules[placement.index];
        let module = plugins::build(&placement.kind, output, mcfg, caps);
        let container = gtk4::Box::new(Orientation::Horizontal, 0);
        container.set_valign(gtk4::Align::Center);
        let group = match placement.align {
            Align::Center => &center,
            Align::End => &end,
            Align::Start | Align::Fill => &start,
        };
        group.append(&container);
        attach_clock_calendar(&container, mcfg, config.bar.position);
        slots.push(PluginSlot {
            kind: placement.kind.clone(),
            module,
            container,
            last_view: None,
        });
    }

    root.set_start_widget(Some(&start));
    root.set_center_widget(Some(&center));
    root.set_end_widget(Some(&end));
    (root.upcast(), slots)
}

/// The grid layout: a homogeneous `rows × columns` track with per-module `cell` placement and
/// hexpanding fillers for empty columns. Used for `layout = "grid"` and any multi-row config.
fn populate_grid_inner(
    config: &Config,
    engine: &GridEngine,
    output: &str,
    caps: &plugins::Caps,
) -> (gtk4::Widget, Vec<PluginSlot>) {
    let grid = Grid::builder()
        .hexpand(true)
        .column_homogeneous(true)
        .column_spacing(config.bar.spacing as i32)
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
        let module = plugins::build(&placement.kind, output, mcfg, caps);
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
        attach_clock_calendar(&container, mcfg, config.bar.position);
        slots.push(PluginSlot {
            kind: placement.kind.clone(),
            module,
            container,
            last_view: None,
        });
    }

    (grid.upcast(), slots)
}

/// Build the grid + module slots, attach to the window, wire backends, and return the host that
/// owns them along with the reconciled [`TimerSet`] and queried [`plugins::Caps`] (both reused by
/// the structural-reload path).
fn build_grid_and_host(
    window: &ApplicationWindow,
    config: &Config,
    engine: &GridEngine,
    output: &str,
    backend: Option<Rc<RefCell<dyn WmConnection>>>,
) -> BuiltBar {
    // Backend capabilities, queried once at init and handed to every plugin at construction. If a
    // future backend gains dynamic capabilities, lift this to a WmEvent::CapsChanged.
    let caps = plugins::Caps {
        show_desktop: backend
            .as_ref()
            .map(|b| b.borrow().supports_show_desktop())
            .unwrap_or(false),
    };

    let (grid, slots) = populate_grid(config, engine, output, &caps);
    window.set_child(Some(&grid));

    // Commands flow to the active backend's execute (no-op without a backend).
    let command_sink: Box<dyn Fn(&WmCommand)> = match backend {
        Some(b) => Box::new(move |cmd| b.borrow_mut().execute(cmd)),
        None => Box::new(|_| {}),
    };
    // Launch intents flow to the shell executor (DBus activation, falling back to spawn).
    let executor = crate::shell::executor::Executor::real();
    let launch_sink: Box<dyn Fn(&Launch)> = Box::new(move |intent| executor.execute(intent));

    // Start the audio backend only if some plugin subscribes to it. Held alive by `volume_sink`
    // (captured into the Host), so it lives as long as the bar.
    let subscribes = |topic: &Topic| slots.iter().any(|s| s.module.subscribe().contains(topic));
    let needs_audio = subscribes(&Topic::Audio);
    let needs_network = subscribes(&Topic::Network);
    let needs_tray = subscribes(&Topic::Tray);
    let needs_bluetooth = subscribes(&Topic::Bluetooth);
    // The applications menu has no event topic; key off the placed module kind.
    let needs_appmenu = slots.iter().any(|s| s.kind == "appmenu");
    // Backend-class plugins (volume/network/tray) are reconciled through a host-owned `Backends`
    // slot (F2c): the sinks route through whatever's currently in it, so a backend can be started or
    // stopped on a structural reload. Same shape as `TimerSet` — indirection through a mutable slot.
    let backends = Rc::new(RefCell::new(Backends::default()));
    let volume_sink: Box<dyn Fn(&VolumeCommand)> = {
        let backends = backends.clone();
        Box::new(move |cmd| {
            if let Some(b) = backends.borrow().audio.as_ref() {
                b.borrow().execute(cmd);
            }
        })
    };
    let tray_sink: Box<dyn Fn(&TrayCommand)> = {
        let backends = backends.clone();
        Box::new(move |cmd| {
            if let Some(b) = backends.borrow().tray.as_ref() {
                match cmd {
                    TrayCommand::Activate { key } => b.activate(key),
                    TrayCommand::SecondaryActivate { key } => b.secondary_activate(key),
                    TrayCommand::Scroll { key, delta, horizontal } => {
                        b.scroll(key, *delta, *horizontal)
                    }
                    TrayCommand::MenuClick { key, id } => b.menu_click(key, *id),
                }
            }
        })
    };

    let bluetooth_sink: Box<dyn Fn(&BluetoothCommand)> = {
        let backends = backends.clone();
        Box::new(move |cmd| {
            if let Some(b) = backends.borrow().bluetooth.as_ref() {
                b.execute(cmd);
            }
        })
    };

    let host = Host::new(
        slots,
        command_sink,
        launch_sink,
        volume_sink,
        tray_sink,
        bluetooth_sink,
        config.bar.position,
        config.bar.effective_icon_size(),
    );

    // Start the backends the initial module set needs (re-run on every structural reload).
    reconcile_backends(&backends, &host, needs_audio, needs_network, needs_tray, needs_bluetooth);

    // Host-attached volume mixer popovers (needs the host + GTK, so it can't ride in populate).
    host.attach_volume_mixers();

    // Clock-tick + memory/CPU-poll timers are owned by the TimerSet and (re)created by reconcile,
    // which both this initial build and every structural reload call — so there's exactly one
    // start path and reload can't double-register a timer.
    let timers = Rc::new(RefCell::new(TimerSet::default()));
    timers.borrow_mut().reconcile(&host, memory_interval_secs(config));

    // Applications menu (E2): populate the host app cache + recents once, and watch the application
    // dirs so installs/uninstalls refresh the menu. Only when an appmenu module is placed.
    if needs_appmenu {
        {
            let state = host.menu();
            let mut m = state.borrow_mut();
            m.apps = wafflebar_core::list_applications();
            m.recents = wafflebar_core::Recents::load();
        }
        watch_app_dirs(&host);
    }

    BuiltBar { host, timers, caps, backends }
}

/// What `build_grid_and_host` hands back: the host plus the host-level resource owners the reload
/// path reconciles (timers, backends) and the queried caps reused by rebuilds.
struct BuiltBar {
    host: Rc<Host>,
    timers: Rc<RefCell<TimerSet>>,
    caps: plugins::Caps,
    backends: Rc<RefCell<Backends>>,
}

/// Host-owned backend-class backends (F2c), reconciled on structural reload so add-first/remove-last
/// of volume/network/tray applies live. The command sinks route through these slots; the delivery
/// handlers hold `Weak<Host>` (no cycle). Held in `Rc<RefCell<_>>`, like `TimerSet`.
#[derive(Default)]
struct Backends {
    audio: Option<Rc<RefCell<PulseBackend>>>,
    tray: Option<Rc<SniBackend>>,
    /// The network subscription future; `abort()` is its cancellation handle (drops the proxy).
    network: Option<glib::JoinHandle<()>>,
    /// The BlueZ backend (poll future + shared connection for commands); `stop()` aborts the loop.
    bluetooth: Option<Rc<BtBackend>>,
}

/// What reconciling one backend against the new module set requires.
#[derive(Debug, PartialEq, Eq)]
enum BackendAction {
    Start,
    Stop,
    None,
}

fn reconcile_action(want: bool, present: bool) -> BackendAction {
    match (want, present) {
        (true, false) => BackendAction::Start,
        (false, true) => BackendAction::Stop,
        _ => BackendAction::None,
    }
}

/// Start/stop the backend-class backends to match what the host's current slots need. Idempotent;
/// run at initial build and on every structural reload. Volume and network fully start/stop; **tray
/// is start-once-keep** — once started it persists (idles on remove-last) to avoid dropping the SNI
/// Watcher bus name, which external items observe as flicker (see docs/UPSTREAM.md).
fn reconcile_backends(
    backends: &Rc<RefCell<Backends>>,
    host: &Rc<Host>,
    want_audio: bool,
    want_network: bool,
    want_tray: bool,
    want_bluetooth: bool,
) {
    // Audio (libpulse): start connects + wires delivery; stop drops the backend (Drop disconnects
    // the context + releases the glib-mainloop integration).
    // Hoist the presence check into a `let` so the `backends.borrow()` temporary is dropped before
    // the arms — a `match` scrutinee's temporaries otherwise live through the whole match, colliding
    // with the arm's `borrow_mut()` ("RefCell already borrowed").
    let has_audio = backends.borrow().audio.is_some();
    match reconcile_action(want_audio, has_audio) {
        BackendAction::Start => {
            if let Some(b) = PulseBackend::new().map(|b| Rc::new(RefCell::new(b))) {
                let host_weak = Rc::downgrade(host);
                b.borrow().set_handler(move |ev| {
                    if let Some(h) = host_weak.upgrade() {
                        h.deliver_event(&Event::Volume(ev));
                    }
                });
                backends.borrow_mut().audio = Some(b);
            }
        }
        BackendAction::Stop => backends.borrow_mut().audio = None,
        BackendAction::None => {}
    }

    // Network (zbus subscription as a future): abort() is the cancellation handle — it drops the
    // future at its await point, releasing the proxy + connection; it doesn't block, so no hang.
    let has_network = backends.borrow().network.is_some();
    match reconcile_action(want_network, has_network) {
        BackendAction::Start => {
            let host_weak = Rc::downgrade(host);
            let handle = glib::spawn_future_local(NmBackend.run(Box::new(move |state| {
                if let Some(h) = host_weak.upgrade() {
                    h.deliver_event(&Event::Network(state));
                }
            })));
            backends.borrow_mut().network = Some(handle);
        }
        BackendAction::Stop => {
            if let Some(handle) = backends.borrow_mut().network.take() {
                handle.abort();
            }
        }
        BackendAction::None => {}
    }

    // Tray (SNI): start-once-keep. Starting acquires the Watcher bus name; dropping it would fire
    // NameOwnerChanged and make external items re-register (visible flicker), so we never stop it —
    // remove-last just idles it (harmless single instance). Re-adding reuses the live backend.
    let has_tray = backends.borrow().tray.is_some();
    if want_tray && !has_tray {
        let b = Rc::new(SniBackend::new());
        let host_weak = Rc::downgrade(host);
        b.start(Rc::new(move |items| {
            if let Some(h) = host_weak.upgrade() {
                h.deliver_event(&Event::Tray(items));
            }
        }));
        backends.borrow_mut().tray = Some(b);
    }

    // Bluetooth (BlueZ poll future): start/stop like network, but the backend is an Rc we keep (the
    // command sink calls execute() on it); stop() aborts its poll loop.
    let has_bluetooth = backends.borrow().bluetooth.is_some();
    match reconcile_action(want_bluetooth, has_bluetooth) {
        BackendAction::Start => {
            let b = BtBackend::new();
            let host_weak = Rc::downgrade(host);
            b.start(Rc::new(move |state| {
                if let Some(h) = host_weak.upgrade() {
                    h.deliver_event(&Event::Bluetooth(state));
                }
            }));
            backends.borrow_mut().bluetooth = Some(b);
        }
        BackendAction::Stop => {
            if let Some(b) = backends.borrow_mut().bluetooth.take() {
                b.stop();
            }
        }
        BackendAction::None => {}
    }
}

/// Watch the XDG `applications` dirs; on a change (debounced like the config watch — mass package
/// installs fire bursts), reparse the app list into the host cache and rebuild the menu. The
/// monitors live for the process (leaked like the fd/config watches).
fn watch_app_dirs(host: &Rc<Host>) {
    let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    for dir in wafflebar_core::application_dirs() {
        let monitor = match gtk4::gio::File::for_path(&dir)
            .monitor_directory(gtk4::gio::FileMonitorFlags::NONE, gtk4::gio::Cancellable::NONE)
        {
            Ok(m) => m,
            Err(_) => continue, // dir may not exist; skip it
        };
        let (host_weak, debounce) = (Rc::downgrade(host), debounce.clone());
        monitor.connect_changed(move |_, _, _, _| {
            if let Some(id) = debounce.borrow_mut().take() {
                id.remove();
            }
            let (host_weak, debounce_inner) = (host_weak.clone(), debounce.clone());
            let id = glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                *debounce_inner.borrow_mut() = None;
                if let Some(host) = host_weak.upgrade() {
                    host.menu().borrow_mut().apps = wafflebar_core::list_applications();
                    host.refresh_appmenu();
                }
                glib::ControlFlow::Break
            });
            *debounce.borrow_mut() = Some(id);
        });
        std::mem::forget(monitor);
    }
}

/// The memory module's configured poll interval in seconds (default 5). Read from config because
/// the memory poller's GLib timer is created with a fixed period.
fn memory_interval_secs(config: &Config) -> u64 {
    config
        .modules
        .iter()
        .find(|m| m.kind == "memory")
        .and_then(|m| m.opt_i64("interval"))
        .filter(|n| *n > 0)
        .map(|n| n as u64)
        .unwrap_or(5)
}

/// App-level ownership of the GLib timer `SourceId`s whose existence tracks which plugins are
/// present: the clock-tick intervals and the memory/CPU pollers. Reconciled on every structural
/// rebuild so add/remove/reorder/kind-change never leaves a stale timer running or double-registers
/// one. Timers are the *only* resource class that would leak on every rebuild if ignored — a
/// blindly re-added timer stacks — whereas backends, held as single instances by the host's sinks,
/// leak only if recreated, which F2b never does (backend reconciliation is F2c). See
/// docs/UPSTREAM.md (F2b).
#[derive(Default)]
struct TimerSet {
    /// `Topic::Timer` intervals (seconds) → the source driving a `Tick` at that period.
    clock: HashMap<u32, glib::SourceId>,
    /// /proc/meminfo poller, present iff some module subscribes `Topic::Memory`.
    memory: Option<glib::SourceId>,
    /// /proc/stat poller, present iff some module subscribes `Topic::Cpu`.
    cpu: Option<glib::SourceId>,
}

impl TimerSet {
    /// Diff the live timers against what the host's *current* slots need: remove what's gone, start
    /// what's new, keep the intersection. Idempotent (calling it unchanged is a no-op). Every timer
    /// closure captures `Weak<Host>`, never `Rc<Host>`: a timer must not pin the host alive, and a
    /// tick that fires after the slot is gone resolves to a dropped upgrade. See docs/UPSTREAM.md
    /// (timer closures owned alongside the host capture Weak).
    ///
    /// Note: when memory/CPU stay present across a rebuild the existing poller is kept as-is, so a
    /// changed `interval` only takes effect on restart — F2b reconciles timer *existence*, not
    /// period.
    fn reconcile(&mut self, host: &Rc<Host>, mem_secs: u64) {
        let intervals: HashSet<u32> = host.timer_intervals().into_iter().collect();
        let want_memory = host.subscribes(&Topic::Memory);
        let want_cpu = host.subscribes(&Topic::Cpu);
        self.reconcile_to(&intervals, want_memory, want_cpu, host, mem_secs);
    }

    /// The pure diff, split out so the leak-prone bookkeeping is testable without building real
    /// plugin slots (which would need a second GTK-init thread — forbidden, see the tests). `host`
    /// is used only to `downgrade` into the timer closures; the desired set comes from the args.
    fn reconcile_to(
        &mut self,
        intervals: &HashSet<u32>,
        want_memory: bool,
        want_cpu: bool,
        host: &Rc<Host>,
        mem_secs: u64,
    ) {
        // Clock intervals.
        let needed = intervals;
        let gone: Vec<u32> = self
            .clock
            .keys()
            .copied()
            .filter(|s| !needed.contains(s))
            .collect();
        for secs in gone {
            if let Some(id) = self.clock.remove(&secs) {
                id.remove();
            }
        }
        for &secs in needed {
            if self.clock.contains_key(&secs) {
                continue;
            }
            let host_weak = Rc::downgrade(host);
            let id = glib::timeout_add_seconds_local(secs, move || {
                if let Some(h) = host_weak.upgrade() {
                    h.deliver_event(&Event::Tick { secs });
                }
                glib::ControlFlow::Continue
            });
            self.clock.insert(secs, id);
        }

        // Memory poller: /proc/meminfo on a GLib timer at the configured interval.
        match (want_memory, self.memory.is_some()) {
            (true, false) => {
                let host_weak = Rc::downgrade(host);
                self.memory = Some(ProcMemBackend.start(
                    std::time::Duration::from_secs(mem_secs),
                    Box::new(move |state| {
                        if let Some(h) = host_weak.upgrade() {
                            h.deliver_event(&Event::Memory(state));
                        }
                    }),
                ));
            }
            (false, true) => {
                if let Some(id) = self.memory.take() {
                    id.remove();
                }
            }
            _ => {}
        }

        // CPU poller: /proc/stat at 1 Hz, tracking per-tick deltas (first tick is silent).
        match (want_cpu, self.cpu.is_some()) {
            (true, false) => {
                let host_weak = Rc::downgrade(host);
                self.cpu = Some(ProcStatBackend.start(Box::new(move |state| {
                    if let Some(h) = host_weak.upgrade() {
                        h.deliver_event(&Event::Cpu(state));
                    }
                })));
            }
            (false, true) => {
                if let Some(id) = self.cpu.take() {
                    id.remove();
                }
            }
            _ => {}
        }
    }
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


#[cfg(test)]
mod tests {
    use super::*;
    use wafflebar_core::{Launch, Position, TrayCommand, VolumeCommand, WmCommand};

    #[test]
    fn backend_reconcile_matrix() {
        // want / present → action. Symmetric for volume/network; tray ignores Stop (start-once-keep).
        assert_eq!(reconcile_action(true, false), BackendAction::Start);
        assert_eq!(reconcile_action(false, true), BackendAction::Stop);
        assert_eq!(reconcile_action(true, true), BackendAction::None);
        assert_eq!(reconcile_action(false, false), BackendAction::None);
    }

    /// A slot-less host: building real `PluginSlot`s would need `gtk4::Box`es, and creating GTK
    /// widgets here would init GTK on this test's thread — but libtest gives each test its own
    /// thread and `renderer_gtk_behaviors` already owns the one GTK-init thread (a second panics:
    /// "Attempted to initialize GTK from two different threads"). So this test stays GTK-free and
    /// drives `reconcile_to` directly. The empty host exists only to `downgrade` into the timer
    /// closures; building GLib timers needs a main context, not a GTK display.
    fn empty_host() -> Rc<Host> {
        Host::new(
            Vec::new(),
            Box::new(|_: &WmCommand| {}),
            Box::new(|_: &Launch| {}),
            Box::new(|_: &VolumeCommand| {}),
            Box::new(|_: &TrayCommand| {}),
            Box::new(|_: &wafflebar_core::BluetoothCommand| {}),
            Position::Top,
            18,
        )
    }

    fn intervals(secs: &[u32]) -> HashSet<u32> {
        secs.iter().copied().collect()
    }

    /// Timers are the only resource class that leaks on *every* structural rebuild if
    /// reconciliation is wrong (a blindly re-added timer stacks; backends, held as single instances,
    /// leak only if recreated, which F2b never does). So the timer diff gets the stress coverage,
    /// walking each structural edit type as a change in the desired set.
    #[test]
    fn timer_reconciliation() {
        let host = empty_host();
        let mut t = TimerSet::default();
        let none = intervals(&[]);
        let one = intervals(&[1]);

        // Empty bar → no timers.
        t.reconcile_to(&none, false, false, &host, 5);
        assert!(t.clock.is_empty() && t.memory.is_none() && t.cpu.is_none());

        // Add a memory module → its poller starts; clock/cpu still absent.
        t.reconcile_to(&none, true, false, &host, 5);
        assert!(t.memory.is_some(), "memory poller started on add");
        assert!(t.clock.is_empty() && t.cpu.is_none());

        // Remove it → poller torn down (SourceId removed, Option cleared).
        t.reconcile_to(&none, false, false, &host, 5);
        assert!(t.memory.is_none(), "memory poller removed on remove");

        // Add a clock → one interval-keyed tick timer.
        t.reconcile_to(&one, false, false, &host, 5);
        assert_eq!(t.clock.len(), 1, "one clock interval timer");

        // Change kind clock→cpu (slot's needs flip) → clock timer gone, cpu poller up.
        t.reconcile_to(&none, false, true, &host, 5);
        assert!(t.clock.is_empty(), "clock timer removed on kind change");
        assert!(t.cpu.is_some(), "cpu poller started on kind change");

        // Reorder doesn't change the desired set: reconcile is idempotent, all timers stay up with
        // no duplication.
        t.reconcile_to(&one, true, true, &host, 5);
        t.reconcile_to(&one, true, true, &host, 5); // reorder → identical needs
        assert_eq!(t.clock.len(), 1);
        assert!(t.memory.is_some() && t.cpu.is_some(), "all timers up, none duplicated");

        // Distinct intervals each get a timer; re-reconciling the same set adds nothing.
        let ten = intervals(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        t.reconcile_to(&ten, true, true, &host, 5);
        assert_eq!(t.clock.len(), 10);
        t.reconcile_to(&ten, true, true, &host, 5);
        assert_eq!(t.clock.len(), 10, "idempotent: no duplicate interval timers");

        // Leak stress: 10 add-all / remove-all cycles must return to the same resting state every
        // time, never accumulating sources.
        for _ in 0..10 {
            t.reconcile_to(&one, true, true, &host, 5);
            assert_eq!(t.clock.len(), 1);
            assert!(t.memory.is_some() && t.cpu.is_some());
            t.reconcile_to(&none, false, false, &host, 5);
            assert!(t.clock.is_empty() && t.memory.is_none() && t.cpu.is_none());
        }
    }
}
