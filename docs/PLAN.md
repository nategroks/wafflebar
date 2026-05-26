# wafflebar — Implementation Plan (Part A / v1)

> *"a griddy bar with yummy customization. No bloat."* — built in Rust, for wlroots compositors (dwl first).

This plan covers **Part A (v1 bar)** only. Part B (GUI configurator + guest mode) gets its own
`docs/PLAN-v2.md` after Part A's M4 PR is merged and you say "go on Part B".

---

## 0. Environment — verified on the target machine

| Thing | Finding |
|---|---|
| Distro | Pentoo 2.18 (gentoo-based, OpenRC) |
| Compositor | **dwl 0.8**, custom build at `/usr/local/src/dwl`, launched by `/usr/local/bin/dwl-nvidia` |
| Toolchain | Rust **1.93.1** + cargo ✅ |
| GTK | **gtk4 4.18.6** ✅ · `wayland-client 1.24` · `wayland-protocols 1.47` · `glib 2.84` |
| gtk4-layer-shell | **not installed** — `gui-libs/gtk4-layer-shell` is in the Gentoo tree (M1 prereq) |
| waybar feature set (v1 ground truth) | `wlr/taskbar` · clock · cpu · memory · network · pulseaudio · tray |
| Battery | **none** (desktop) → no battery module in v1 |
| Launcher | **wofi** only (no fuzzel) → launcher button targets `wofi` |
| Theme / icons / font | Nordic / **indigo-reality** / Hurmit Nerd Font Mono |

### 0.1 The dwl data gap (resolved with your approval)
Your dwl 0.8 build implements **neither** `zdwl_ipc` **nor** `wlr-foreign-toplevel-management`. It only emits
the classic dwl **stdout status** (`<output> tags … / title … / layout … / selmon …`). That is why the current
waybar shows no tags and an empty taskbar.

**Approved decisions:**
- Patch dwl with **`dwl-ipc-unstable-v2`** (codeberg `dwl/dwl-patches/wiki/ipc`) → exposes `zdwl_ipc_manager_v2`.
- Patch dwl with **`wlr-foreign-toplevel-management`** → exposes the full window list for the taskbar.
- Both land in **M2**, rebuild `/usr/local/bin/dwl`, re-login. Backups kept; additive, low-risk.

---

## 1. Toolkit decision (accepting the brief — no pivot)

**Rust + GTK4 (`gtk4-rs`) + `gtk4-layer-shell`.** Accepted as written. Rationale:
- `gtk4-layer-shell` (pentamassiv, **v0.8.0**, MIT) cleanly anchors the bar via the layer-shell protocol on wlroots.
- GTK4 inherits your **Nordic** theme + **indigo-reality** icons + Hurmit font for free, and CSS theming maps 1:1 to
  the waybar `style.css` mental model you already use.
- `gtk4-rs` is mature and well-documented.

