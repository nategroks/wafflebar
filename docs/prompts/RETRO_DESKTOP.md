# Prompt — chiseled desktop: Workbench/CDE look, deterministic monitor order, Tengoku font

Paste this file (or one `## Task` section at a time — they are independent) to Claude Code from the
wafflebar repo root. It is written to be handed over verbatim; everything it asserts about this repo
was checked against the tree at the time of writing, with file:line anchors so drift is visible.

> **Status — mostly executed.** This started as a hand-off prompt and was then carried out in the
> same branch. What landed: `themes/workbench.css` + `themes/cde.css` (registered as built-ins in
> `crates/wafflebar/src/theme.rs`), `themes/workbench-preset.toml`, the GTK/X11/dwl configs under
> `contrib/desktop/`, the EDID-ordered display daemon `contrib/monitors/wb-monitors` with its
> tests, hotplug reconciliation in `crates/wafflebar/src/app.rs` over the new
> `crates/wafflebar-core/src/outputs.rs`, the bundled font plus `scripts/install-fonts.sh`, and
> `scripts/install-desktop.sh` to apply the lot. What did **not**: the dwl side is a snippet
> (`contrib/desktop/dwl-colors.h`) because `natewm-asm` is a separate repository, Qt styling is
> written up rather than shipped as a file, and the hardware gates below (a real cable swap, a
> physical unplug, a reboot) can only be closed on the actual desk. Sections still phrased as
> instructions are the specification the code was written against — read them as the contract, and
> as the brief for the parts that remain.

---

## 0. The look we are copying

The reference is a montage of three 1990s desktops: **Amiga Workbench 3.1** (grey face, blue accents,
chunky gadget squares in the title bars), **CDE/Motif** (a Help Viewer with a magenta active title
band, a File Manager, and the front-panel dock with the `One / Two / Three / Four` workspace
switcher), and **DeluxePaint** (a tool palette of small square bevelled buttons).

They share one visual grammar. Implement the grammar, not a screenshot pastiche:

1. **Flat face colours.** No gradients, no blur, no translucency, no drop shadows, no animation.
2. **Zero rounded corners.** `border-radius: 0` on literally everything.
3. **Every interactive surface is a 2px hard bevel** — light on top+left, dark on bottom+right, drawn
   in solid colours (not `box-shadow` softening). Pressed/active state **inverts** the bevel.
4. **Content areas are inset wells** — the same bevel reversed, so text sits in a recess.
5. **Title bands** are a solid colour strip with square gadget boxes at one or both ends; active vs
   inactive is signalled by the band colour alone, never by shadow or opacity.
6. **Separators are chisels:** one dark 1px line immediately followed by one light 1px line.
7. **Typography is grid-aligned bitmap-style, black on grey.** No thin weights, no letter-spacing.
8. **Icons are small pictograms** (16px), not font glyphs.

Anything that reads as "modern" — a hover fade, a 6px radius, a soft shadow — is a bug.

---

## Task A — retro themes for wafflebar

### A1. What to build

Two new themes, both complete (every class in `docs/STYLING.md` styled — that file is the styling
contract; the reference theme `themes/nord.css` shows the full inventory in practice):

- `themes/workbench.css` — Amiga Workbench 3.1 palette.
- `themes/cde.css` — CDE/Motif palette, magenta active accent.

Plus `themes/workbench-preset.toml`: a ready `config.toml` with the geometry the look needs
(see A4), in the shape of the existing `themes/natewm-preset.toml`.

### A2. Palettes (use these hexes; do not improvise)

**Workbench 3.1**

| Role | Hex | Used for |
|---|---|---|
| face | `#a0a0a0` | bar surface (`#grid`), module boxes |
| face-light | `#d0d0d0` | raised bevel top/left |
| face-dark | `#262626` | raised bevel bottom/right |
| face-hover | `#b0b0b0` | the only hover affordance — one step, no fade |
| well | `#8c8c8c` | inset content recesses |
| text | `#000000` | all label text |
| text-inv | `#ffffff` | text on the accent band |
| accent | `#6688bb` | active tag / focused task / title band |
| dim | `#6a6a6a` | empty tags, secondary text |
| warn / alert / ok | `#ee8822` / `#dd4422` / `#55aa77` | bucket **bands** (urgent tag, tray attention) |
| warn-text / alert-text / ok-text | `#8a4a00` / `#a01f0e` / `#1f6b3a` | bucket **label text** — the bright hues wash out on the `#8c8c8c` well |

**CDE / Motif**

