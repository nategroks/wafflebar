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

**This is now the project's D-Bus-server idiom — validated by the SNI Watcher (D2), reused by the
Notifications server (G).** Own the bus name; expose a minimal `Send + Sync` `#[interface]` (state in
`Arc<Mutex<…>>` / atomics); bridge to the main thread; keep the GTK-free core data (`TrayItem`,
`Notification`) at the boundary; render host-side. The bridge mechanism depends on whether a *natural
bus signal* exists: the Watcher rides its own `ItemRegistered` (the Host subscribes to it), but the
Notifications spec has no "posted" signal, so G uses an `async-channel` from the iface to a
main-thread drain loop — `JoinHandle::abort`-style cleanliness, no manual thread management beyond the
channel.

**Server-side surfaces are in-process, not separate daemons, and don't fight an existing owner.**
Notifications run in the bar process (the bar *is* the shell — like tray and menu), reusing the zbus
connection + GLib loop; the GTK-free `core::notification` keeps the door open to lifting it to its own
binary (v1→v2 isolation, for the server this time). One owner per session: acquire the name with
`DoNotQueue` so contention is immediate, log-and-disable if another daemon (mako/dunst) holds it, and
offer `--replace-notifications` (`ReplaceExisting | AllowReplacement`) to take over deliberately.

