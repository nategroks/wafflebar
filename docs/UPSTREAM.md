# Upstream reference: xfce4-panel

wafflebar **mirrors the structure of [xfce4-panel](https://gitlab.xfce.org/xfce/xfce4-panel)**
(C / Meson / GTK, in development since 2003 — the most-deployed traditional desktop panel that
isn't tied to a full DE). When implementing a plugin or subsystem, **read the corresponding
xfce4-panel source first and mirror its *behavior*** — not its C/GObject idioms (`g_signal_connect`,
`XfconfChannel`, `GObject` boilerplate). The Rust equivalents (traits, channels, `serde`, the
`notify` crate, GTK4 CSS) are simpler.

Clone it shallow next to your work and keep it open:
```
git clone --depth=1 https://github.com/xfce-mirror/xfce4-panel /tmp/refs/xfce4-panel
```

## Structure mapping (xfce4-panel → wafflebar)

| xfce4-panel (C) | wafflebar (Rust) | status |
|---|---|---|
| `panel/` (window, positioning, struts, dialogs) | `src/app/` + `src/shell/window/` | partial |
| `libxfce4panel/` (plugin SDK, wrapper proto, widgets) | `wafflebar-core` (`Plugin`, `View`, `ActionId`) | done |
| `wrapper/` (out-of-process plugin host) | `src/wrapper/` | v2 stub (View boundary already serializable) |
| `plugins/pager/` (workspace switcher) | `src/plugins/dwl/tags.rs` (`tags`) | done |
| `plugins/tasklist/` | `src/plugins/tasklist.rs` | **this PR** |
| `plugins/statustray/` (SNI + XEmbed) | `src/plugins/statustray/` | designed (ARCHITECTURE.md), Phase D |
| `plugins/clock/` | `src/plugins/clock.rs` | done |
| `plugins/launcher/` (pinned apps) | `src/plugins/launcher/` | Phase B |
| `plugins/applicationsmenu/` (+ libgarcon) | `src/plugins/applicationsmenu/` | Phase E |
| `plugins/{separator,showdesktop,windowmenu,actions,directorymenu}/` | `src/plugins/<same>/` | later |
| Xfconf (config + live update) | `wafflebar-core::config` (TOML + `notify` watch) | partial |
| Panel/Add-Items/per-plugin dialogs (`.glade`) | `src/shell/preferences/` | Phase F |

**Not from xfce4-panel:** `src/wm/` — wafflebar abstracts the window manager (dwl/river/sway/…)
to be portable across tiling WMs, which is beyond xfce4-panel's scope (it sits directly on
X11/Wayland via WM hints).

## Per-plugin workflow
1. Read `plugins/<name>/` in the checkout.
2. Note its GTK widgets, D-Bus calls, file-format parsing, and config keys.
3. Mirror the *behavior* on our `Plugin` trait + `View` boundary.
4. v1 implements the core; defer the rest with `// TODO(<plugin>): mirror …:<function>` markers.

## Affordance discipline
Affordances are designed when the *shape* becomes clear, not when the first consumer arrives.
Exercising the affordance later validates the original design without forcing a substrate revision.
Track record: the cached `wl_seat` in M2's dwl backend (added before tasklist's activate/close needed
it), the `View::Popover` variant baked into the enum at M2 (no consumer then), and the seat/popover
both paying off later — launcher shipped right-click menus in B2b with no substrate change, and that
same launcher popover code retired Phase D's hardest risk (layer-shell + `gtk4::Popover` Wayland
compatibility) before D research even started.

## Reducer discipline
A plugin's `on_event` **must return `dirty=false` when its derived state is unchanged**, even if a
`WmEvent` arrived. Compositors re-emit state (dwl re-sends `Tags`/`Windows` on frames and unrelated
focus activity); dirtying on every arrival rebuilds widgets for no reason. The equality check
belongs at the reducer boundary so every future plugin inherits the protection without asking. The
keyed-diff reconcile (C2) is the *second* line of defense — it resolves an over-dirty to zero widget
ops — not the first. (Found via instrumentation in C2: `tags`/`tasklist` were dirtying on every
event, ~8 idle rebuilds of the tag row in 6s.)

**Derive to the display value first, then equality-check** — compare *what the user sees*, not the
raw signal. A continuous source (wifi strength, memory bytes, CPU fraction) feeding a discrete
display (a signal-bucket icon, a rounded percent) must round/bucket *before* the dirty check, so
sub-display-resolution jitter produces zero re-renders (volume sub-percent, network signal bucket,
memory `34.7%`→`34.9%` both render `34%`).

