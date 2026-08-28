# wafflebar configuration

Config lives at `$XDG_CONFIG_HOME/wafflebar/config.toml` (usually `~/.config/wafflebar/config.toml`),
or pass `--config <FILE>`. Format is TOML. If no config is found, a built-in default (a single
centered clock) is used.

> **Schema version.** Every config carries `schema = N` at the top. The current version is **1**.
> wafflebar refuses to load a config whose `schema` is newer than it understands.

## Top-level

```toml
schema = 1     # required-ish; defaults to the current version if omitted
```

## `[bar]`

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `monitor` | string | `"primary"` | `"primary"`/`"left"`/`"center"`/`"right"` (by layout position — survives NVIDIA connector renaming), `"all"` (one bar per monitor), or an exact connector (`DP-1`) / model name. Re-evaluated on hotplug: bars appear on new outputs and vanish with departed ones, and a positional selection moves when the layout changes (a new left-most display, say). Ordering is total — `x`, then `y`, then connector — so it never depends on the order the compositor announced outputs in. See `crates/wafflebar-core/src/outputs.rs`. |
| `position` | `"top"` \| `"bottom"` | `"top"` | which screen edge the bar docks to |
| `layout` | `"pack"` \| `"grid"` | `"pack"` | how modules are arranged — see below |
| `height` | integer | `26` | bar thickness in px (also the layer-shell exclusive zone) |
| `icon_size` | integer | `0` | icon pixel size for bar glyphs; `0` auto-derives from `height` so icons track the bar's thickness, a non-zero value pins an explicit size |
| `spacing` | integer | `0` | extra px between adjacent modules |
| `length_percent` | integer | `100` | bar length as a % (1–100) of the monitor width; `<100` makes a shorter floating panel |
| `alignment` | `"start"` \| `"center"` \| `"end"` | `"center"` | where a shorter-than-full bar sits along its edge (ignored at `length_percent = 100`) |
| `reserve_space` | bool | `true` | reserve screen space (strut) so tiled windows avoid the bar; `false` lets windows extend under it |
| `keep_below` | bool | `false` | keep the bar below normal windows (layer `Bottom`) instead of above (`Top`) |
| `lock` | bool | `false` | lock the layout — Settings disables drag-reorder / add / remove (field edits still work) |
| `theme` | string | _(built-in Nord)_ | a built-in name — `nord`, `dawn`, `workbench`, `cde` — or a path to a GTK4 CSS file |

### `layout` — pack vs grid