| Role | Hex | Used for |
|---|---|---|
| face | `#aeb2c3` | bar surface, module boxes |
| face-light | `#d3d6e0` | raised bevel top/left |
| face-dark | `#6c7080` | raised bevel bottom/right |
| face-hover | `#bcc0cf` | hover, one step |
| well | `#9296a8` | inset recesses |
| text | `#000000` | label text |
| text-inv | `#ffffff` | text on the accent band (and on the dark `urgent` tag) |
| accent | `#a83e7a` | active title band, active tag (the Help Viewer magenta) |
| dim | `#7a7e8c` | empty tags, secondary text |
| warn / alert / ok | `#c07a1e` / `#a5202a` / `#4a7a4a` | bucket **bands** |
| warn-text / alert-text / ok-text | `#7a4a10` / `#8a1a22` / `#1f5c33` | bucket **label text** on the `#9296a8` well |

### A3. The bevel, in GTK4 CSS

GTK4 CSS has no mixins, so define the bevel once as a pair of rules and reuse the selectors. The
raised form:

```css
.wb-button {
    border-radius: 0;
    border-style: solid;
    border-width: 2px;
    border-top-color: #d3d6e0;    /* face-light */
    border-left-color: #d3d6e0;
    border-right-color: #6c7080;  /* face-dark */
    border-bottom-color: #6c7080;
    background-color: #aeb2c3;    /* face */
    background-image: none;       /* kill Adwaita's gradient */
    box-shadow: none;             /* kill Adwaita's shadow */
    transition: none;             /* kill Adwaita's fade */
    padding: 2px 6px;
}
```

The pressed/inset form is the same rule with the two colour pairs swapped. Apply it to
`.wb-button:active`, `.tag.active`, `.task.focused`, `.wb-showdesktop.active`, and to every content
well (`.clock`, `.window`, `.feedblock`, `.cc-stat`).

Rules that keep it honest:

- **Paint on `#grid`, not `window#wafflebar`** — a non-transparent system GTK theme paints `#grid`
  and covers the window node (`docs/STYLING.md`, "Paint the bar background on `#grid`").
- Reset Adwaita per selector: `background-image: none; box-shadow: none; text-shadow: none;
  transition: none; border-radius: 0;`. Adwaita reasserts these on states you did not name, so state
  selectors (`:hover`, `:active`, `:checked`, `:disabled`, `:focus`) each need their own reset.
- Hover must not fade or glow. Use a 1px change or nothing at all; Motif hover is essentially inert.
- Focus is a **1px dotted black outline** inset inside the bevel (`outline: 1px dotted #000000;
  outline-offset: -4px`), the Motif focus rectangle — not a coloured ring.
- Keep paddings even, and bar height − (2×border) an even number, so the 2px bevel never lands on a
  half pixel.

### A4. Geometry and per-module mapping

Bar: `height = 26`, `font-size: 12px` (see Task C for why 12), `icon_size = 16`, `spacing = 2`,
`layout = "pack"`, `position = "top"`, `reserve_space = true`.

| Module (classes from `docs/STYLING.md`) | Retro treatment |
|---|---|
| `#grid` | face colour, plus a 2px raised bevel on the whole bar so it reads as the CDE front panel |
| `.tags` / `.tag` | the front-panel workspace switcher: equal-width raised boxes; `.active` inverted to inset + accent band; `.occupied` a 2px accent underline; `.urgent` alert face with black text |
| `.clock` | inset well, black text on `well`, no seconds by default (the font is proportional — see C3) |
| `.window` (focused title) | inset well spanning the centre group, text truncated with `…` |
| `.tasklist` / `.task` | raised bevel per window, `.focused` inverted, `.minimized` text in a 50%-mixed grey; `.task-icon` 16px |
| `.launcher`, `.appmenu` | raised gadget box with the 16px pictogram; the popover is a flat 1px black-outlined Motif menu (`.appmenu-popover`), `row:selected` = accent band + `text-inv` |
| `.statustray` / `.tray-item` | raised boxes on a 16px grid; `.needs-attention` = alert face |
| `.cpu`, `.memory`, `.network`, `.volume`, `.bluetooth` | inset wells with a 16px icon + label; the shared buckets `.low/.normal/.high/.critical` colour the **text**, never the background (Workbench had no colour animation) |
| `.wb-separator` | `.line` = the 1px dark + 1px light chisel; `.handle` = the Motif sash (a 4px raised nub); `.dots` = 2px squares |
| `.notification` | a small Workbench alert window: 2px raised bevel, a title band in accent (`.notification-critical` in alert), `.notification-close` a square gadget box |
| `.controlcenter` panel | a Workbench window: title band, bevelled body, sliders re-styled so `scale trough` is an inset well and `scale slider` is a raised square gadget (no round handle) |
| `.settings` (prefs window) | same face/bevel language so Settings matches the panel |