**Protocol access (the one non-obvious bit):** the bar surface is GTK4, but tags/taskbar need raw Wayland protocols
(`zdwl_ipc`, `wlr-foreign-toplevel`). Plan: obtain GDK's existing `wl_display`
(`gdk_wayland_display_get_wl_display`) and drive a `wayland-client` event queue on the GLib main loop via the Wayland
fd (a `glib::Source` on the queue's fd). **One connection, one event loop**, no threads fighting GTK. Fallback if that
proves fiddly: a second `wayland-client` connection on its own thread feeding updates over a channel. Documented in
`docs/architecture.md` at M2.

*No `docs/toolkit-decision.md` — I'm not pivoting, so there's nothing to argue.*

---

## 2. Architecture

```
wafflebar/                      cargo workspace
├── crates/
│   ├── wafflebar-core/         lib: config, grid engine, Module trait, protocol clients
│   │                           (fully doc-commented + unit-tested; no GTK deps where avoidable)
│   └── wafflebar/              bin: GTK4 app, layer-shell surfaces, module→widget wiring
├── protocols/                  vendored XML: wlr-layer-shell, wlr-foreign-toplevel, dwl-ipc-unstable-v2
├── docs/                       PLAN.md, config.md, architecture.md
└── themes/                     nord.css (ships matching your current waybar look)
```

- **Process model:** one `wafflebar` process; **one layer-shell surface per monitor** (you have 3 — multi-output is
  first-class, not bolted on). Top-anchored, exclusive-zone = bar height.
- **`Module` trait** (in core): `fn build(&self, cfg) -> ModuleHandle` returning a `gtk::Widget` + an async update
  stream. Modules never touch GTK off the main thread; updates arrive via `async-channel` and are applied in a
  `MainContext::spawn_local` task. Polling modules (cpu/mem/clock) use a `glib` timeout; event modules
  (tags, tray, network) are push-driven.
- **Data sources** (each a small client in core):
  | Source | Mechanism | Dep |
  |---|---|---|
  | dwl tags / layout / focused title | `zdwl_ipc_manager_v2` (after M2 patch) | wayland-client + vendored XML |
  | taskbar (all windows + icons) | `wlr-foreign-toplevel` (after M2 patch); app_id→.desktop→GTK icon | wayland-client |
  | system tray | `org.kde.StatusNotifierWatcher` / SNI over D-Bus | `zbus` |
  | volume | PulseAudio API (works through pipewire-pulse) | `libpulse-binding` |
  | network | NetworkManager over D-Bus | `zbus` |
  | cpu / memory | read `/proc/stat`, `/proc/meminfo` directly | **none** (no-bloat) |
  | clock | format `chrono` local time | `chrono` |
  | launcher | spawn `wofi --show drun` | `std::process` |
- **Config:** TOML via `serde` + **`toml_edit`** (chosen now so Part B's GUI round-trips comments/ordering).
  Path: `$XDG_CONFIG_HOME/wafflebar/config.toml`. Schema is **versioned** (`schema = N`) from day one — Part B relies on it.
- **Logging/errors:** `tracing` (env-filter), `thiserror` in core, `anyhow` in the binary. No `println!`, no stray `.unwrap()`.

---

## 3. The grid layout model (the "griddy")

Instead of waybar's fixed left/center/right, the bar is a **CSS-grid**: `rows × columns`, each module placed at a cell
with optional span and alignment. v1 bars are usually `1 × N`, but the engine supports multi-row (vertical bars / stacked
layouts later). Maps directly onto a `gtk::Grid`. The engine (parse → validate → place, with overlap detection) lives in
core and is **unit-tested**.

### 3.1 Example A — reproduce today's waybar (1 row, 3 logical zones)
```toml
schema = 1

[bar]
monitor = "all"          # or a serial / connector name
position = "top"
height   = 26
theme    = "themes/nord.css"

[grid]
rows = 1
columns = 12             # 12-col track; align modules into start/center/end

[[modules]]
type = "taskbar"
cell = { row = 0, col = 0, colspan = 4 }
align = "start"

[[modules]]
type = "clock"
cell = { row = 0, col = 4, colspan = 4 }
align = "center"
format = "  %a %d %b   %H:%M"

[[modules]]
type = "cpu"
cell = { row = 0, col = 8, colspan = 1 }
align = "end"
[[modules]]
type = "memory"
cell = { row = 0, col = 9, colspan = 1 }
[[modules]]
type = "network"
cell = { row = 0, col = 10, colspan = 1 }
[[modules]]
type = "pulseaudio"
cell = { row = 0, col = 11, colspan = 1 }
[[modules]]
type = "tray"
cell = { row = 0, col = 11, colspan = 1 }
align = "end"
```

### 3.2 Example B — a 2-row "griddy" layout (shows off the model)
```toml
schema = 1
[bar]
position = "top"
height = 48

[grid]
rows = 2
columns = 6

[[modules]]
type = "tags"                       # dwl workspace/tag indicator
cell = { row = 0, col = 0, rowspan = 2, colspan = 1 }

[[modules]]
type = "window"                     # focused window title
cell = { row = 0, col = 1, colspan = 4 }

[[modules]]
type = "clock"
cell = { row = 1, col = 1, colspan = 2 }
format = "%H:%M:%S"

[[modules]]
type = "cpu"
cell = { row = 1, col = 3 }
[[modules]]
type = "memory"
cell = { row = 1, col = 4 }

[[modules]]
type = "tray"
cell = { row = 0, col = 5, rowspan = 2 }
```

Full schema reference (every module's options) → `docs/config.md`, written in **M1** and kept in lockstep thereafter.

---

## 4. Dependencies — versions, licenses, GPL-2 compatibility

Repo license is **GPL-2.0**. The one real hazard: **Apache-2.0-*only* is GPL-2.0-incompatible** (patent clause). All
crates below are **MIT or MIT/Apache-2.0 dual** → we take the **MIT** option → fully GPL-2.0 compatible. **No
GPL-incompatible deps.**

| Crate | Ver (approx) | License | Note |
|---|---|---|---|
| `gtk4` (gtk4-rs) | 0.10 | MIT | bindings MIT; GTK4 itself LGPL (dynamic link, fine) |
| `gtk4-layer-shell` | 0.8 | MIT | layer-shell surface |
| `wayland-client` + `wayland-scanner` + `wayland-protocols-wlr` | 0.31 | MIT | protocol clients |
| `zbus` | 5.x | MIT | tray + NetworkManager D-Bus |
| `libpulse-binding` | 2.x | MIT | volume (via pipewire-pulse) |
| `chrono` | 0.4 | MIT/Apache (→MIT) | clock |
| `serde` + `toml_edit` | 1 / 0.22 | MIT/Apache (→MIT) | config round-trip |
| `tracing` (+`-subscriber`) | 0.1/0.3 | MIT | logging |
| `thiserror` / `anyhow` | 1/2 | MIT/Apache (→MIT) | errors |
| `notify` | 8 | MIT/Apache(→MIT) or CC0 | config hot-reload |
| `clap` | 4 | MIT/Apache (→MIT) | CLI args |
| `async-channel` | 2 | MIT/Apache (→MIT) | module→UI updates |

**System build deps (emerge at M1):** `gui-libs/gtk4-layer-shell`. (gtk4, wayland, glib already present.)

> ⚠️ **Open question for you (§7):** keeping the project **GPL-2.0-only** is fine with the above (all deps MIT/dual),
> but the Rust ecosystem is Apache-heavy, so future deps may force MIT-only choices. Relicensing to
> **GPL-2.0-or-later** would give breathing room (it's Apache-2.0-compatible via GPLv3). Your call — default is to
> stay GPL-2.0-only and elect MIT on every dep.

---

## 5. Milestones (branch + PR each; no direct commits to `main`)

- **M1 — `feat/scaffold`**: cargo workspace; emerge `gtk4-layer-shell`; a layer-shell bar window (top, per-monitor,
  exclusive zone); **clock** module; the **grid engine** + TOML config loader + `docs/config.md`; tracing/anyhow/thiserror
  wiring; unit tests for the grid engine and config parser. → PR. *(~bar appears with a working clock.)*
- **M2 — `feat/dwl-ipc`**: patch dwl (`dwl-ipc-unstable-v2` + `wlr-foreign-toplevel`), rebuild + re-login; `zdwl_ipc`
  client → **tags / layout / window-title** modules; foreign-toplevel client → **taskbar** (icon + truncated title,
  click-to-focus). → PR + screenshots. *(this is also the fix for your "something off" bar.)*
- **M3 — `feat/system-modules`**: **tray** (zbus SNI watcher), **pulseaudio** volume, **network** (NM via D-Bus),
  **cpu**+**memory** (/proc, zero deps), **launcher** button → wofi. → PR.
- **M4 — `feat/polish`**: ship `themes/nord.css` matching your current waybar; config **hot-reload** (`notify`);
  packaging (`cargo build --release`, install notes, swap `waybar` → `wafflebar` in `~/.config/dwl/startup.sh`);
  **perf measurement** in the PR body (target: idle CPU < 0.5%, RSS < 30 MB with all v1 modules).

If any milestone exceeds ~800 LOC of new code, it gets split.

---

## 6. Risks / watch-items
- **GDK ↔ wayland-client display sharing** (M2) — the trickiest integration; fallback is a second connection on a thread.
- **dwl re-patching** (M2) — additive patches, but it rebuilds the dwl you just got working; backups + re-login required.
  Monitor serial-positioning patch already in your `dwl.c` must be preserved across the re-patch.
- **Taskbar icons** — app_id → `.desktop` → icon-name resolution is imperfect for some apps; fallback to a generic icon.
- **PipeWire vs Pulse** — using `libpulse-binding` against `pipewire-pulse`; verify the daemon at M3.

---

## 7. One decision I need from you before M1
**License:** stay **GPL-2.0-only** (default; all deps MIT/dual, fully compatible) — or relicense to
**GPL-2.0-or-later** for Apache-2.0 headroom? *(Not blocking M1 scaffolding; easy to set before first real dep lands.)*

---

## Done = this file on `plan/initial`, PR open.

**Approve plan and start M1?**
