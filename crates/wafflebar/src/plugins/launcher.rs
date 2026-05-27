//! Launcher module: a row of application buttons. Left-click launches the app; right-click opens
//! a context menu mirroring xfce4-panel's: the app name as the default-launch entry, a separator,
//! then the desktop file's `Actions=` (in file order — never re-sorted).
//!
//! Pure reducer: it resolves `.desktop` files at construction (host-side filesystem reads are fine
//! here — this is the binary crate), then only ever emits `View`s and `Launch` intents. The actual
//! activation (DBus or spawn) is the host executor's job; see `src/shell/executor.rs`.

use tracing::warn;
use wafflebar_core::{
    ActionId, ConfigField, DesktopApp, Event, MenuItem, Plugin, Reaction, Topic, View,
};

/// Default pixel size for launcher icons (override with `icon_size` in config).
const DEFAULT_ICON_SIZE: u32 = 20;

pub struct Launcher {
    apps: Vec<DesktopApp>,
    icon_size: u32,
}

impl Launcher {
    /// Build from config: `items = ["firefox.desktop", "/path/to/x.desktop", "org.gnome.Calculator"]`.
    /// Each entry is an absolute/relative path (contains `/`) or a desktop-file id resolved against
    /// the XDG application dirs. Unresolvable or non-launchable entries are dropped with a warning.
    pub fn new(cfg: &wafflebar_core::ModuleConfig) -> Self {
        let icon_size = cfg
            .opt_i64("icon_size")
            .filter(|n| *n > 0)
            .map(|n| n as u32)
            .unwrap_or(DEFAULT_ICON_SIZE);
        let apps = cfg
            .opt_str_list("items")
            .iter()
            .filter_map(|item| match resolve_item(item) {
                Some(app) => Some(app),
                None => {
                    warn!(item, "launcher: could not resolve a launchable .desktop entry");
                    None
                }
            })
            .collect();
        Self { apps, icon_size }
    }

    /// The button child: the app icon, or the app name as text when there's no `Icon=` key.
    /// (Middle tier — `Icon=` present but unresolvable in the theme → `application-x-executable`
    /// — is the renderer's job, since only it can see the icon theme.)
    fn button_child(&self, app: &DesktopApp) -> View {
        match &app.icon {
            Some(icon) => View::icon(icon, self.icon_size).with_class("wb-launcher-icon"),
            None => View::label(&app.name).with_class("wb-launcher-label"),
        }
    }

    /// The right-click context menu, or empty when the app has no actions (then left-click is the
    /// only affordance). Shape mirrors xfce4-panel: default-launch entry (app name) → separator →
    /// actions in `Actions=` order.
    fn menu_for(&self, idx: usize, app: &DesktopApp) -> Vec<MenuItem> {
        if app.actions.is_empty() {
            return Vec::new();
        }
        let mut menu = vec![
            MenuItem::Item {
                label: app.name.clone(),
                action: ActionId::new(format!("launch:{idx}")),
            },
            MenuItem::Separator,
        ];
        // Preserve desktop-file action order verbatim.
        for action in &app.actions {
            menu.push(MenuItem::Item {
                label: action.name.clone(),
                action: ActionId::new(format!("action:{idx}:{}", action.id)),
            });
        }
        menu
    }
}

impl Plugin for Launcher {
    fn id(&self) -> &str {
        "launcher"
    }

    fn subscribe(&self) -> Vec<Topic> {
        Vec::new() // static: app set is fixed at construction
    }

    fn view(&self) -> View {
        let buttons = self
            .apps
            .iter()
            .enumerate()
            .map(|(idx, app)| {
                self.button_child(app)
                    .button(ActionId::new(format!("launch:{idx}")))
                    .with_menu(self.menu_for(idx, app))
                    .with_class("wb-launcher-button")
            })
            .collect();
        View::row(buttons, 2).with_class("module").with_class("launcher")
    }

    fn on_event(&mut self, _ev: &Event) -> Reaction {
        Reaction::none()
    }

