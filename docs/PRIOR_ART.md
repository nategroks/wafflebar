# Prior Art — design lessons wafflebar is stealing (and mistakes it isn't)

wafflebar is a **desktop panel** for tiling WMs, not a status line. This doc records what we
take from the projects that solved these problems before us, and what we deliberately avoid.

## xfce4-panel — the plugin-isolation gold standard
*(gitlab.xfce.org/xfce/xfce4-panel — `panel/panel-plugin-external-wrapper.c`, `libxfce4panel/`)*

Since 4.8, xfce4-panel can run a plugin **in its own process** via a *wrapper*: the panel
spawns a wrapper per external plugin, the wrapper embeds the plugin, and the two talk over
**D-Bus** (e.g. `PROVIDER_PROP_TYPE_ACTION_SHOW_CONFIGURE`). A plugin is internal or external
per the `X-XFCE-Internal` key in its desktop file. The payoff: **a crashing plugin can't take
the panel down.**

- **Steal:** a `Module` boundary clean enough that a module *could* later be hoisted into its
  own process (config is serializable, state arrives via events, no shared mutable globals).
- **Don't (yet):** actually fork a process per module. That's a v2+ hardening step; v1 modules
  are in-process trait objects. We just refuse to design anything that forecloses isolation.

## KDE Plasma panel — layer-shell anchoring done at scale
*(plasma-workspace; LayerShellQt)*

Plasma anchors its panel as a layer-shell surface (via LayerShellQt) with exclusive-zone
reservation, and plasmoids are sandboxed declarative components. The lesson is mostly about
**anchoring discipline**: a panel reserves space (struts/exclusive zone) so tiled clients never
draw under it, and popups are real surfaces, not painted-on overlays.

- **Steal:** treat exclusive-zone math + popup surfaces as first-class (see ARCHITECTURE popup
  strategy). On X11 the equivalent is `_NET_WM_STRUT_PARTIAL` + `_NET_WM_WINDOW_TYPE_DOCK`.

## Cairo-Dock — the WM-abstraction blueprint
*(github.com/Cairo-Dock/cairo-dock-core — GLDI, `src/implementations/cairo-dock-wayland-manager.*`)*

Cairo-Dock runs on "every desktop" via **GLDI**: a generic core plus swappable backends chosen
at build/runtime (`-X`/`-L` to force X11/Wayland). A *Wayland manager* receives compositor
signals and **dispatches them to a Windows manager and a Desktop manager**; compositor-specific
features (KWin workspaces, labwc/cosmic) live behind that seam. It uses gtk-layer-shell for
positioning.

- **Steal — this is our `src/wm/` model exactly:** a `WindowManager` trait is the seam; per-WM
  backends (dwl, river, hyprland, sway, i3, bspwm) translate native events into a common
  vocabulary (workspaces, window list, layout, outputs). The bar never calls dwl APIs directly.
- **Steal:** backend selection at runtime (detect the compositor), not a compile-time fork.

## Waybar — the cautionary tale (what NOT to do)
*(github.com/Alexays/Waybar — `src/modules/wlr/taskbar.cpp`, `src/modules/sway/window.cpp`)*

Waybar is a **status line wearing a panel's clothes**. It's a single layer-shell window; it's
stuck on **GTK3 specifically because GTK4 + system tray is unsolved there**; its only window
list is `wlr/taskbar` rendering **one label + one tooltip per window** (no rich entries, no
menus); and custom modules are limited to a label+tooltip with no popup/interaction story.

- **Don't:** treat the bar as one flat widget row. wafflebar is GTK**4**, has real `Popover`
  popups, rich taskbar entries (icon + title + context menu), and a proper tray.
- **Steal anyway:** *how* it consumes `wlr-foreign-toplevel` and sway-ipc in those two files is
  a solid reference for our dwl backend's toplevel bookkeeping and click→activate plumbing.

## eww — GTK layer-shell idioms in Rust
*(github.com/elkowar/eww)*

eww shows the Rust + GTK + `gtk-layer-shell` patterns (surface setup, reactive redraw) and a
reactive config model. Still status-bar-shaped, but the layer-shell wiring and the
"state changes → declarative re-render" loop are good priors for our module render path.

- **Steal:** the reactive update discipline — modules expose state; the widget re-renders on
  state change. Don't busy-poll; subscribe.

## StatusNotifierItem / StatusNotifierWatcher — the tray, done right
*(freedesktop.org StatusNotifierItem spec; org.kde.StatusNotifierWatcher)*

Modern trays are **D-Bus**, not XEmbed. One `StatusNotifierWatcher` exists on the session bus;
apps register items as `org.freedesktop.StatusNotifierItem-PID-ID`; a **Host** (a panel)
registers with the Watcher to display them. wafflebar must be **both** — spawn the Watcher if
none exists, and act as a Host.

- **Steal:** implement Watcher + Host over zbus; render `StatusNotifierItem` icons + their
  `com.canonical.dbusmenu` context menus.
- **Don't:** lead with XEmbed. It's the X11-only fallback, behind a feature flag, later.

## The dwl IPC wire format we consume (verified against the patched dwl)
*(protocols/dwl-ipc-unstable-v2.xml — vendored)*

- `zdwl_ipc_manager_v2`: `get_output(wl_output) -> zdwl_ipc_output_v2`; manager events
  `tags(count)`, `layout(name)` advertise capabilities.
- `zdwl_ipc_output_v2` events (per monitor): `tag(tag, state, clients, focused)` where
  `tag_state ∈ {none, active, urgent}`, `layout(index)`, `layout_symbol(str)`, `title(str)`,
  `appid(str)`, `active(bool)`, `fullscreen`, `floating`, and `frame` (commit a batch).
- requests (click-handlers): `set_tags(tagmask, toggle)`, `set_layout(index)`,
  `set_client_tags(...)`.

This is the same protocol waybar-dwl speaks, so the **tags** and **window** modules are pure
dwl-ipc consumers; the **taskbar** layers `wlr-foreign-toplevel-management` on top for the full
window list + `activate`/`close`/`set_minimized` requests.

## Summary of the seams we're committing to
1. `Module` trait — in-process now, isolation-ready boundary (xfce4-panel lesson).
2. `WindowManager` trait in `src/wm/` — Cairo-Dock GLDI model; dwl backend first.
3. Real popups + exclusive-zone discipline (Plasma/Cairo-Dock), GTK4 `Popover`.
4. Tray = SNI Watcher+Host over D-Bus (freedesktop spec), not XEmbed-first.
5. Subscribe, never poll (eww).
