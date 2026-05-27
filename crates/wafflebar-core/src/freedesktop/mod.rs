//! freedesktop `.desktop` support: parse → filter → a serializable [`Launch`] intent.
//!
//! Shared infrastructure for the `launcher` plugin (B2) and the applications menu (E1/E2). The
//! [`Launch`] type is pure data (no `zbus`/`Command` handles) so it crosses the plugin → host
//! boundary cleanly — the host executor owns the live handles (see `docs/ARCHITECTURE.md`).
//!
//! Parsing/metadata come from the `freedesktop-desktop-entry` crate; the buggy bits (Exec field
//! codes, locale env chain) are replaced by [`exec`] and [`locale`].

pub mod exec;
pub mod locale;

use std::path::Path;

use freedesktop_desktop_entry::DesktopEntry;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("parsing desktop entry {path}: {msg}")]
    Parse { path: String, msg: String },
}

/// A `[Desktop Action <id>]` entry (right-click menu item).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopAction {
    pub id: String,
    pub name: String,
    pub exec: Option<String>,
    pub icon: Option<String>,
}

/// A parsed, launchable application (already filtered: `Type=Application`, not `Hidden`,
/// `TryExec` satisfied, shown in the current desktop).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopApp {
    pub path: String,
    pub file_id: String,
    pub name: String,
    pub icon: Option<String>,
    pub actions: Vec<DesktopAction>,
    exec: Option<String>,
    dbus_activatable: bool,
}

/// A serializable launch intent the host executor performs. **No live handles** — pure data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Launch {
    /// DBus activation: `org.freedesktop.Application` on `bus_name` at `object_path`.
    /// `fallback_exec` is the expanded `Exec=` argv the executor spawns if DBus activation fails
    /// at runtime (the xfce4-panel/GIO behavior); empty if there's no usable `Exec=`.
    DBus {
        bus_name: String,
        object_path: String,
        action: Option<String>,
        files: Vec<String>,
        fallback_exec: Vec<String>,
    },
    /// Spawn argv directly (`Exec=` field codes already expanded).
    Exec { argv: Vec<String> },
}

impl DesktopApp {
    /// Load + filter a `.desktop` file. `Ok(None)` if it shouldn't be launched here.
    pub fn load(path: impl AsRef<Path>) -> Result<Option<Self>, Error> {
        let path = path.as_ref();
        let locales = locale::from_env();
        let entry = DesktopEntry::from_path(path.to_path_buf(), Some(locales.as_slice()))
            .map_err(|e| Error::Parse {
                path: path.display().to_string(),
                msg: e.to_string(),
            })?;
        Ok(Self::from_entry(&entry, &locales))
    }

