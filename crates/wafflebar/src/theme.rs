//! Theme loading + hot-reload (CSS theming, PR2).
//!
//! Two stacked GTK CSS providers on the display: the **theme** (a named built-in or a file path) at
//! `PRIORITY_APPLICATION`, and an optional **user override** (`$XDG_CONFIG_HOME/wafflebar/theme.css`)
//! at `PRIORITY_USER` so a few lines layer on top of a curated theme without rewriting it. Both
//! hot-reload via the F2 watcher pattern (the second consumer): a `gio::FileMonitor` on the config
//! dir catches `bar.theme` edits (config.toml) and the override file, and a monitor on an external
//! theme path catches edits to it. **Single provider per layer** — reload replaces the provider's
//! content (`load_from_*`), never adds a new provider (which would accumulate).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gtk4::prelude::*;
use gtk4::{gdk, gio, glib, CssProvider};
use tracing::{debug, warn};
use wafflebar_core::Config;

const NORD_CSS: &str = include_str!("../../../themes/nord.css");
const DAWN_CSS: &str = include_str!("../../../themes/dawn.css");

/// A built-in theme by name, or `None` for an unknown name.
fn builtin(name: &str) -> Option<&'static str> {
    match name {
        "nord" => Some(NORD_CSS),
        "dawn" => Some(DAWN_CSS),
        _ => None,
    }
}

