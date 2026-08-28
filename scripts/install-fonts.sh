#!/bin/sh
# Install wafflebar's bundled fonts, then *prove* they bound.
#
# The verify step is the point. A CSS `font-family: "Tengoku"` that fontconfig cannot resolve does
# not fail loudly — it silently falls back to something else and the bar just looks slightly wrong
# forever (docs/NATEWM_MODE.md, "Hurmit binding — fc-match closes the silent-failure trap"). So this
# script exits non-zero if fc-match hands back a different family than the one it installed.
#
# Usage:
#   scripts/install-fonts.sh            # per-user  (~/.local/share/fonts, ~/.config/fontconfig)
#   scripts/install-fonts.sh --system   # system    (/usr/local/share/fonts, /etc/fonts/conf.d) — needs root
#   scripts/install-fonts.sh --verify   # verify only, install nothing
set -eu

here=$(CDA=$(dirname "$0") && cd "$CDA/.." && pwd)
src="$here/assets/fonts"
mode=user
verify_only=0

for arg in "$@"; do
    case "$arg" in
        --system) mode=system ;;
        --verify) verify_only=1 ;;
        -h|--help) sed -n '1,14p' "$0"; exit 0 ;;
        *) echo "install-fonts: unknown argument: $arg" >&2; exit 2 ;;
    esac
done

if [ "$mode" = system ]; then
    fontdir=/usr/local/share/fonts/wafflebar
    confdir=/etc/fonts/conf.d
else
    fontdir="${XDG_DATA_HOME:-$HOME/.local/share}/fonts/wafflebar"
    confdir="${XDG_CONFIG_HOME:-$HOME/.config}/fontconfig/conf.d"
fi

command -v fc-cache >/dev/null 2>&1 || { echo "install-fonts: fontconfig (fc-cache) not found" >&2; exit 1; }
command -v fc-match >/dev/null 2>&1 || { echo "install-fonts: fontconfig (fc-match) not found" >&2; exit 1; }

if [ "$verify_only" -eq 0 ]; then
    mkdir -p "$fontdir" "$confdir"
    for f in "$src"/*.ttf "$src"/*.otf; do
        [ -e "$f" ] || continue
        install -m 0644 "$f" "$fontdir/"
        echo "installed $(basename "$f") -> $fontdir/"
    done
    for c in "$src"/*.conf; do
        [ -e "$c" ] || continue
        install -m 0644 "$c" "$confdir/"
        echo "installed $(basename "$c") -> $confdir/"
    done
    fc-cache -f "$fontdir" >/dev/null
    echo "fc-cache: rebuilt"
fi

# --- verify: every family we ship must resolve to itself, not to a fallback -------------------
status=0
for want in Tengoku; do
    got=$(fc-match -f '%{family}' "$want")
    file=$(fc-match -f '%{file}' "$want")
    case "$got" in
        *"$want"*) printf 'ok   %-12s -> %s (%s)\n' "$want" "$got" "$file" ;;
        *) printf 'FAIL %-12s -> %s (%s)  <-- fell back; the family is NOT installed\n' \
               "$want" "$got" "$file"; status=1 ;;
    esac
done

if [ "$status" -ne 0 ]; then
    echo >&2
    echo "install-fonts: at least one family did not bind. A theme naming it will silently use" >&2
    echo "the fallback above. Fix the install before trusting any screenshot." >&2
fi
exit "$status"
