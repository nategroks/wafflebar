# Styling reference

The CSS classes the renderer emits, and what each conveys. This is the **styling contract**: a theme
(`themes/*.css` or a user `theme.css`) styles these; a new plugin emits classes by this convention and
updates this doc in the same PR. The reference theme `themes/nord.css` styles every class here.

Theming model: `bar.theme = "nord"` selects a built-in theme (or a `/path.css`); an optional
`$XDG_CONFIG_HOME/wafflebar/theme.css` layers on top (at `PRIORITY_USER`, above the theme). Both
hot-reload. See `docs/UPSTREAM.md`.

## Structure (the host)

| Selector | Conveys |
|---|---|
| `window#wafflebar` | the bar window — background, base color, font |
| `#grid` | the placement grid |
| `.module` | every plugin's container box |
| `.wb-button` | generic clickable wrapper (on every `Button`) — hover affordance |
| `.dim-label` | secondary/subtitle text and informational notes |
| `.placeholder` | a not-yet-implemented module kind |

## Per-plugin

**clock** — `.clock` (the time text).

**tags** (dwl tags / sway workspaces) — `.tags` (row); `.tag` (each); state: `.active` (viewed),
`.occupied` (has clients), `.urgent`.

**window** — `.window` (focused window title).

**tasklist** — `.tasklist` (row); `.task` (per-window button), `.task-icon`, `.task-label`; state:
`.focused` (active window), `.minimized`.

**launcher** — `.launcher` (row); `.wb-launcher-button`, `.wb-launcher-icon`, `.wb-launcher-label`;
right-click menu via the shared `.wb-menu` / `.wb-menu-item`.

**statustray** — `.statustray` (row); `.tray-item` (per icon button), `.tray-icon`; state:
`.needs-attention` (SNI NeedsAttention).

**cpu** — `.cpu`, `.cpu-icon`, `.cpu-label`. **memory** — `.memory`, `.mem-icon`, `.mem-label`, and
`.swapping`. **network** — `.network`, `.net-icon`, `.net-label`. **volume** — `.volume`, `.vol-icon`,
`.vol-label`, and `.muted`. (cpu/memory carry the shared load buckets below.)

**showdesktop** — `.wb-showdesktop`; `.active` when engaged.

**separator** — `.wb-separator` with the style modifier `.transparent` / `.line` / `.handle` / `.dots`.

**appmenu** — `.appmenu` (bar button); `.appmenu-popover` (the host-rendered two-pane menu surface;
style its inner `row:selected` / `entry` via descendants).

## Notifications (host popups)

`.notifications` (the overlay stack); `.notification` (each popup) with the urgency modifier
`.notification-low` / `.notification-normal` (default) / `.notification-critical` — the convention is a
left-edge accent encoding urgency, applied identically across themes. Inside: `.notification-summary`,
`.notification-body`, `.notification-close`.

## Shared state classes

Used across the system plugins on bucketed values, so a theme styles them once:

| Class | Meaning |
|---|---|
| `.low` | quiet / low load (typically the default color) |
| `.normal` | nominal |
| `.high` | elevated (warn tint) |
| `.critical` | urgent (alert tint, often bold) |
| `.active` | selected / engaged (tags, showdesktop) |
| `.focused` | holds focus (tasklist) |
| `.occupied` | has content (tags) |
| `.urgent` | demands attention (tags) |

## Deliberately unstyled: the Settings window

The preferences window (`prefs.rs`) is **not** themed by wafflebar — it inherits the user's GTK theme,
so it looks like a native settings dialog rather than the panel. Its widgets emit standard GTK classes
(`window`, `entry`, `switch`, …); a user who wants to theme it can target those in their `theme.css`.
This is a deliberate non-style decision.
