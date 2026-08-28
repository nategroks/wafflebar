#!/bin/sh
# Install the retro desktop: fonts, the wafflebar preset, GTK/X11 app styling, and the deterministic
# monitor daemon. Everything is per-user; nothing here needs root.
#
# Every file it would overwrite is backed up to <file>.bak-<timestamp> first, so this is reversible.
#
# Usage:
#   scripts/install-desktop.sh                 # install everything (workbench palette)
#   scripts/install-desktop.sh --theme cde     # the CDE/Motif palette instead
#   scripts/install-desktop.sh --dry-run       # print what would happen, change nothing
#   scripts/install-desktop.sh --no-config     # skip wafflebar's config.toml (keep your own)
#   scripts/install-desktop.sh --only fonts    # one part: fonts | bar | gtk | x11 | monitors
set -eu

here=$(CDA=$(dirname "$0") && cd "$CDA/.." && pwd)
theme=workbench
dry=0
want_config=1
only=all
stamp=$(date +%Y%m%d-%H%M%S)

while [ $# -gt 0 ]; do
    case "$1" in
        --theme) theme=${2:?--theme needs a value}; shift 2 ;;
        --dry-run) dry=1; shift ;;
        --no-config) want_config=0; shift ;;
        --only) only=${2:?--only needs a value}; shift 2 ;;
        -h|--help) sed -n '1,15p' "$0"; exit 0 ;;
        *) echo "install-desktop: unknown argument: $1" >&2; exit 2 ;;
    esac
done

case "$theme" in
    workbench|cde) ;;
    *) echo "install-desktop: --theme must be 'workbench' or 'cde'" >&2; exit 2 ;;
esac

config_home=${XDG_CONFIG_HOME:-$HOME/.config}
data_home=${XDG_DATA_HOME:-$HOME/.local/share}
bin_home=$HOME/.local/bin

say() { printf '%s\n' "$*"; }
run() {
    if [ "$dry" -eq 1 ]; then
        say "  would: $*"
    else
        "$@"
    fi
}

# Copy with a timestamped backup of anything already there.
place() {
    src=$1; dst=$2
    if [ -e "$dst" ] && ! cmp -s "$src" "$dst"; then
        say "  backup $dst -> $dst.bak-$stamp"
        run cp -p "$dst" "$dst.bak-$stamp"
    fi
    run mkdir -p "$(dirname "$dst")"
    run install -m "${3:-0644}" "$src" "$dst"
    say "  $dst"
}

wants() { [ "$only" = all ] || [ "$only" = "$1" ]; }

# --- fonts -------------------------------------------------------------------
if wants fonts; then
    say "fonts:"
    if [ "$dry" -eq 1 ]; then
        say "  would: scripts/install-fonts.sh"
    else
        sh "$here/scripts/install-fonts.sh"
    fi
fi

# --- the bar -----------------------------------------------------------------
if wants bar && [ "$want_config" -eq 1 ]; then
    say "wafflebar:"
    tmp=$(mktemp)
    sed "s/^theme = \"workbench\".*/theme = \"$theme\"/" \
        "$here/themes/workbench-preset.toml" > "$tmp"
    place "$tmp" "$config_home/wafflebar/config.toml"
    rm -f "$tmp"
    say "  (theme = $theme; both themes are built in, no CSS file needed)"
fi

# --- GTK apps ----------------------------------------------------------------
if wants gtk; then
    say "gtk:"
    for v in 3.0 4.0; do
        src="$here/contrib/desktop/gtk-$v/gtk.css"
        tmp=$(mktemp)
        if [ "$theme" = cde ]; then
            # The shipped CSS is the Workbench palette with the CDE swap documented in its header;
            # apply that swap rather than keeping a second near-identical file in the tree.
            sed -e 's/#a0a0a0/#aeb2c3/g' -e 's/#d0d0d0/#d3d6e0/g' -e 's/#262626/#6c7080/g' \
                -e 's/#8c8c8c/#9296a8/g' -e 's/#6688bb/#a83e7a/g' -e 's/#b0b0b0/#bcc0cf/g' \
                "$src" > "$tmp"
        else
            cp "$src" "$tmp"
        fi
        place "$tmp" "$config_home/gtk-$v/gtk.css"
        rm -f "$tmp"
        place "$here/contrib/desktop/gtk-$v/settings.ini" "$config_home/gtk-$v/settings.ini"
    done
    say "  note: libadwaita apps ignore this by design — see contrib/README.md"
fi

# --- X11 / XWayland ----------------------------------------------------------
if wants x11; then
    say "x11:"
    place "$here/contrib/desktop/Xresources" "$HOME/.Xresources"
    say "  load with: xrdb -merge ~/.Xresources   (add it to your session startup)"
fi

# --- monitors ----------------------------------------------------------------
if wants monitors; then
    say "monitors:"
    place "$here/contrib/monitors/wb-monitors" "$bin_home/wb-monitors" 0755
    place "$here/contrib/monitors/wb-monitors.service" \
          "$config_home/systemd/user/wb-monitors.service"
    if [ ! -e "$config_home/wafflebar/monitors.toml" ]; then
        place "$here/contrib/monitors/monitors.toml.example" \
              "$config_home/wafflebar/monitors.toml"
        say "  EDIT IT: run '$bin_home/wb-monitors --identify' and paste your real displays in."
        say "  Until then the example's displays match nothing, and yours are simply ordered by"
        say "  identity — deterministic, but not the order you asked for."
    else
        say "  $config_home/wafflebar/monitors.toml exists; left alone"
    fi
    say "  enable with: systemctl --user daemon-reload && systemctl --user enable --now wb-monitors"
fi

say ""
if [ "$dry" -eq 1 ]; then
    say "dry run: nothing was changed."
else
    say "done. Restart wafflebar (and re-login for the GTK settings to be picked up everywhere)."
    say "Verify the font actually bound:  scripts/install-fonts.sh --verify"
fi
