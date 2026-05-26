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
| `monitor` | string | `"primary"` | `"primary"`/`"left"`/`"center"`/`"right"` (by layout position — survives NVIDIA connector renaming), `"all"` (one bar per monitor), or an exact connector (`DP-1`) / model name |
| `position` | `"top"` \| `"bottom"` | `"top"` | which screen edge the bar docks to |
| `height` | integer | `26` | bar thickness in px (also the layer-shell exclusive zone) |
| `theme` | string | _(built-in Nord)_ | path to a GTK4 CSS file |

## `[grid]`

The bar is a grid, not fixed left/center/right slots. Modules occupy cells on a `rows × columns` track.

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `rows` | integer | `1` | number of rows (≥ 1) |
| `columns` | integer | `12` | number of columns (≥ 1) |

## `[[modules]]`

One table per module, in any order.

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `type` | string | _(required)_ | module type — see below |
| `cell` | table | _(required)_ | `{ row, col, rowspan = 1, colspan = 1 }` (zero-based) |
| `align` | `"start"`\|`"center"`\|`"end"`\|`"fill"` | `"start"` | widget alignment within its cell |
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
| `network` | 🔜 M3 | NetworkManager |
| `pulseaudio` | 🔜 M3 | volume |
| `tray` | 🔜 M3 | StatusNotifierItem |
| `launcher` | 🔜 M3 | opens wofi |

Module types that aren't implemented yet render a dim placeholder labelled with their type.

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