Both themes must render sanely at `bar.theme = "workbench"` / `"cde"` **and** with a user
`theme.css` layered on top — that layering is existing behaviour, do not change it.

### A5. The windows around the bar

The bar is one of four surfaces. Before writing anything here, **check what is actually installed and
which repo owns it** — do not invent config for a compositor patch that is not in the tree.

1. **dwl window borders/titlebars** (lives in the `natewm-asm` repo, not this one — see
   `docs/NATEWM_MODE.md`): `borderpx = 2`; focused border = accent, unfocused = `face-dark`; if the
   titlebar patch is present, give it the same title band + square gadget boxes and the Tengoku font
   string at 12px. If that repo is not attached to the session, produce the exact diff as a patch
   file plus instructions rather than guessing at its contents.
2. **GTK3/GTK4 apps:** `~/.config/gtk-3.0/gtk.css` and `~/.config/gtk-4.0/gtk.css` carrying the same
   bevel rules, plus `gtk-font-name = Tengoku 12` in `settings.ini`. Note the known trap: libadwaita
   apps ignore user themes for their own widgets — say so plainly instead of pretending the theme is
   global.
3. **Qt apps:** `qt5ct`/`qt6ct` with a Motif-ish style and the same palette, if Qt apps are present.
4. **XWayland/X11 legacy apps:** `~/.Xresources` (`*background: #aeb2c3`, `*foreground: #000000`,
   `*font`), loaded with `xrdb -merge` at session start.

Deliver these as files under a new `contrib/desktop/` directory in this repo with a short README, so
the whole look is reproducible from one checkout.

---

## Task B — monitors auto-arrange, in order, whatever port they are in

### B1. The actual problem

Connector names (`DP-1`, `HDMI-A-1`, `DP-2`) are assigned by **port**, not by display. Move a cable
and the names swap. Anything keyed on connector names — a kanshi profile, a hardcoded `bar.monitor`,
a `wlr-randr` script — then arranges the desktop wrongly. wlroots compositors add outputs to the
layout in the order they appear, so plug order silently becomes screen order.

### B2. Requirements

- **R1 — identity is EDID, never connector.** Key every display on `(make, model, serial)`.
- **R2 — one ordered list is the source of truth**, e.g. `~/.config/natewm/monitors.toml`:

  ```toml
  [policy]
  align   = "top"       # top | center | bottom — how differing heights line up
  unknown = "append"    # append | disable — what to do with a display not listed below

  [[display]]
  rank   = 1            # 1 = left-most
  make   = "Dell Inc."
  model  = "DELL U2723QE"
  serial = "ABC123"
  mode   = "3840x2160@60"
  scale  = 1.5

  [[display]]
  rank   = 2
  make   = "LG Electronics"
  model  = "LG HDR 4K"
  serial = "DEF456"
  ```

  Get the real identity strings with `wlr-randr --json` (wlroots) — dump them into the file rather
  than typing them from the OSD.
- **R3 — re-apply on every hotplug**, output removal, and mode change, with no session restart.
- **R4 — deterministic fallback.** An unlisted display is appended right-most, ties broken by
  `(make, model, serial)` string order — never by arrival order, never randomly.
- **R5 — layout is computed, not stored.** `x` for rank *n* is the running sum of the scaled logical
  widths of ranks `1..n-1`; `y` from `policy.align`. No gaps, no overlaps, ever.
- **R6 — never end with zero enabled outputs.** If no ranked display is present, enable whatever is
  connected.
- **R7 — idempotent.** Applying twice changes nothing; if the current layout already matches the
  computed one, issue no commands at all (this is what stops a hotplug storm from flickering).

### B3. Implementation

Prefer a small daemon speaking `wlr-output-management-unstable-v1` (or, as the pragmatic first cut, a
loop that runs `wlr-randr --json`, computes the layout, and applies it with one `wlr-randr` call) over
kanshi profiles: kanshi needs a profile per plugged-in *subset*, which is combinatorial and exactly
the thing that breaks when a cable moves. If kanshi is already in use, generate its config from the
ordered list instead of hand-maintaining it.