    /// Filter + extract from an already-parsed entry. Returns `None` if not launchable.
    /// (NoDisplay is intentionally *not* filtered — that gates menu visibility, an E1 concern;
    /// a launcher pins a specific app regardless.)
    pub fn from_entry<L: AsRef<str>>(entry: &DesktopEntry, locales: &[L]) -> Option<Self> {
        if entry.type_() != Some("Application") {
            return None; // Link/Directory aren't launchable applications
        }
        if entry.hidden() {
            return None; // Hidden = "deleted", treat as absent
        }
        if let Some(te) = entry.try_exec() {
            if !binary_available(te) {
                return None; // TryExec gate: the program isn't installed
            }
        }
        if !show_in_current_desktop(entry) {
            return None;
        }
        let path = entry.path.to_string_lossy().to_string();
        let file_id = file_id(&path);
        let actions = entry
            .actions()
            .unwrap_or_default()
            .iter()
            .filter(|a| !a.is_empty())
            .map(|a| DesktopAction {
                id: a.to_string(),
                name: entry
                    .action_name(a, locales)
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| a.to_string()),
                exec: entry.action_exec(a).map(str::to_string),
                icon: entry.action_entry(a, "Icon").map(str::to_string),
            })
            .collect();
        Some(DesktopApp {
            name: entry
                .name(locales)
                .map(|c| c.to_string())
                .unwrap_or_else(|| file_id.clone()),
            icon: entry.icon().map(str::to_string),
            exec: entry.exec().map(str::to_string),
            dbus_activatable: entry.dbus_activatable(),
            actions,
            path,
            file_id,
        })
    }

    /// Build a launch intent for the app (or one of its `Actions=`), passing `files`.
    ///
    /// **Actions always go via `Exec=`.** The spec's DBus `ActivateAction` is stubbed in v1
    /// (TODO: E-phase), so an action with no `Exec=` is unlaunchable and yields `ExecError::Empty`.
    ///
    /// The **primary** launch prefers DBus activation when declared *and* the app id is a valid
    /// bus name, carrying the expanded `Exec=` as `fallback_exec` so the host executor can spawn
    /// it if DBus activation fails at runtime — a deliberate deviation we inherit from
    /// xfce4-panel/GIO. Otherwise it spawns `Exec=` directly.
    pub fn launch(&self, action: Option<&str>, files: &[String]) -> Result<Launch, exec::ExecError> {
        // Action launch: Exec= only (ActivateAction unimplemented in v1).
        if let Some(a) = action {
            let raw = self
                .actions
                .iter()
                .find(|x| x.id == a)
                .and_then(|x| x.exec.as_deref())
                .ok_or(exec::ExecError::Empty)?;
            return Ok(Launch::Exec {
                argv: exec::expand(raw, self.icon.as_deref(), Some(&self.name), &self.path, files)?,
            });
        }
        // Primary launch: expand Exec= once, reuse it as the DBus fallback or the direct argv.
        let exec_argv = self
            .exec
            .as_deref()
            .map(|raw| exec::expand(raw, self.icon.as_deref(), Some(&self.name), &self.path, files))
            .transpose()?;
        if self.dbus_activatable {
            if let Some(bus_name) = bus_name_from_id(&self.file_id) {
                return Ok(Launch::DBus {
                    object_path: object_path_from_bus(&bus_name),
                    bus_name,
                    action: None,
                    files: files.to_vec(),
                    fallback_exec: exec_argv.unwrap_or_default(),
                });
            }
        }
        Ok(Launch::Exec {
            argv: exec_argv.ok_or(exec::ExecError::Empty)?,
        })
    }
}

/// XDG `applications` dirs in lookup order: `$XDG_DATA_HOME` then `$XDG_DATA_DIRS` (spec defaults).
pub fn application_dirs() -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    let mut dirs = Vec::new();
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    if let Some(h) = data_home {
        dirs.push(h.join("applications"));
    }
    let data_dirs = std::env::var_os("XDG_DATA_DIRS")
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    for d in data_dirs.split(':').filter(|s| !s.is_empty()) {
        dirs.push(PathBuf::from(d).join("applications"));
    }
    dirs
}

/// Every launchable application across the XDG dirs, sorted by name. Deduped by desktop-file id
/// (the first dir in lookup order claims the id, per spec — a `~/.local` override hides a system
/// entry). `NoDisplay` entries are excluded (they're meant to stay out of menus — note the launcher
/// resolver *keeps* them for explicit references). This is the data layer for F4's application
/// picker; E reuses it and adds the Menu-Spec category tree on top.
pub fn list_applications() -> Vec<DesktopApp> {
    let locales = locale::from_env();
    let mut seen = std::collections::HashSet::new();
    let mut apps = Vec::new();
    for dir in application_dirs() {
        let Ok(read) = std::fs::read_dir(&dir) else { continue };
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let id = file_id(&path.to_string_lossy());
            if !seen.insert(id) {
                continue; // an earlier dir already claimed this id
            }
            let Ok(de) = DesktopEntry::from_path(path.clone(), Some(locales.as_slice())) else {
                continue;
            };
            if de.no_display() {
                continue; // hidden from menus (kept by the launcher only for explicit refs)
            }
            if let Some(app) = DesktopApp::from_entry(&de, &locales) {
                apps.push(app);
            }
        }
    }
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