enum ThemeSource {
    Builtin(&'static str),
    Path(PathBuf),
}

struct Theming {
    theme: CssProvider,
    overlay: CssProvider,
    config_path: Option<PathBuf>,
    /// Monitors an external theme *file* (when `bar.theme` is a path); replaced on each reload so it
    /// tracks the current path and never accumulates.
    theme_monitor: RefCell<Option<gio::FileMonitor>>,
    /// Debounce timer shared by all watchers — an editor's write burst collapses to one reload.
    debounce: RefCell<Option<glib::SourceId>>,
}

/// Install the theme providers on `display` and start watching for live edits. Leaked for the
/// process lifetime (like the bar and the fd/config watches).
pub fn install(display: &gdk::Display, config_path: Option<&Path>) {
    let theme = CssProvider::new();
    let overlay = CssProvider::new();
    gtk4::style_context_add_provider_for_display(
        display,
        &theme,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    gtk4::style_context_add_provider_for_display(
        display,
        &overlay,
        gtk4::STYLE_PROVIDER_PRIORITY_USER, // above the theme, so a user override wins
    );

    let theming = Rc::new(Theming {
        theme,
        overlay,
        config_path: config_path.map(Path::to_path_buf),
        theme_monitor: RefCell::new(None),
        debounce: RefCell::new(None),
    });
    theming.reload();

    // Watch the config dir: `bar.theme` changes (config.toml) and the override file (theme.css),
    // which both live there. (An external theme path is watched separately, in `reload`.)
    if let Some(dir) = theming.config_dir() {
        if let Ok(monitor) = gio::File::for_path(&dir)
            .monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        {
            let weak = Rc::downgrade(&theming);
            monitor.connect_changed(move |_, _, _, _| {
                if let Some(t) = weak.upgrade() {
                    t.schedule();
                }
            });
            std::mem::forget(monitor);
        }
    }
    std::mem::forget(theming);
}

impl Theming {
    fn config_dir(&self) -> Option<PathBuf> {
        self.config_path.as_ref().and_then(|p| p.parent()).map(Path::to_path_buf)
    }

    /// Debounced reload (100ms, matching the F2 config watcher).
    fn schedule(self: &Rc<Self>) {
        if let Some(id) = self.debounce.borrow_mut().take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local(Duration::from_millis(100), move || {
            if let Some(this) = weak.upgrade() {
                *this.debounce.borrow_mut() = None;
                this.reload();
            }
            glib::ControlFlow::Break
        });
        *self.debounce.borrow_mut() = Some(id);
    }

    /// Re-evaluate from scratch: read the config, resolve the theme, (re)apply both layers, and
    /// (re)watch an external theme path. Idempotent — re-applying unchanged content is a no-op.
    fn reload(self: &Rc<Self>) {
        let config = self
            .config_path
            .as_ref()
            .and_then(|p| Config::load(p).ok())
            .unwrap_or_default();
        let config_dir = self.config_dir();

        match resolve(config.bar.theme.as_deref(), config_dir.as_deref()) {
            ThemeSource::Builtin(css) => {
                self.theme.load_from_string(css);
                *self.theme_monitor.borrow_mut() = None; // no external file to watch
            }
            ThemeSource::Path(path) => {
                self.theme.load_from_path(&path);
                self.watch_theme_path(&path);
            }
        }

        // Optional user override layer: present iff the file exists; empty content = no override.
        match config_dir.map(|d| d.join("theme.css")).filter(|p| p.exists()) {
            Some(path) => self.overlay.load_from_path(&path),
            None => self.overlay.load_from_string(""),
        }
        debug!("theme reloaded");
    }

    /// Watch an external theme file; replacing the previous monitor (drop = stop watching) so it
    /// tracks the current path with no accumulation.
    fn watch_theme_path(self: &Rc<Self>, path: &Path) {
        let monitor = match gio::File::for_path(path)
            .monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        {
            Ok(m) => m,
            Err(_) => {
                *self.theme_monitor.borrow_mut() = None;
                return;
            }
        };
        let weak = Rc::downgrade(self);
        monitor.connect_changed(move |_, _, _, _| {
            if let Some(t) = weak.upgrade() {
                t.schedule();
            }
        });
        *self.theme_monitor.borrow_mut() = Some(monitor);
    }
}

/// Resolve `bar.theme` to a source. Empty/unset → built-in Nord. A value that *looks like a path*
/// (contains `/` or ends `.css`) is a file (with `~`/relative-to-config-dir expansion; missing →
/// warn + Nord); otherwise a built-in name (unknown → warn + Nord).
fn resolve(bar_theme: Option<&str>, config_dir: Option<&Path>) -> ThemeSource {
    let Some(theme) = bar_theme.map(str::trim).filter(|s| !s.is_empty()) else {
        return ThemeSource::Builtin(NORD_CSS);
    };
    if theme.contains('/') || theme.ends_with(".css") {
        let path = expand_path(theme, config_dir);
        if path.exists() {
            return ThemeSource::Path(path);
        }
        warn!(theme, "theme file not found; using built-in nord");
        return ThemeSource::Builtin(NORD_CSS);
    }
    match builtin(theme) {
        Some(css) => ThemeSource::Builtin(css),
        None => {
            warn!(theme, "unknown theme (built-ins: nord, dawn); using nord");
            ThemeSource::Builtin(NORD_CSS)
        }
    }
}

/// Expand `~/`, and resolve a relative path against the config dir (not the cwd — that would
/// surprise users).
fn expand_path(theme: &str, config_dir: Option<&Path>) -> PathBuf {
    if let Some(rest) = theme.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    let path = Path::new(theme);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        config_dir.map(|d| d.join(path)).unwrap_or_else(|| path.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_picks_builtin_path_or_falls_back() {
        // Empty / unset → nord.
        assert!(matches!(resolve(None, None), ThemeSource::Builtin(_)));
        assert!(matches!(resolve(Some("  "), None), ThemeSource::Builtin(_)));
        // Known built-in name → builtin; unknown → nord fallback.
        assert!(matches!(resolve(Some("dawn"), None), ThemeSource::Builtin(_)));
        assert!(matches!(resolve(Some("noord"), None), ThemeSource::Builtin(_)));
        // A path-looking value that doesn't exist → nord fallback (not Path).
        assert!(matches!(resolve(Some("/nope/x.css"), None), ThemeSource::Builtin(_)));
    }

    #[test]
    fn expand_path_handles_tilde_relative_absolute() {
        let cfg = Path::new("/home/u/.config/wafflebar");
        assert_eq!(expand_path("/abs/t.css", Some(cfg)), PathBuf::from("/abs/t.css"));
        assert_eq!(expand_path("mine.css", Some(cfg)), cfg.join("mine.css"));
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(expand_path("~/t.css", None), PathBuf::from(home).join("t.css"));
        }
    }
}