This helper belongs in the WM repo (`natewm-asm`), not in wafflebar. If that repo is not attached,
write it as a standalone script under `contrib/monitors/` here plus a systemd user unit, and say
where it should ultimately live.

### B4. wafflebar's own gaps (this repo — fix these)

1. **No hotplug handling.** `build_bars` enumerates `display.monitors()` exactly once at startup
   (`crates/wafflebar/src/app.rs:87`) and never observes the list again. Plug a display in and it
   gets no bar; unplug one and a stale window survives. Fix: hold the `ListModel` and
   `connect_items_changed` → reconcile against a `HashMap<connector, ApplicationWindow>` — create
   bars for new outputs, destroy bars for departed ones, and **re-run `select_monitors`** so a
   positional selection (`"left"`, `"center"`, `"right"`) moves the bar when the left-most display
   changes.
2. **Ordering is not fully deterministic.** `select_monitors` sorts by `x` only
   (`crates/wafflebar/src/app.rs:118`); two outputs sharing an `x` (stacked vertically) order by
   enumeration accident. Fix: sort by `(x, y, connector)`.
3. **Make the ordering testable.** Extract the pure part — `fn order_outputs(&[OutputGeom]) ->
   Vec<usize>` over a GTK-free `OutputGeom { x, y, w, h, connector }` — into `wafflebar-core`, and
   unit-test: single output; two side-by-side; two stacked; three with the middle removed; identical
   geometry with different connectors. `select_monitors` then becomes a thin GTK adapter over it.
4. **Docs.** `docs/config.md` already documents `bar.monitor` positional selection as surviving
   NVIDIA connector renaming — extend that row to state the hotplug behaviour once it exists.

Keep the config schema at `1` unless you add a key; if you do, bump it and document it.

---

## Task C — the Tengoku font

### C1. Already landed in this repo (do not redo)

- `assets/fonts/Tengoku.ttf` — vendored, sha256
  `6a1ad038651fbf7e7fea32805f3b9c9000de3b2c99f8f0bb5ea9cce24295ba1c`.
- `assets/fonts/10-tengoku.conf` — fontconfig rule: antialias off, hinting off, weak fallbacks.
- `scripts/install-fonts.sh` — installs per-user (or `--system`) and **fails non-zero if `fc-match`
  hands back any family other than `Tengoku`**.
- `assets/fonts/README.md` — metrics, the licensing caveat (unknown; resolve before publishing).

### C2. What is left

Run `scripts/install-fonts.sh`, then wire the family into: the two new themes, the GTK
`settings.ini` files, and the dwl border/titlebar font string. Every `font-family` list keeps a
fallback (`"Tengoku", "Hurmit Nerd Font Mono", monospace`) because Tengoku has **no icon or
powerline coverage**.

### C3. Sizing — non-negotiable

Tengoku's design grid is `1024/12` units (unitsPerEm 1024; every advance width is a multiple of
85.33; asc/desc/gap 853/−171/85 → a 13px line box at 12px). One grid cell equals one device pixel
only at **12px**, so use `12px`, `24px`, or `36px` and nothing between. Off-grid sizes put stem edges
between pixels and, with antialiasing already disabled, the result is visibly ragged.

It is also **proportional, not monospace** (14 distinct advance widths, 196 glyphs). A
`%H:%M:%S` clock will shimmer as digits change width — default the clock format to `%a %d %b %H:%M`,
or keep a monospace face for `.clock` alone. State which choice you made.

---

## Verification gates — all must pass, and "looks close" is not a pass

This repo's docs already carry the discipline (`docs/STYLING.md` § "Verifying a theme actually
applies"; `docs/NATEWM_MODE.md` § "Hurmit binding"). Apply it:

1. **Font binding.** `scripts/install-fonts.sh --verify` exits 0 and names `Tengoku` — never trust a
   bare family string in CSS.
2. **CSS actually wins.** Run once with a probe theme (`#grid { background: #00ffff; }`). If the bar
   is not cyan, wafflebar's CSS is not applying and every later screenshot is meaningless. Do this on
   a system GTK theme **visibly different** from the theme under test.
3. **Pixel-sample the face.** `grim` the bar and read the dominant background RGB: it must equal
   `#a0a0a0` (workbench) / `#aeb2c3` (cde) exactly.
4. **Bevel gate.** Sample the 2px band at a button's top edge and its bottom edge: they must be
   face-light and face-dark exactly, and swap on `:active`. A bevel that samples equal on both edges
   is a rule that did not apply.