- **`"pack"` (default)** — the xfce4-panel model. Modules are grouped by their [`align`](#modules):
  `"start"` packs flush to the left edge, `"end"` flush to the right, `"center"` is centered; the
  slack between groups is absorbed automatically. Modules are sized to their content (no stretched
  columns, no manual spacer). **`cell` / `colspan` / `rowspan` are ignored** in pack mode — order
  within each group follows module declaration order. This is what you want for a normal bar.
- **`"grid"`** — the explicit `rows × columns` track below: every module is placed at its `cell`,
  columns are equal width, and empty columns are reserved. Use this for multi-row bars or precise
  column control. **Any config with `[grid] rows > 1` uses the grid path regardless of `layout`.**

## `[grid]` (used when `layout = "grid"` or `rows > 1`)

Modules occupy cells on a `rows × columns` track. In the default `pack` layout this section is
ignored (kept only for validation).

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `rows` | integer | `1` | number of rows (≥ 1) |
| `columns` | integer | `12` | number of columns (≥ 1) |

## `[[modules]]`

One table per module, in any order.

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `type` | string | _(required)_ | module type — see below |
| `cell` | table | _(required)_ | `{ row, col, rowspan = 1, colspan = 1 }` (zero-based) — used by `layout = "grid"`; ignored (but still parsed) in `pack` |
| `align` | `"start"`\|`"center"`\|`"end"`\|`"fill"` | `"start"` | in `pack`: which group the module joins (start/center/end edge); in `grid`: alignment within its cell |
| _(others)_ | — | — | per-module options (e.g. `format`) live inline |

Placement is validated at startup: a module that runs past the grid edge, or overlaps another
module, is a hard error with a clear message.

### Module types

| `type` | Status | Options |
|--------|--------|---------|
| `clock` | ✅ M1 | `format` (chrono strftime, default `"%a %d %b   %H:%M"`) |
| `tags` | 🔜 M2 | dwl tag indicator (needs dwl IPC patch) |
| `window` | 🔜 M2 | focused window title |
| `taskbar` | 🔜 M2 | all windows (needs foreign-toplevel patch) |
| `cpu`, `memory` | 🔜 M3 | `/proc`-based |
| `network` | ✅ | NetworkManager indicator; `max_chars` (integer, 0 = unlimited) truncates the connection-name label with an ellipsis |
| `pulseaudio` | 🔜 M3 | volume |
| `tray` | 🔜 M3 | StatusNotifierItem |
| `launcher` | 🔜 M3 | opens wofi |
| `controlcenter` | ✅ | Quick-settings flyout button; `icon` (theme name or PNG/SVG path), `label` (optional text). See below. |

Module types that aren't implemented yet render a dim placeholder labelled with their type.

#### `controlcenter`

A bar button that opens a quick-settings panel (the macOS/GNOME-style flyout): a header with the
clock/date, battery, and live power-draw/temperature/brightness stats; sliders for **volume**
(the audio backend), **microphone** (`wpctl`), **brightness** (`brightnessctl`/sysfs), the **CPU
power cap** (`intel-rapl` powercap, in watts), and the **battery charge limit**
(`charge_control_end_threshold`); expandable **Bluetooth** and **Wi-Fi** cards; a **stopwatch** and a
**countdown** timer; and a row of session actions (lock / suspend / reboot / power off via
`loginctl`/`systemctl`).

Each control reads live state and **hides itself when its backing mechanism is absent** (no
backlight, no `wpctl`, no `intel-rapl`, no charge threshold) — so the panel only ever shows controls
that actually do something on your hardware. The button subscribes to the audio/Bluetooth/network
backends, so adding it starts them (each "needs a restart" like the other backend modules).

```toml
[[modules]]
type = "controlcenter"
align = "end"
# icon = "preferences-system-symbolic"   # default; a theme name or a PNG/SVG path
# label = ""                              # optional text beside the icon
```

### Icons

wafflebar's own status/UI glyphs (apps-menu button, network, volume, cpu, memory, show-desktop)
are **bundled Lucide icons** (`assets/icons/`, ISC-licensed) — shipped as recolorable `*-symbolic`
SVGs that take their color from the theme, so they look consistent regardless of your system GTK
icon theme. Real *application* icons (launcher items, tasklist windows, tray) still come from the
system icon theme, so they stay recognizable.

Any config key that takes an icon (e.g. the apps-menu `icon`) accepts **either** a theme icon name
**or a file path** to a PNG/SVG (`~/` is expanded), so you can use a custom image:

```toml
[[modules]]
type = "appmenu"
icon = "~/.config/wafflebar/menu.png"   # or a name like "wb-appmenu-symbolic"
```

## Example A — reproduce a waybar-style left/center/right bar

```toml
schema = 1
[bar]
position = "top"
height = 26
[grid]
rows = 1
columns = 12
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
type = "tray"
cell = { row = 0, col = 11 }
align = "end"
```

## Example B — a 2-row griddy layout

```toml
schema = 1
[bar]
height = 48
[grid]
rows = 2
columns = 6
[[modules]]
type = "tags"
cell = { row = 0, col = 0, rowspan = 2 }
[[modules]]
type = "window"
cell = { row = 0, col = 1, colspan = 4 }
[[modules]]
type = "clock"
cell = { row = 1, col = 1, colspan = 2 }
format = "%H:%M:%S"
[[modules]]
type = "tray"
cell = { row = 0, col = 5, rowspan = 2 }
```
