//! Live config reload (F2): watch the config file and re-apply edits without a restart.
//!
//! The reload pipeline: a `gio::FileMonitor` fires on edits → a ~100ms debounce collapses an
//! editor's write burst into one reparse → [`diff_config`] classifies the change → option-only
//! changes call `configure()` on just the affected plugins; structural changes are flagged.
//!
//! We watch the **containing directory**, not the file: editors (`vim :w`) save atomically
//! (write tempfile + rename over the original), which deletes the watched file and would kill a
//! file-level watch. A directory watch filtered to the config filename survives the rename.
//!
//! Both paths are live. Option-only changes call `configure()` on the affected plugins. Structural
//! changes (a `[[modules]]` entry added/removed/reordered or its `type`/placement changed, or any
//! `[bar]`/`[grid]` change) run the hot in-place rebuild (F2b): tear down all plugins, reconstruct
//! from the new config, swap the grid, and reconcile timers — see [`watch_config`]'s `rebuild`. The
//! audio/network/tray *backends* are not reconciled (they hang off the host's immutable sinks);
//! hot-starting them is deferred to F2c, and the rebuild warns when an edit needs one that isn't
//! running.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use tracing::{debug, warn};
use wafflebar_core::Config;

use crate::render::Host;

/// What a config edit requires.
#[derive(Debug, PartialEq, Eq)]
pub enum ReloadPlan {
    /// Nothing relevant changed.
    Unchanged,
    /// Only options on existing modules changed → `configure()` these slot indices.
    Reconfigure(Vec<usize>),
    /// The module set / order / kinds or `[bar]`/`[grid]` changed → needs a full rebuild (F2b).
    Structural,
}

/// Classify the difference between two configs. Pure (the tested core of F2).
pub fn diff_config(old: &Config, new: &Config) -> ReloadPlan {
    if old.bar != new.bar || old.grid != new.grid || old.modules.len() != new.modules.len() {
        return ReloadPlan::Structural;
    }
    let mut changed = Vec::new();
    for (i, (o, n)) in old.modules.iter().zip(&new.modules).enumerate() {
        // kind or placement change is structural (which plugin / where it sits); options are not.
        if o.kind != n.kind || o.cell != n.cell || o.align != n.align {
            return ReloadPlan::Structural;
        }
        if o.options != n.options {
            changed.push(i);
        }
    }
    if changed.is_empty() {
        ReloadPlan::Unchanged
    } else {
        ReloadPlan::Reconfigure(changed)
    }
}

struct ReloadState {
    cached: Config,
    debounce: Option<glib::SourceId>,
}

/// Start watching `path`; apply edits live — option changes via `host`, structural changes via
/// `rebuild` (F2b). The `FileMonitor` is leaked to live for the process (like the fd watch); cheap
/// and there's exactly one per bar.
pub fn watch_config(
    path: PathBuf,
    host: Rc<Host>,
    rebuild: Rc<dyn Fn(&Config)>,
    initial: Config,
) {
    let Some(dir) = path.parent().map(|p| p.to_path_buf()) else {
        return;
    };
    let filename = path.file_name().map(|f| f.to_os_string());
    let monitor = match gio::File::for_path(&dir)
        .monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
    {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, "config reload: could not watch config dir; live reload off");
            return;
        }
    };

    let state = Rc::new(RefCell::new(ReloadState { cached: initial, debounce: None }));
    monitor.connect_changed(move |_, changed, _other, _event| {
        // Only our config file (the directory watch sees siblings too).
        if changed.basename().map(|b| b.into_os_string()) != filename {
            return;
        }
        // Debounce: cancel any pending reload and schedule a fresh one. Editor write bursts
        // (delete+create+changed) collapse into a single reparse.
        let pending = state.borrow_mut().debounce.take();
        if let Some(id) = pending {
            id.remove();
        }
        let (state_cb, path_cb, host_cb, rebuild_cb) =
            (state.clone(), path.clone(), host.clone(), rebuild.clone());
        let id = glib::timeout_add_local(Duration::from_millis(100), move || {
            state_cb.borrow_mut().debounce = None;
            reload(&path_cb, &host_cb, &rebuild_cb, &state_cb);
            glib::ControlFlow::Break
        });
        state.borrow_mut().debounce = Some(id);
    });
    std::mem::forget(monitor); // lives for the process; nothing to clean up before exit
}

fn reload(
    path: &PathBuf,
    host: &Rc<Host>,
    rebuild: &Rc<dyn Fn(&Config)>,
    state: &Rc<RefCell<ReloadState>>,
) {
    let new = match Config::load(path) {
        Ok(c) => c,
        Err(e) => {
            // A mid-edit save with a typo'd quote, etc. — keep the running config, don't crash.
            warn!(error = %e, "config reload: parse failed; keeping previous config");
            return;
        }
    };
    let plan = diff_config(&state.borrow().cached, &new);
    match plan {
        ReloadPlan::Unchanged => return,
        ReloadPlan::Reconfigure(indices) => {
            debug!(?indices, "config reload: reconfiguring changed modules");
            for i in indices {
                // The slot index lines up with the module index (both are declaration order).
                host.configure_slot(i, &new.modules[i]);
            }
        }
        ReloadPlan::Structural => {
            // F2b: hot rebuild — tear down all plugins, reconstruct from the new config, swap the
            // grid in place, and reconcile timers. Backends aren't reconciled here (F2c); the
            // rebuild closure warns if an edit adds the first consumer of an unstarted backend.
            debug!("config reload: structural change — rebuilding plugins in place");
            rebuild(&new);
        }
    }
    state.borrow_mut().cached = new;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> Config {
        Config::parse(toml).unwrap()
    }

    const BASE: &str = "schema = 1\n\
        [[modules]]\ntype = \"clock\"\ncell = { row = 0, col = 0 }\nformat = \"%H:%M\"\n\
        [[modules]]\ntype = \"separator\"\ncell = { row = 0, col = 1 }\n";

    #[test]
    fn identical_config_is_unchanged() {
        assert_eq!(diff_config(&parse(BASE), &parse(BASE)), ReloadPlan::Unchanged);
    }

    #[test]
    fn option_change_reconfigures_only_that_module() {
        let edited = BASE.replace("%H:%M", "%H:%M:%S");
        assert_eq!(diff_config(&parse(BASE), &parse(&edited)), ReloadPlan::Reconfigure(vec![0]));
    }

    #[test]
    fn adding_a_module_is_structural() {
        let added = format!("{BASE}[[modules]]\ntype = \"cpu\"\ncell = {{ row = 0, col = 2 }}\n");
        assert_eq!(diff_config(&parse(BASE), &parse(&added)), ReloadPlan::Structural);
    }

    #[test]
    fn changing_a_kind_is_structural() {
        let changed = BASE.replacen("clock", "cpu", 1);
        assert_eq!(diff_config(&parse(BASE), &parse(&changed)), ReloadPlan::Structural);
    }

    #[test]
    fn moving_a_module_is_structural() {
        let moved = BASE.replace("col = 1", "col = 5");
        assert_eq!(diff_config(&parse(BASE), &parse(&moved)), ReloadPlan::Structural);
    }

    #[test]
    fn bar_change_is_structural() {
        let pos = format!("[bar]\nposition = \"bottom\"\n{BASE}");
        assert_eq!(diff_config(&parse(BASE), &parse(&pos)), ReloadPlan::Structural);
    }
}
