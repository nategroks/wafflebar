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
use wafflebar_core::{
    Align, Config, Event, GridEngine, Launch, Position, Topic, TrayCommand, VolumeCommand,
    WindowManager, WmCommand,
};

use crate::event_loop;
use crate::plugins;
use crate::plugins::cpu::backend::{CpuBackend, ProcStatBackend};
use crate::plugins::memory::backend::{MemoryBackend, ProcMemBackend};
use crate::plugins::network::backend::{NetworkBackend, NmBackend};
use crate::plugins::statustray::backend::SniBackend;
use crate::plugins::volume::backend::PulseBackend;
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
        // Wake on the wl_display fd becoming readable (G_IO_IN) rather than polling: a truly idle
        // bar makes zero syscalls. `dispatch()` runs the unchanged read-guard dance. The source
        // lives for the process lifetime (like the timer it replaced); we keep the SourceId so a
        // future clean-shutdown path can `remove()` it before the wl_display is dropped.
        let fd = b.borrow().fd();
        let host_p = host.clone();
        let b_p = b.clone();
        let _fd_source = event_loop::add_fd_watch_local(fd, move || {
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

    // Backend capabilities (queried once) handed to every plugin at construction.
    // Queried once at backend init. If a future backend has dynamic capabilities, lift this to a
    // WmEvent::CapsChanged.
    let caps = plugins::Caps {
        show_desktop: backend
            .as_ref()
            .map(|b| b.borrow().supports_show_desktop())
            .unwrap_or(false),
    };

    // One host-owned container per placement; the module's View is rendered into it.
    let mut slots = Vec::with_capacity(engine.placements.len());
    for placement in &engine.placements {
        let mcfg = &config.modules[placement.index];
        let module = plugins::build(&placement.kind, output, mcfg, &caps);
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
            last_view: None,
        });
    }

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
    let needs_memory = subscribes(&Topic::Memory);
    let needs_cpu = subscribes(&Topic::Cpu);
    let needs_tray = subscribes(&Topic::Tray);
    let audio = if needs_audio {
        PulseBackend::new().map(|b| Rc::new(RefCell::new(b)))
    } else {
        None
    };
    let volume_sink: Box<dyn Fn(&VolumeCommand)> = match &audio {
        Some(b) => {
            let b = b.clone();
            Box::new(move |cmd| b.borrow().execute(cmd))
        }
        None => Box::new(|_| {}),
    };

    // SNI tray backend: created before the host (so tray_sink can hold it), started after (so its
    // emit can hold a Weak<Host>) — same shape as the audio backend.
    let tray = needs_tray.then(|| Rc::new(SniBackend::new()));
    let tray_sink: Box<dyn Fn(&TrayCommand)> = match &tray {
        Some(b) => {
            let b = b.clone();
            Box::new(move |cmd| match cmd {
                TrayCommand::Activate { key } => b.activate(key),
                TrayCommand::SecondaryActivate { key } => b.secondary_activate(key),
                TrayCommand::Scroll { key, delta, horizontal } => b.scroll(key, *delta, *horizontal),
                TrayCommand::MenuClick { key, id } => b.menu_click(key, *id),
            })
        }
        None => Box::new(|_| {}),
    };

    let host = Host::new(
        slots,
        command_sink,
        launch_sink,
        volume_sink,
        tray_sink,
        config.bar.position,
    );

    // Forward audio events into the host. `Weak` breaks the host → volume_sink → backend →
    // handler → host cycle.
    if let Some(b) = &audio {
        let host_weak = Rc::downgrade(&host);
        b.borrow().set_handler(move |ev| {
            if let Some(h) = host_weak.upgrade() {
                h.deliver_event(&Event::Volume(ev));
            }
        });
    }

    // Network backend: a zbus subscription driven as a future on *this* (main) GLib context, so
    // events arrive on the main thread. Started only if some plugin subscribes to Topic::Network.
    if needs_network {
        let host_weak = Rc::downgrade(&host);
        glib::spawn_future_local(NmBackend.run(Box::new(move |state| {
            if let Some(h) = host_weak.upgrade() {
                h.deliver_event(&Event::Network(state));
            }
        })));
    }

    // Start the tray backend now that the host exists (emit holds a Weak<Host>).
    if let Some(b) = &tray {
        let host_weak = Rc::downgrade(&host);
        b.start(Rc::new(move |items| {
            if let Some(h) = host_weak.upgrade() {
                h.deliver_event(&Event::Tray(items));
            }
        }));
    }

    // Memory backend: polls /proc/meminfo on a GLib timer at the configured interval (5s default).
    // The SourceId is dropped but the source persists (it owns the closure), like the fd watch.
    if needs_memory {
        let secs = config
            .modules
            .iter()
            .find(|m| m.kind == "memory")
            .and_then(|m| m.opt_i64("interval"))
            .filter(|n| *n > 0)
            .map(|n| n as u64)
            .unwrap_or(5);
        let host_weak = Rc::downgrade(&host);
        let _mem_source = ProcMemBackend.start(
            std::time::Duration::from_secs(secs),
            Box::new(move |state| {
                if let Some(h) = host_weak.upgrade() {
                    h.deliver_event(&Event::Memory(state));
                }
            }),
        );
    }

    // CPU backend: polls /proc/stat at 1 Hz, tracking per-tick deltas (first tick is silent).
    if needs_cpu {
        let host_weak = Rc::downgrade(&host);
        let _cpu_source = ProcStatBackend.start(Box::new(move |state| {
            if let Some(h) = host_weak.upgrade() {
                h.deliver_event(&Event::Cpu(state));
            }
        }));
    }
    host
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