    fn on_action(&mut self, action: &ActionId) -> Reaction {
        match parse_action(&action.0) {
            Some(Action::Launch(idx)) => self.do_launch(idx, None),
            Some(Action::DesktopAction(idx, id)) => self.do_launch(idx, Some(&id)),
            None => {
                warn!(action = %action.0, "launcher: unrecognized action id");
                Reaction::none()
            }
        }
    }

    fn configure(&mut self, cfg: &wafflebar_core::ModuleConfig) -> Reaction {
        // Config-only: reconstruct, re-resolving the .desktop `items` against the new config.
        *self = Self::new(cfg);
        Reaction::dirty()
    }

    fn config_schema(&self) -> Vec<ConfigField> {
        // The items list is rendered by the host as an add/remove/reorder editor with a `.desktop`
        // application picker (F4).
        vec![ConfigField::desktop_list("items", "Items")]
    }
}

impl Launcher {
    fn do_launch(&self, idx: usize, action: Option<&str>) -> Reaction {
        let Some(app) = self.apps.get(idx) else {
            // Stale index (e.g. a rebuilt view raced an old click) — log and no-op, never panic.
            warn!(idx, "launcher: launch for out-of-range app index");
            return Reaction::none();
        };
        match app.launch(action, &[]) {
            Ok(intent) => Reaction::launch(intent),
            Err(e) => {
                // Stale/missing action id, or an entry with no Exec= — log and no-op.
                warn!(app = %app.name, ?action, error = %e, "launcher: could not build launch intent");
                Reaction::none()
            }
        }
    }
}

enum Action {
    Launch(usize),
    DesktopAction(usize, String),
}

/// Parse `"launch:N"` / `"action:N:ID"` action ids. Action IDs may contain `:`, so the desktop
/// action id is everything after the second `:`.
fn parse_action(s: &str) -> Option<Action> {
    if let Some(n) = s.strip_prefix("launch:") {
        return n.parse().ok().map(Action::Launch);
    }
    if let Some(rest) = s.strip_prefix("action:") {
        let (idx, id) = rest.split_once(':')?;
        return idx.parse().ok().map(|n| Action::DesktopAction(n, id.to_string()));
    }
    None
}

