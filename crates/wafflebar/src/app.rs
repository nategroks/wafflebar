//! GTK4 + layer-shell host: builds the bar, instantiates modules, renders their `View`s, and
//! drives the event loop (dwl backend fd + timers) on GLib's main loop.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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
/// `config_path` (when present) is watched for live reload.
pub fn build_bars(
    app: &Application,
    config: &Config,
    engine: &GridEngine,
    config_path: Option<&std::path::Path>,
) {
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
        present_bar(app, None, config, engine, &backend, config_path);
        return;
    }

    let targets = select_monitors(&mons, &config.bar.monitor);
    info!(want = config.bar.monitor, selected = targets.len(), total = mons.len(), "monitor selection");
    for mon in targets {
        present_bar(app, Some(&mon), config, engine, &backend, config_path);
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

    let (host, timers, caps) =
        build_grid_and_host(&window, config, engine, &output_name, backend.clone());
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

    // Timers (clock ticks + memory/CPU pollers) are owned and reconciled by `TimerSet`, set up
    // inside `build_grid_and_host` and re-run on every structural reload — so there's no separate
    // timer-start loop here anymore.

    // Live config reload (F2 + F2b). Option edits re-`configure()` the changed slots in place;
    // structural edits (modules added/removed/reordered, a kind/cell/align change, or a [bar]/[grid]
    // change) rebuild every plugin via `rebuild` below. The host persists across a structural
    // rebuild, so its wiring (sinks, fd watch, backends holding `Weak<Host>`) stays valid.
    if let Some(path) = config_path {
        // Initial backend presence, captured now: a rebuild can't hot-start the audio/network/tray
        // backends (they hang off the host's immutable sinks — that's F2c), so we warn instead when
        // an edit adds the first consumer of one that never started.
        let had_audio = host.subscribes(&Topic::Audio);
        let had_network = host.subscribes(&Topic::Network);
        let had_tray = host.subscribes(&Topic::Tray);

        let rebuild: Rc<dyn Fn(&Config)> = {
            let window = window.clone();
            let output = output_name.clone();
            let host = host.clone();
            let timers = timers.clone();
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
                timers.borrow_mut().reconcile(&host, memory_interval_secs(new_config));

                // Backends are not reconciled in F2b (see TimerSet docs): warn when an edit adds the
                // first consumer of one that isn't running. A removed consumer's backend just idles
                // (single instance — wasteful, not a leak).
                if host.subscribes(&Topic::Audio) && !had_audio {
                    warn!("structural reload: added the first volume module — restart to connect the audio backend (F2c hot-starts backends)");
                }
                if host.subscribes(&Topic::Network) && !had_network {
                    warn!("structural reload: added the first network module — restart to connect the network backend (F2c)");
                }
                if host.subscribes(&Topic::Tray) && !had_tray {
                    warn!("structural reload: added the first tray module — restart to start the tray backend (F2c)");
                }

                host.render_all();
                info!(modules = engine.placements.len(), "structural reload applied");
            })
        };
        crate::config_reload::watch_config(path.to_path_buf(), host.clone(), rebuild, config.clone());
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
) -> (Grid, Vec<PluginSlot>) {
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
        slots.push(PluginSlot {
            kind: placement.kind.clone(),
            module,
            container,
            last_view: None,
        });
    }

    (grid, slots)
}

/// Build the grid + module slots, attach to the window, wire backends, and return the host that
/// owns them along with the reconciled [`TimerSet`] and queried [`plugins::Caps`] (both reused by
/// the structural-reload path).
fn build_grid_and_host(
    window: &ApplicationWindow,
    config: &Config,
    engine: &GridEngine,
    output: &str,
    backend: Option<Rc<RefCell<DwlBackend>>>,
) -> (Rc<Host>, Rc<RefCell<TimerSet>>, plugins::Caps) {
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

    // Clock-tick + memory/CPU-poll timers are owned by the TimerSet and (re)created by reconcile,
    // which both this initial build and every structural reload call — so there's exactly one
    // start path and reload can't double-register a timer.
    let timers = Rc::new(RefCell::new(TimerSet::default()));
    timers.borrow_mut().reconcile(&host, memory_interval_secs(config));

    (host, timers, caps)
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

#[cfg(test)]
mod tests {
    use super::*;
    use wafflebar_core::{Launch, Position, TrayCommand, VolumeCommand, WmCommand};

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
            Position::Top,
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