/// Desktop file ID = filename without `.desktop`. (Full spec IDs encode nested
/// `applications/` subdirs as `dir-name`; that needs the menu root and is an E1 concern.)
fn file_id(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// A valid D-Bus well-known bus name: ≥2 dot-separated elements, each non-empty, made of
/// `[A-Za-z0-9_-]`, not starting with a digit. `firefox` → `None` (→ Exec fallback);
/// `org.gnome.Calculator` → `Some`.
fn bus_name_from_id(id: &str) -> Option<String> {
    let parts: Vec<&str> = id.split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    let ok = parts.iter().all(|p| {
        !p.is_empty()
            && !p.starts_with(|c: char| c.is_ascii_digit())
            && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    });
    ok.then(|| id.to_string())
}

/// Object path for an app bus name: `.`→`/`, prefixed `/`, and `-`→`_` (object-path elements
/// can't contain `-`). `org.gnome.Calculator` → `/org/gnome/Calculator`.
fn object_path_from_bus(bus: &str) -> String {
    let mut p = String::with_capacity(bus.len() + 1);
    p.push('/');
    p.push_str(&bus.replace('.', "/").replace('-', "_"));
    p
}

/// Is `program` runnable? Absolute/relative path → exists; bare name → found on `$PATH`.
fn binary_available(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(program).is_file()))
        .unwrap_or(false)
}