**Notification popups (G1b) are a host-managed widget stack, not a reducer `View`.** Like the menu and
prefs, notifications are host-rendered from GTK-free data — but unlike a plugin's `View` (which the
keyed-diff reconciles), the popup stack manages GTK widgets directly in a `HashMap<id, widget>` keyed
by notification id. `replaces_id` rebuilds the existing row's children in place (same widget → same
stack position, no flicker); the keyed-diff is for *reducer-described* content and doesn't apply here.
Per-id expiry timers live in a `HashMap<id, SourceId>` — the `TimerSet` discipline (F2b) reused:
critical urgency / `Never` schedule no timer; a fired timer or a dismiss emits `NotificationClosed`,
an action emits `ActionInvoked` then closes (the popup→server reverse of the G1a channel, via the
`NotifyServer`'s stored connection). A visible cap (5; critical may exceed to a hard max) with a FIFO
overflow queue — never drop, only on close/expiry. Image resolution priority: `image-path` hint →
`app_icon` (icon name or absolute path) → generic fallback. The raw `image-data` hint is deferred (a
TODO). **`argb_to_rgba` (D2c.1) has exactly two consumers — SNI icon and SNI attention, both ARGB32.**
Notification `image-data` is **RGBA** (spec's Image Data Format: R,G,B,A; little-endian memory order
differs from SNI's), so when it lands it needs its *own* conversion path, not `argb_to_rgba` — the
design-note "third consumer" assumption was wrong.

## Finish what's nearly done before opening new surface
At a milestone boundary, prefer finishing a nearly-done feature over starting a new one. An 80%-done
feature is a *quality* problem (works for the common case, breaks on edges — users experience it as
broken), which is worse than a 0%-done *feature gap* (users know it's absent). Exceptions: a genuine
architectural unknown is blocking, or the new surface is on a deadline. (D2c finished statustray
before opening E/F/G.)

The principle applies when the unfinished piece would **degrade silently** or **block a downstream
consumer**. *Documented, rare, architecturally-bounded* limitations are a different shape and are
tolerable across a milestone boundary — defer them when a higher-leverage milestone is ready. (F2b
left three backend-class edits — first-add / last-remove of volume/network/tray — applying-with-a-warn
rather than live; each is explicit, rare, and bounded to the F2c rewireable-sinks change, so F3
[preferences UI, unblocks per-plugin config for *every* plugin] was taken before F2c. Contrast F2→F2b,
where "structural edits warn restart" was a silent half-promise of the live-reload feature and so
*did* lean finish-first.)

## Pay down accumulating debt before adding components
When a *class* of debt is accumulating across multiple components, paying it down outranks adding new
components — even when the new ones are individually more exciting. Adding components before the
paydown extends the debt surface and the eventual migration. This is the finish-before-opening
principle scaled from one feature to a debt class. (Phase F was taken over E/G because every C1
plugin shipped `TODO(F)` config markers, and each new plugin would add more.)

## Phase F is shaped by the GTK-free-plugin invariant
F looks different from xfce4-panel *by design*. xfce plugins own GTK settings dialogs and bind
GObject properties to Xfconf; our plugins are GTK-free reducers, so neither is available to us. The
`configure(&ModuleConfig) -> Reaction` trait surface (uniform, object-safe — a per-plugin typed
config struct would break `Box<dyn Plugin>`) and host-rendered schema-driven settings forms are
*forced* by the M2 invariant, not chosen. That the trait shape makes F doable at all validates the
M2 call that no GTK type crosses the plugin boundary. Resist "just let plugins build their own
dialogs" during F3 — it would break the v1→v2 isolation that the whole architecture rests on.

## Preferences UI (F3) — three shape rules
**Schema is declarative — describes fields, not values.** `config_schema()` returns field
descriptions (key/label/kind/static-default), never a current-value snapshot. Current values live in
the TOML; the host reads them per-key to populate a form and writes them per-key on edit. Plugins
never hold or expose current config values across the trait boundary — the TOML is the single source
of truth, read at `configure()` time. (A value-in-the-field design would create two sources — the
plugin's internal value and the TOML's — that drift; reading/writing both directions through the TOML
removes the synchronization entirely. Same isolation discipline as `configure()` returning a
`Reaction` and `View` carrying no widgets, applied one level deeper.)

**One settings window, because the host owns all forms.** xfce4-panel spawns N windows (one
per-plugin dialog) because each dialog is *owned by its plugin*. Our forms are host-rendered from
schema, so they fold into a single master-detail window (target list + the selected target's form).
That's the v1→v2 isolation paying a UX dividend, not a divergence to be defensive about.

**The UI writes TOML and lets the watcher drive reload.** A field edit writes the value back to the
file (via `toml_edit`, preserving comments/order) and stops — it does **not** apply the change
itself. The existing file-watch reload (F2/F2b) picks it up via `configure()` or a structural rebuild.
One code path for UI-edit and hand-edit; no separate apply pipeline, no in-memory overlay, no
double-apply. The bar config (`[bar]`) is described by a host-owned schema (not a plugin) and rendered
through the same pipeline; its edits persist but apply on restart (anchors/zone/CSS are fixed at
window creation), surfaced by an honest footer rather than a hidden half-working path.

## Add Items & application discovery (F4)
**Plugin metadata is a static binary registry, not a trait method.** `plugins::catalog()` lists the
addable kinds (`PluginInfo { kind, name, description, icon, unique }`). The binary knows its
compiled-in plugin set at build time — plugins describe what they *are* statically; instances are
dynamic. Metadata is binary-time, not runtime, so it stays out of the `Plugin` trait (which describes
a live instance). `unique` is a coarse global flag, set only for `statustray` (a second instance fails
to acquire the SNI Watcher name and renders empty — a silent failure we prevent by disabling the Add
entry). Per-output plugins (tags/window/showdesktop) legitimately allow multiple instances; a
`Uniqueness::PerOutput` scope is a TODO if same-output duplicates prove confusing.

**`.desktop` discovery is F4-owned; E reuses the data layer and adds categorization.**
`core::freedesktop::list_applications()` enumerates `$XDG_DATA_DIRS/applications`, dedupes by id (first
dir wins, per spec), and excludes `NoDisplay` (the launcher resolver *keeps* `NoDisplay` for explicit
references — enumeration and explicit-resolution differ here). E's applications menu builds the
Menu-Spec category tree *on top of* the same enumeration: F4 = list/search, E = categorize. Don't
reinvent the walk in E.

**`FieldKind::default` now has three load-bearing uses** — form population (F3), Add-Items seeding of a
new module's options (F4), and a future reset-to-default affordance (F5+). It's static schema data
(what the field shows when the key is absent), not a current-value snapshot, so it doesn't reintroduce
the two-sources problem. Don't remove or repurpose it casually.

**`PluginInfo.unique` is a coarse global flag — lift to a `Uniqueness` variant when multi-monitor
lands.** It's `true` only for `statustray` today. Two consumers now want a finer scope: Add Items
(disabling the second instance of a per-output plugin on a given output) and a future multi-monitor
preferences UI. When someone implements per-output placement properly, lift `unique: bool` to
`Uniqueness::{Global, PerOutput, None}`. Deferring the variant was right — one consumer, one true case.

## Applications menu (E) — categorize, don't parse `.menu`
The menu groups apps by their `Categories=` key into the freedesktop registered **Main Categories**
(Audio/Video fold into AudioVideo; first registered category in an app's list wins; unmatched →
"Other"), *not* by parsing the Menu-Spec `.menu` XML. The main buckets are the user-visible value;
the full `.menu` tree (distro custom layouts, `<Include>`/`<Exclude>`, merge order) is large, fragile,
and has no adopted Rust crate — deferred until a real custom-layout consumer appears. `core::menu`
consumes F4's `list_applications()` and adds categorization on top: the F4/E split (F4 = list/search,
E = categorize) designed at F4, validated here. The directory watch that rebuilds on app
install/uninstall is **host-side** (gio, E2), not core — `core::menu::categorized()` is a pure
rebuilt-on-demand function (core is GTK/gio-free).

**Recents = frecency, not last-N.** `launches × 2^(-age_days / 30)` (30-day half-life): recency ranks
recent launches up without discarding frequently-used apps. Keyed by desktop-file **id** (stable
across reinstalls, never the path). Capped at 50 with lowest-score eviction — bounded growth, no
arbitrary expiry (frecency ranks the long tail). Persisted to `$XDG_DATA_HOME/wafflebar/recents.toml`
on each launch (user-paced, tiny file). Corrupt file → start fresh, never block the menu.

**`View::AppMenu` — the host-rendered-rich-content escape hatch (E2).** The boundary rule, now
validated twice (prefs forms, the apps menu): **reducer-derived state goes through `View`;
immediate-mode interactive state lives in the host widget.** Search-as-you-type, multi-pane focus,
scrolled selection have no meaningful reducer derivation — pushing keystrokes through
`Event → reducer → View → re-render` would be the wrong epistemic model (the reducer computing on
data it doesn't own). So `View::AppMenu` is a *marker*: the reducer carries only its config
(favorites, recents settings, which it does own), and the host builds the whole widget from the
GTK-free `core::menu`/`core::recents` data it holds. The invariant is for *plugin* isolation (v1→v2);
host infrastructure (renderer, prefs, the menu UI) was never going to be process-isolated, so
host-rendering it isn't a carve-out. Future surfaces with substantial non-reducer-derived UI state
(file pickers, wizards) follow the same pattern: a narrow marker variant + a host widget; don't
expand `View` into an immediate-mode toolkit.

**Cold-open is show-only by construction.** The menu content is built at render time (startup, and
on the directory watch's refresh), not on open — `View::AppMenu` is a static marker, so the keyed
diff would skip it on a cache change, which is why the watch force-rebuilds the appmenu slots
(`Host::refresh_appmenu`, clearing `last_view`). Opening the popover just shows pre-built widgets, so
the reparse-on-open bottleneck the <200ms target guards against is structurally avoided. The
directory watch is **host-side** (gio belongs with its consumer; `core::menu` stays a pure function).

**Founding scope.** E2 lands the applications menu — the last of the original prompt's three pillars
(applications menu + system tray (D2) + widget customization (F)), all working across tiling WMs by
design. Keyboard navigation (E3) is the finishing enhancement on the menu; F2c closes the
backend-reconciliation gap; then notifications (G) and CSS theming round out v1.0.

## Defer optimizations until the architecture demonstrably fails
Optimizations are deferred until the architecture demonstrably fails to prevent the load case they
target. The reducer-diff-keyed pipeline prevents most redundant work *by construction* (value-compare
suppresses unchanged events; the keyed diff reuses widgets for unchanged nodes), so caching on top of
it is usually redundant — justify it against a *measured* failure mode, not an imagined one.
Declined so far: the 20 Hz poll (already ~0% CPU; replaced in C3 only to reach true zero), the shared
`PollingBackend<T>` (deferred at C1 — wrong abstraction until the third instance), and the tray
pixbuf decode cache (the keyed diff already reuses the `Image` for unchanged icons, so identical
bytes never re-decode).

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

## Resource lifecycle & structural reload (F2b)
Plugins are pure reducers and own **no** live resources — the GTK-free / serializable invariant (for
v1→v2 process isolation) forbids a plugin from holding a libpulse context, a zbus proxy, or a GLib
`SourceId`. So `Plugin::teardown` is, and should stay, a no-op hook on every v1 plugin; the real
cleanup lives **app-level, keyed by topic/backend**, not per-plugin. A structural config reload keeps
the *same* `Host` (its sinks, fd-watch, and backends holding `Weak<Host>` all stay valid) and replaces
only the slot vector + grid.

The load-bearing distinction for what a rebuild must clean up: **timers leak on *every* rebuild if
ignored** (a blindly re-added interval/poller stacks a second source), whereas **backends leak only if
*recreated*** (they're single instances held by the host's immutable sinks; leaving them alone is
safe, just possibly idle). That split is why F2b reconciles timers (the `TimerSet`: clock intervals +
memory/CPU pollers, diffed remove-then-add so re-adds can't stack) but defers backend
start/stop — which needs rewireable sinks — to F2c. Adding the first consumer of an unstarted backend
warns rather than connecting; removing the last leaves it idling.

**Timer-closure ownership rule:** a timer whose lifetime is managed alongside the host must capture
`Weak<Host>`, never `Rc<Host>` — otherwise the host→…→timer→host cycle pins the host forever and a tick
firing after teardown would act on a stale slot. Memory/CPU established this; clock joined them in F2b
(it previously captured a strong clone because its `SourceId` was leaked, not owned). (Note: F2b
reconciles timer *existence*, not period — a changed memory `interval` still needs a restart, since the
poller's GLib timer is created with a fixed period.)

## Backend reconciliation (F2c) — closing the F2b deferral
F2c made the backend-class backends (volume/network/tray) hot-reconcilable, closing the F2b/F3/F4
"add-first needs restart" limitations. **Rewireable sinks:** the command sinks no longer capture a
concrete backend at `Host::new`; they route through a host-owned `Rc<RefCell<Backends>>` slot, so a
backend can be started/stopped on a structural reload. Same shape as `TimerSet` — indirection through
a mutable slot, reconciled (idempotent) on every rebuild. `Weak<Host>` in the delivery handlers, as
always.

**Teardown discipline is per-backend, not uniform — governed by what external observers see, not by
symmetry.** Volume (libpulse) and network (zbus) **fully stop** on remove-last: drop the libpulse
context (Drop disconnects); `JoinHandle::abort()` the network future (**the handle is the
cancellation handle — it cancels the await at its point, doesn't block, so no separate `Cancellable`
and no hang**; this resolved the F2b long-await worry). Tray is **start-once-keep**: dropping the SNI
Watcher bus name fires `NameOwnerChanged` and makes *external* items re-register (visible flicker), so
once started it persists — remove-last idles it (harmless single instance). The principle: a backend's
teardown shape is dictated by the cost its teardown imposes on outside processes, not by making all
three look the same.

**`[bar]` reposition is live re-anchoring, not window recreation.** gtk4-layer-shell reconfigures a
*mapped* surface, so a top↔bottom move is `set_anchor` + `auto_exclusive_zone_enable` on the live
window + updating the host's `Cell<Position>` (for popover direction) — verified live. No destroy/
recreate, no slot re-parenting. (This finding collapsed reposition from a potential F2c2 to ~80 lines.)

**Gotcha caught by live-verify:** a `match` scrutinee's temporaries live until the *end of the match*,
so `match f(cell.borrow().x) { … cell.borrow_mut() … }` panics "already borrowed". Hoist the read into
a `let` before the match. (An `if` condition's temporaries drop before its body, so `if` is safe — but
hoist for uniformity.)

## Design-note discipline
A design note that reaches a finding **contradicting its own framing** is the phase working as
intended, not a detour. The point of reading upstream / the existing code before committing to a design
is to surface the inverse-direction question — "what does the code actually do?" vs. "what does the
conventional shape assume?" F2b's note set out to add `Plugin::teardown`-as-cleanup-mechanism (the
conventional shape: plugins own their backends) and the read proved the opposite (the invariant forbids
it; resources are host-level). Default to *read first, then propose findings that contradict the
framing when the code supports them* — not *read the framing, then validate it*.

## Testing under GTK
Tests that initialize GTK must be merged into **one `#[test]` fn per test binary**: libtest gives each
test its own thread, GTK binds init to the first thread that calls it, and a second init panics
("Attempted to initialize GTK from two different threads") — even under `--test-threads=1`. So either
fold all GTK-touching asserts into a single self-skipping fn (the `renderer_gtk_behaviors` pattern,
which also covers the B3 separator/popover asserts), or keep the test **GTK-free** by exercising the
logic below the widget layer (F2b's `timer_reconciliation` drives the timer diff against a slot-less
host — GLib timers need only a main context, not a display).

## Extract the decision from the I/O
When interaction logic can't be tested against its integration target, **extract the decision into a
pure function taking explicit `(state, input)` and returning an action; make the integration layer a
thin translator.** The pure function gets exhaustive unit tests; the I/O layer stays small enough to
verify by reading. Three instances: `FakeWm` (WindowManager logic without a real compositor),
`Recents` internals (frecency without a clock or disk), and E3's `handle_key` (keyboard nav without
GTK input injection — `(Pane, search_empty, NavKey) -> KeyAction`, the GTK controller just normalizes
the event, calls it, and applies the action). The throughline: the integration target resisting
direct testing is the signal to separate decision from I/O, not to skip the test.

A GTK detail from E3 worth keeping: **keyboard-nav controllers on a multi-widget popover must use
capture phase** (`set_propagation_phase(Capture)`) to decide routing *before* a focused `ListBox`'s
built-in nav fires. Bubble phase is too late — the ListBox has already moved its selection. Return
`Proceed` for the keys you don't handle so native editing (text cursor, insert) still works.

## Roadmap (phases, each tied to a real directory)
- **A** finish M2 — `tasklist` (this PR).
- **B** plugin framework — `Plugin::configure` (per-instance TOML + `notify` live-reload), `launcher`, `separator`/`showdesktop`.
- **C** system plugins (cpu/mem/net/volume) + keyed-diff reconcile + real fd-watch.
- **D** popups + `statustray` (SNI; read `plugins/systray/sn-plugin.c`, `systray-box.c`).
- **E** applications menu (garcon-style `.desktop`/Menu-Spec parsing, then the menu plugin).
- **F** preferences UI (panel/add-items/per-plugin dialogs, CSS theming).
- **G** notifications (`org.freedesktop.Notifications`).
