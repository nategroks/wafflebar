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
| `window#wafflebar` | the bar window — font + base color; **see paint-surface note below** |
| `#grid` | the placement grid — **the bar's visible background surface; paint it here** |
| `.module` | every plugin's container box |
| `.wb-button` | generic clickable wrapper (on every `Button`) — hover affordance |
| `.dim-label` | secondary/subtitle text and informational notes |
| `.placeholder` | a not-yet-implemented module kind |

**Paint the bar background on `#grid`, not `window#wafflebar`.** In a layer-shell bar the visible
surface is the `#grid` container that holds the modules — it sits on top of the window node. A
non-transparent system GTK theme paints `#grid`, covering any `background-color` set only on
`window#wafflebar`, so a theme that paints just the window shows system-theme bleed-through. Set the
bar background (and base text `color`) on `#grid`; `window#wafflebar` carries font/base only.

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
`.vol-label`, and `.muted`. **bluetooth** — `.bluetooth`, `.bt-icon`, and the click-out menu
`.bt-menu` with `.bt-menu-power` (the toggle row) / `.bt-menu-item` (device rows). (cpu/memory carry
the shared load buckets below.)

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

## Settings window

The preferences window (`prefs.rs`) is themed to match the panel — its root carries `.settings`, so
themes style `.settings` (background/text), `.settings row:selected`, `.settings entry`/`spinbutton`,
and `.settings button` (incl. `.destructive-action`). (Earlier it was deliberately native-GTK; now it
matches the apps menu for a consistent look.)

## Verifying a theme actually applies

Theme changes must be verified against a system whose GTK theme is **distinct** from the wafflebar
theme under test — otherwise a system theme that resembles the wafflebar theme masks
theme-application bugs (this is how the original "themes never applied under the `v4_12` feature flag"
bug stayed hidden: the dev host ran a system-wide Nord GTK theme that bled through). Practical rules:

- **Don't trust `GTK_THEME=<name>` blindly.** GTK4 silently falls through to its compiled-in Adwaita
  when a named theme lacks `gtk-4.0/gtk.css`. Take a control screenshot of a plain GTK window first to
  confirm the baseline actually switched.
- **Pixel-sample, don't eyeball.** Screenshot the bar (`grim`) and read the dominant background RGB;
  compare against the theme's expected value (e.g. Nord `#2e3440`, Dawn `#fdf6e3`), not "looks close."
- **Use a garish probe as the airtight discriminator.** A theme like `#grid { background: #00ffff; }`
  produces a color no system theme would — if the bar is cyan, wafflebar's CSS definitively won.
- **An empty theme file is the "no wafflebar CSS" baseline.** It shows the raw system surface, proving
  the real themes do work rather than coinciding with the system.