/// Resolve a config `items` entry to a launchable [`DesktopApp`], or `None`.
fn resolve_item(item: &str) -> Option<DesktopApp> {
    if item.contains('/') {
        return DesktopApp::load(item).ok().flatten();
    }
    let id = if item.ends_with(".desktop") {
        item.to_string()
    } else {
        format!("{item}.desktop")
    };
    for dir in wafflebar_core::application_dirs() {
        let path = dir.join(&id);
        if path.is_file() {
            if let Ok(Some(app)) = DesktopApp::load(&path) {
                return Some(app);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use wafflebar_core::{Cell, ModuleConfig};

    fn write_desktop(dir: &std::path::Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn cfg_with(items: Vec<String>) -> ModuleConfig {
        let mut options = std::collections::BTreeMap::new();
        options.insert(
            "items".to_string(),
            toml::Value::Array(items.into_iter().map(toml::Value::String).collect()),
        );
        ModuleConfig {
            kind: "launcher".into(),
            cell: Cell { row: 0, col: 0, rowspan: 1, colspan: 1 },
            align: Default::default(),
            options,
        }
    }

    #[test]
    fn parses_action_ids() {
        assert!(matches!(parse_action("launch:3"), Some(Action::Launch(3))));
        match parse_action("action:2:NewWindow") {
            Some(Action::DesktopAction(2, id)) => assert_eq!(id, "NewWindow"),
            _ => panic!("expected desktop action"),
        }
        // Action id containing a colon survives.
        match parse_action("action:0:foo:bar") {
            Some(Action::DesktopAction(0, id)) => assert_eq!(id, "foo:bar"),
            _ => panic!("expected desktop action with colon"),
        }
        assert!(parse_action("bogus").is_none());
        assert!(parse_action("launch:notanumber").is_none());
    }

    #[test]
    fn configure_re_resolves_items() {
        let dir = tempdir();
        let path = write_desktop(
            dir.path(),
            "app.desktop",
            "[Desktop Entry]\nType=Application\nName=App\nExec=app\n",
        );
        let mut l = Launcher::new(&cfg_with(vec![path]));
        assert_eq!(l.apps.len(), 1);
        let r = l.configure(&cfg_with(vec![])); // items removed
        assert!(r.dirty);
        assert_eq!(l.apps.len(), 0, "reconfigure re-resolves the items list");
    }

    #[test]
    fn menu_preserves_actions_order() {
        // `Actions=NewWindow;NewIncognito;` must render in *that* order, not alphabetized.
        let dir = tempdir();
        let path = write_desktop(
            dir.path(),
            "browser.desktop",
            "[Desktop Entry]\nType=Application\nName=Browser\nExec=browser\nIcon=web-browser\n\
             Actions=NewWindow;NewIncognito;\n\n\
             [Desktop Action NewWindow]\nName=New Window\nExec=browser --new-window\n\n\
             [Desktop Action NewIncognito]\nName=New Incognito\nExec=browser --incognito\n",
        );
        let launcher = Launcher::new(&cfg_with(vec![path]));
        let menu = launcher.menu_for(0, &launcher.apps[0]);
        // [default-launch, separator, NewWindow, NewIncognito]
        assert_eq!(menu.len(), 4);
        assert!(matches!(&menu[0], MenuItem::Item { label, .. } if label == "Browser"));
        assert!(matches!(menu[1], MenuItem::Separator));
        match (&menu[2], &menu[3]) {
            (MenuItem::Item { label: a, action: aa }, MenuItem::Item { label: b, action: ba }) => {
                assert_eq!(a, "New Window");
                assert_eq!(aa, &ActionId::new("action:0:NewWindow"));
                assert_eq!(b, "New Incognito");
                assert_eq!(ba, &ActionId::new("action:0:NewIncognito"));
            }
            _ => panic!("expected two action items in file order"),
        }
    }

    #[test]
    fn no_actions_means_no_menu() {
        let dir = tempdir();
        let path = write_desktop(
            dir.path(),
            "plain.desktop",
            "[Desktop Entry]\nType=Application\nName=Plain\nExec=plain\nIcon=plain\n",
        );
        let launcher = Launcher::new(&cfg_with(vec![path]));
        assert!(launcher.menu_for(0, &launcher.apps[0]).is_empty());
    }

    #[test]
    fn button_child_falls_back_to_name_without_icon() {
        let dir = tempdir();
        let with_icon = write_desktop(
            dir.path(),
            "icon.desktop",
            "[Desktop Entry]\nType=Application\nName=Iconned\nExec=x\nIcon=some-icon\n",
        );
        let no_icon = write_desktop(
            dir.path(),
            "noicon.desktop",
            "[Desktop Entry]\nType=Application\nName=NoIcon\nExec=x\n",
        );
        let launcher = Launcher::new(&cfg_with(vec![with_icon, no_icon]));
        assert!(matches!(launcher.button_child(&launcher.apps[0]), View::Icon { .. }));
        assert!(matches!(launcher.button_child(&launcher.apps[1]), View::Label { text, .. } if text == "NoIcon"));
    }

    #[test]
    fn stale_index_no_ops() {
        let launcher = Launcher::new(&cfg_with(vec![]));
        assert_eq!(launcher.do_launch(99, None), Reaction::none());
    }

    #[test]
    fn stale_action_id_no_ops() {
        let dir = tempdir();
        let path = write_desktop(
            dir.path(),
            "app.desktop",
            "[Desktop Entry]\nType=Application\nName=App\nExec=app\n",
        );
        let launcher = Launcher::new(&cfg_with(vec![path]));
        // Action id not present in the file → Err inside launch() → no-op, no panic.
        assert_eq!(launcher.do_launch(0, Some("ghost")), Reaction::none());
    }

    // Minimal tempdir without pulling a dev-dependency.
    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir() -> TempDir {
        let mut p = std::env::temp_dir();
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("wafflebar-launcher-test-{n}-{:?}", std::thread::current().id()));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}