/// Evaluate `OnlyShowIn`/`NotShowIn` against the host's `$XDG_CURRENT_DESKTOP` (colon list).
///
/// wafflebar is a *panel*, not a desktop environment — so we honor the host's identity and must
/// never set `XDG_CURRENT_DESKTOP=wafflebar`. When it's unset (common on minimal tiling-WM
/// setups) we're permissive and show everything; filtering on an empty identity would wrongly
/// hide every `OnlyShowIn=` entry.
fn show_in_current_desktop(entry: &DesktopEntry) -> bool {
    let current: Vec<String> = std::env::var("XDG_CURRENT_DESKTOP")
        .ok()
        .map(|s| s.split(':').map(|x| x.to_string()).collect())
        .unwrap_or_default();
    if current.is_empty() {
        return true; // permissive when the host declares no desktop
    }
    let list = |key: &str| -> Vec<String> {
        entry
            .desktop_entry(key)
            .map(|v| v.split(';').filter(|s| !s.is_empty()).map(str::to_string).collect())
            .unwrap_or_default()
    };
    let intersects = |xs: &[String]| xs.iter().any(|x| current.iter().any(|c| c == x));
    let only = list("OnlyShowIn");
    if !only.is_empty() && !intersects(&only) {
        return false;
    }
    let not = list("NotShowIn");
    if intersects(&not) {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn parse(input: &str, locales: &[&str]) -> DesktopEntry {
        DesktopEntry::from_str(PathBuf::from("/t/app.desktop"), input, Some(locales))
            .expect("parse fixture")
    }

    // ---- locale audit: verify the crate's name() resolution given our chain ----
    #[test]
    fn locale_resolution_matches_spec() {
        let f = "[Desktop Entry]\nType=Application\nExec=x\nName=Default\nName[en_US]=US\nName[en]=English\n";
        let name = |loc: &str| {
            let chain = locale::locale_chain(loc);
            parse(f, &chain.iter().map(String::as_str).collect::<Vec<_>>())
                .name(&chain)
                .unwrap()
                .to_string()
        };
        assert_eq!(name("en_US.UTF-8"), "US"); // exact
        assert_eq!(name("en_GB.UTF-8"), "English"); // falls back to en
        assert_eq!(name("fr_FR.UTF-8"), "Default"); // no match -> default
    }

    // ---- filters ----
    fn app(input: &str) -> Option<DesktopApp> {
        DesktopApp::from_entry(&parse(input, &[] as &[&str]), &[] as &[&str])
    }

    #[test]
    fn filters_non_application_types() {
        assert!(app("[Desktop Entry]\nType=Link\nName=L\nURL=http://x\n").is_none());
        assert!(app("[Desktop Entry]\nType=Directory\nName=D\n").is_none());
    }

    #[test]
    fn filters_hidden() {
        assert!(app("[Desktop Entry]\nType=Application\nExec=x\nName=H\nHidden=true\n").is_none());
    }

    #[test]
    fn try_exec_gate() {
        assert!(app("[Desktop Entry]\nType=Application\nExec=x\nName=N\nTryExec=/no/such/bin\n").is_none());
        // `sh` is on PATH in any sane test env
        assert!(app("[Desktop Entry]\nType=Application\nExec=x\nName=N\nTryExec=sh\n").is_some());
    }

    #[test]
    fn malformed_is_error_not_panic() {
        let r = DesktopEntry::from_str(PathBuf::from("/t/x.desktop"), "not a desktop file", Some(&[] as &[&str]));
        // crate returns an entry with no [Desktop Entry] group; our filter rejects it (not Application)
        if let Ok(e) = r {
            assert!(DesktopApp::from_entry(&e, &[] as &[&str]).is_none());
        }
    }

    // ---- bus name / object path derivation ----
    #[test]
    fn bus_name_validation() {
        assert_eq!(bus_name_from_id("org.gnome.Calculator").as_deref(), Some("org.gnome.Calculator"));
        assert_eq!(bus_name_from_id("firefox"), None); // single element -> not a bus name
        assert_eq!(bus_name_from_id("org.7zip.x"), None); // element starts with digit
    }

    #[test]
    fn object_path_derivation() {
        assert_eq!(object_path_from_bus("org.gnome.Calculator"), "/org/gnome/Calculator");
        assert_eq!(object_path_from_bus("org.foo.bar-baz"), "/org/foo/bar_baz"); // '-' -> '_'
    }

    // ---- Launch round-trip (the two branches) ----
    #[test]
    fn dbus_activatable_yields_dbus_launch() {
        let e = DesktopEntry::from_str(
            PathBuf::from("/usr/share/applications/org.gnome.Calculator.desktop"),
            "[Desktop Entry]\nType=Application\nName=Calculator\nExec=gnome-calculator\nDBusActivatable=true\n",
            Some(&[] as &[&str]),
        )
        .unwrap();
        let app = DesktopApp::from_entry(&e, &[] as &[&str]).unwrap();
        assert_eq!(
            app.launch(None, &[]).unwrap(),
            Launch::DBus {
                bus_name: "org.gnome.Calculator".into(),
                object_path: "/org/gnome/Calculator".into(),
                action: None,
                files: vec![],
                fallback_exec: vec!["gnome-calculator".into()],
            }
        );
    }

    #[test]
    fn action_launch_uses_exec_not_dbus() {
        // Even a DBusActivatable app routes its Actions= through Exec= in v1 (ActivateAction stub).
        let e = DesktopEntry::from_str(
            PathBuf::from("/usr/share/applications/org.gnome.Calculator.desktop"),
            "[Desktop Entry]\nType=Application\nName=Calculator\nExec=gnome-calculator\nDBusActivatable=true\nActions=New;\n\n[Desktop Action New]\nName=New Window\nExec=gnome-calculator --new\n",
            Some(&[] as &[&str]),
        )
        .unwrap();
        let app = DesktopApp::from_entry(&e, &[] as &[&str]).unwrap();
        assert_eq!(
            app.launch(Some("New"), &[]).unwrap(),
            Launch::Exec { argv: vec!["gnome-calculator".into(), "--new".into()] }
        );
    }

    #[test]
    fn stale_action_id_yields_empty_err() {
        // An action ID not present in the desktop file is an error the plugin logs + no-ops on.
        let app = app("[Desktop Entry]\nType=Application\nName=X\nExec=x\n").unwrap();
        assert!(matches!(app.launch(Some("ghost"), &[]), Err(exec::ExecError::Empty)));
    }

    #[test]
    fn non_dbus_yields_exec_launch() {
        // Firefox-style: no DBusActivatable, Exec with %u and no files passed.
        let app = app("[Desktop Entry]\nType=Application\nName=Firefox\nExec=firefox %u\n").unwrap();
        assert_eq!(app.launch(None, &[]).unwrap(), Launch::Exec { argv: vec!["firefox".into()] });
    }

    #[test]
    fn dbus_declared_but_invalid_busname_falls_back_to_exec() {
        // DBusActivatable=true but file id "weirdapp" isn't a valid bus name -> Exec.
        let e = DesktopEntry::from_str(
            PathBuf::from("/x/weirdapp.desktop"),
            "[Desktop Entry]\nType=Application\nName=W\nExec=weird --run\nDBusActivatable=true\n",
            Some(&[] as &[&str]),
        )
        .unwrap();
        let app = DesktopApp::from_entry(&e, &[] as &[&str]).unwrap();
        assert_eq!(app.launch(None, &[]).unwrap(), Launch::Exec { argv: vec!["weird".into(), "--run".into()] });
    }
}
