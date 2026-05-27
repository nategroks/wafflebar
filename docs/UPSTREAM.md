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

## Roadmap (phases, each tied to a real directory)
- **A** finish M2 — `tasklist` (this PR).
- **B** plugin framework — `Plugin::configure` (per-instance TOML + `notify` live-reload), `launcher`, `separator`/`showdesktop`.
- **C** system plugins (cpu/mem/net/volume) + keyed-diff reconcile + real fd-watch.
- **D** popups + `statustray` (SNI; read `plugins/systray/sn-plugin.c`, `systray-box.c`).
- **E** applications menu (garcon-style `.desktop`/Menu-Spec parsing, then the menu plugin).
- **F** preferences UI (panel/add-items/per-plugin dialogs, CSS theming).
- **G** notifications (`org.freedesktop.Notifications`).
