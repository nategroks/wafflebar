# Bundled fonts

## Tengoku.ttf

Vendored so a fresh checkout can install it with `scripts/install-fonts.sh` (see below) and so the
retro themes have a font they can actually name. It is **not** compiled into the binary — wafflebar
resolves fonts through fontconfig like any GTK app, so the file has to be installed on the system.

### What fontconfig sees

```
family:        "Tengoku"          # this exact string is what a CSS font-family must say
style:         "Medium"
postscriptname: Tengoku
fontformat:    TrueType
sha256:        6a1ad038651fbf7e7fea32805f3b9c9000de3b2c99f8f0bb5ea9cce24295ba1c
```

### Metrics — why 12px and its multiples

| Value | Number | Meaning |
|---|---|---|
| unitsPerEm | 1024 | — |
| advance widths | multiples of 1024/12 (85.33) | the design grid is **1/12 em**, i.e. one grid cell = 1px at `font-size: 12px` |
| ascender / descender / lineGap | 853 / −171 / 85 | a 13px line box at 12px |
| glyphs / cmap | 196 | Latin-1 + a few extras; **no Nerd Font / powerline glyphs** |

Consequences for themes:

- Set `font-size` to **12px, 24px, or 36px**. Off-grid sizes (13px, 15px) land stem edges between
  device pixels and the font goes mushy — the whole point of a grid font is lost.
- The font is **proportional, not monospace** (14 distinct advance widths). Do not use it for the
  columns of a `%H:%M:%S` clock and expect the digits not to shimmer — either accept the jitter or
  keep a monospace face for the clock module.
- It has no icon glyph coverage. Keep a fallback in every `font-family` list, and keep wafflebar's
  bundled Lucide SVG icons (`assets/icons/`) for the status glyphs — they are images, not glyphs, so
  they are unaffected.
- Antialiasing and hinting should be **off** for this face; `10-tengoku.conf` does that, and
  `install-fonts.sh` installs it.

### Licensing — unresolved

The archive shipped as a bare `Tengoku.ttf` with no license file, and the `name` table carries no
license or vendor URL (foundry string is `2ttf`, a conversion-tool artifact rather than a
attribution). This repository is GPL-2.0-only; a font of unknown license is vendored here on the
assumption that redistribution is permitted, which has **not** been verified. Before this repo is
published or packaged, either confirm the license and record it in this file, or drop the binary and
have `install-fonts.sh` take a path to a locally supplied copy.