## Bucketed display: icon family vs CSS class
A continuous quantity shown as a bucketed visual can map the bucket to either a per-bucket *icon*
or a *CSS class on a constant icon*. Use a per-bucket icon **only when a real icon family exists**
(network does: `network-wireless-signal-{none,weak,ok,good,excellent}` is freedesktop-standard). For
derived quantities with no such family (CPU load), keep the icon constant and tint via a CSS class —
inventing `cpu-load-*` names would just render broken icons. Disk/thermal will face the same call.

## Async backends: per-future on the main context, not a centralized executor
A backend that drives D-Bus subscriptions (or one task per tracked thing) spawns a
`glib::spawn_future_local` per concern on the main context, rather than one executor multiplexing
them. Each future composes with the GLib loop the way the wl_display fd watch does; `deliver_event`
runs on the main thread; zbus's own reactor handles the I/O threading. Instances so far: the network
NM subscription (one future), and statustray (one future per tray item + the Watcher/Host).
Justification: N is small and the tasks are independent, so GLib's scheduler handles them without a
pool. Revisit only if a future plugin demonstrates main-loop saturation (e.g. high-volume
notification signal traffic) — decide against real load, not anticipated load.

**Server-side D-Bus handlers must be minimal and dispatch substantive work to the main thread via
`spawn_future_local`.** zbus `#[interface]` handlers run on zbus's executor (they're `Send + Sync`)
and must not assume main-thread context — touching `Rc`/main-thread state from them is unsound. The
statustray Watcher handlers only record into an `Arc<Mutex<…>>` and emit a signal; the Host, the
per-item futures, and pruning all run on the main thread and coordinate through bus signals. Any
future server-side surface (a Phase G notifications server, a Wafflebar control interface) follows
the same shape: the GLib loop is the source of truth for stateful work; the zbus executor is a thin
transport.

## Dependency discipline
When a transitive dependency already re-exports what you'd otherwise add directly, use the
re-export. (Network consumes zbus's signal stream via `zbus::export::ordered_stream` rather than
pulling in `futures-util` — one fewer `Cargo.toml` line, one fewer version-skew risk.)

## FFI callback re-entry
Any FFI library that takes a callback and may call it **synchronously from inside the registering
function** is a re-entry hazard whenever that callback touches state the registering site is already
borrowing. libpulse does exactly this: it fires the context state callback from inside `connect()`,
which we call while holding the `Context`'s `RefCell` borrow — the callback's `borrow()` then panics.
The fix is to **defer the callback body to the next loop iteration** (`glib::idle_add_local`), so it
runs after the borrow is released — *not* to restructure the borrow. Apply this reflexively to any
future callback-style FFI (libnm, libpipewire, …). (Aside: a dev box with no audio server surfaced
this on the CONNECTING→FAILED transition; a live server would have hit the same panic on
CONNECTING→READY — the missing happy-path environment bought the bug early.)

## Scroll affordance
Scroll on a `Button` is two optional `ActionId`s (`scroll_up`/`scroll_down`), not a `delta` value.
Discrete-by-construction: the renderer accumulates GTK4 smooth-scroll (`Cell<f64>` remainder) and
dispatches one action per whole tick, so plugins never see sub-tick deltas (five-percent-per-tick
volume, one-item-per-tick lists). Crucially this routes through the *same* `ActionId` dispatch as
click/menu/key, so a new scroll consumer needs no new dispatch path and composes with everything
`Button` already does. Direction is GTK4-normalized (device + natural-scroll applied); v1 always
consumes — `TODO(scroll)` a `handled` flag when nested scrollables ship.

## Roadmap (phases, each tied to a real directory)
- **A** finish M2 — `tasklist` (this PR).
- **B** plugin framework — `Plugin::configure` (per-instance TOML + `notify` live-reload), `launcher`, `separator`/`showdesktop`.
- **C** system plugins (cpu/mem/net/volume) + keyed-diff reconcile + real fd-watch.
- **D** popups + `statustray` (SNI; read `plugins/systray/sn-plugin.c`, `systray-box.c`).
- **E** applications menu (garcon-style `.desktop`/Menu-Spec parsing, then the menu plugin).
- **F** preferences UI (panel/add-items/per-plugin dialogs, CSS theming).
- **G** notifications (`org.freedesktop.Notifications`).