5. **Font grid gate.** Screenshot text at 12px, upscale 8× nearest-neighbour: stems must be integral
   pixel columns with no grey fringe. Grey fringe = antialiasing still on = the fontconfig rule is
   not being read.
6. **Monitor ordering gate.** (a) Nested `cage` with `WLR_WL_OUTPUTS=2`, then `=3`, screenshotting
   after each — bars appear on new outputs without a restart. (b) On real hardware: note the layout,
   then **physically swap two cables between ports** and confirm the arrangement is byte-identical
   (`wlr-randr --json` diff, not eyeball). (c) Unplug a display and confirm its bar is gone and the
   remaining layout has no gap. (d) Reboot and confirm persistence.
7. **Tests and lint.** `cargo test`, `cargo clippy --all-targets`, `cargo fmt --check` all clean; new
   ordering tests included.
8. **No-bloat gate.** No new runtime dependencies without naming the justification; record the
   dependency count and release binary size before/after, as the README's no-bloat budget requires.

Anything you could not verify live, say so explicitly and name what would verify it. Do not report a
gate as passed on the strength of the code looking right.

---

## Constraints

- New CSS classes are added to `docs/STYLING.md` **in the same commit** — that is the styling
  contract, not a nicety.
- Do not touch the NATEWM two-channel design (`docs/NATEWM_MODE.md`): two fds, two reducers, no
  multiplexing.
- Config schema stays at `1` unless a key is added; then bump and document in `docs/config.md`.
- One commit per task, conventional-commit style: `feat(themes): …`, `feat(app): …`,
  `chore(fonts): …`. No drive-by refactors of unrelated code.
- Release profile (`opt-level = "z"`, LTO, strip) stays as it is.

## Definition of done

- [ ] `themes/workbench.css` and `themes/cde.css` style every class in `docs/STYLING.md`.
- [ ] `themes/workbench-preset.toml` gives a working bar at `height = 26` / 12px Tengoku.
- [ ] `contrib/desktop/` reproduces the same look for GTK, Qt, and X11 apps; the dwl side is either
      applied in `natewm-asm` or delivered as a patch with instructions.
- [ ] Displays arrange left→right by the ranked EDID list, identically after any cable swap, with
      hotplug re-applying automatically and never landing on zero outputs.
- [ ] wafflebar creates and destroys bars on hotplug, and `order_outputs` is unit-tested in
      `wafflebar-core`.
- [ ] Tengoku binds via `fc-match`, renders on-grid at 12px, and is used by bar, GTK apps, and window
      titles, always with a declared fallback.
- [ ] All eight verification gates above are reported individually, each with the evidence
      (screenshot, sampled RGB, command output) that closed it.

---

## Appendix — condensed version

> Make wafflebar and my desktop look like 1990s Amiga Workbench 3.1 / CDE-Motif: flat grey faces,
> zero rounded corners, no gradients or shadows or animation, every control a 2px hard bevel (light
> top-left, dark bottom-right, inverted when pressed), content in inset wells, solid title bands with
> square gadget boxes, chisel-line separators, 16px pictogram icons. Ship `themes/workbench.css`
> (`#a0a0a0` face, `#6688bb` accent) and `themes/cde.css` (`#aeb2c3` face, `#a83e7a` accent), both
> styling every class in `docs/STYLING.md`, plus a matching preset TOML and GTK/Qt/X11 configs under
> `contrib/desktop/`. Separately: arrange my monitors left-to-right by a ranked list keyed on EDID
> make/model/serial — never connector names, which follow the port — recomputing x offsets on every
> hotplug, appending unknown displays right-most with a deterministic tie-break, and never leaving
> zero outputs enabled; in wafflebar itself, fix `build_bars` (`crates/wafflebar/src/app.rs:87`) so it
> watches `display.monitors()` for `items-changed` and adds/removes bars live, tie-break
> `select_monitors` on `(x, y, connector)`, and unit-test the ordering as a GTK-free function in
> `wafflebar-core`. Use the bundled Tengoku font at 12px only (its grid is 1024/12, so 12/24/36px are
> the only crisp sizes), always with a fallback family since it has no icon glyphs. Verify, don't
> assume: `fc-match` must name Tengoku; a cyan `#grid` probe must prove the CSS applies; `grim` +
> pixel-sample the face colour and both bevel edges; upscale text 8× to confirm antialiasing is off;
> test hotplug in nested `cage` with `WLR_WL_OUTPUTS=2` then `3`, and physically swap two cables to
> confirm the layout is unchanged.
