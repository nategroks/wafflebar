# wb-monitors — displays in a fixed order, whatever port they are in

`wlr-randr`-driven layout for wlroots compositors (dwl, sway, river). Keys on **EDID identity**
(make/model/serial), not connector names, and recomputes the whole layout on every change.

## Why not just kanshi

kanshi matches a *profile* against the set of connected displays, so a desk with three displays
needs a profile per plugged-in subset — and each profile still lists outputs explicitly. This tool
takes one ranked list and derives every arrangement from it: displays are placed left to right in
rank order, each abutting the previous one's logical width, with anything unlisted appended in a
deterministic (not arrival-order) position. Two displays, one display, all three, in any ports, from
the same six lines of config.

## Use

```sh
wb-monitors --identify              # what your displays actually report — paste into the config
cp monitors.toml.example ~/.config/wafflebar/monitors.toml
wb-monitors --once --dry-run        # see the plan, change nothing
wb-monitors --once                  # apply it
systemctl --user enable --now wb-monitors.service   # apply on every change, for good
```

## The rules it enforces

- **Identity is EDID.** Connector names are assigned by port; moving a cable renames the output.
- **One ranked list decides the order** — every arrangement is derived from it, not stored per-setup.
- **Positions are computed**: x is the running sum of the *logical* (scale-divided) widths, so a
  display at scale 1.5 does not overlap its neighbour. y comes from `policy.align`.
- **Unlisted displays are appended deterministically**, sorted by identity then connector — never by
  the order they were announced in, which is the plug-order dependence this exists to remove.
- **It never leaves zero outputs enabled.** If no configured display is present, whatever is
  connected gets enabled instead of a black screen.
- **It is idempotent**: a layout that already matches produces no commands at all, so a hotplug
  storm does not make the screens flicker.

## Requirements

`wlr-randr` and Python 3.11+ (the config is TOML). `--json` is used when the installed wlr-randr
supports it (0.4+); on older builds the plain listing is parsed instead.

## Tests

```sh
python3 contrib/monitors/test_wb_monitors.py
```

Covers the placement logic — rank order beating announcement order, identical results after a port
swap, contiguous positions, scale affecting the neighbour's offset, the align policies, the
never-blank safety net, idempotence, and the 0.3.x text parser. The apply step itself needs a live
compositor; it was verified against a nested headless sway with three outputs scrambled into
overlapping positions.
