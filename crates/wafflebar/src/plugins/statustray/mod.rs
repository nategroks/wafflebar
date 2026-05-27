//! StatusNotifier (system tray) plugin. D2a: each visible item renders as an icon button;
//! left-click emits `Activate`. Menus (DBusMenu) are D2b; pixmap icons, scroll, attention/overlay
//! and Passive→hidden polish are D2c.
//!
//! Pure reducer: it sees only the typed `Vec<TrayItem>` the host's SNI backend derives (all D-Bus
//! lives in `backend.rs`) and emits typed `TrayCommand`s. Items are keyed by their stable Watcher
//! key so the keyed diff reuses widgets across the frequent re-publishes of a busy tray.

pub mod backend;

use wafflebar_core::{ActionId, Event, Plugin, Reaction, Topic, TrayCommand, TrayItem, View};

const ACTION_ACTIVATE: &str = "activate:";
const ACTION_MENU: &str = "menu:"; // "menu:<key>:<dbusmenu-id>"

pub struct StatusTray {
    /// The visible items, in registration order. Compared as a whole for the dirty check.
    items: Vec<TrayItem>,
}

impl StatusTray {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }
}

impl Default for StatusTray {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for StatusTray {
    fn id(&self) -> &str {
        "statustray"
    }

    fn subscribe(&self) -> Vec<Topic> {
        vec![Topic::Tray]
    }

    fn view(&self) -> View {
        if self.items.is_empty() {
            return View::Empty;
        }
        let buttons: Vec<View> = self
            .items
            .iter()
            .map(|item| {
                // Icon by themed name; placeholder when absent (pixmap icons are D2c).
                let icon = item.icon_name.as_deref().unwrap_or("application-x-executable");
                // Left-click → Activate; right-click → the DBusMenu context menu (B2b Button.menu).
                View::icon(icon, 16)
                    .with_class("tray-icon")
                    .button(ActionId::new(format!("{ACTION_ACTIVATE}{}", item.key)))
                    .with_menu(item.menu.clone())
                    .with_key(format!("tray:{}", item.key)) // stable identity for the keyed diff
                    .with_class("tray-item")
            })
            .collect();
        View::row(buttons, 2).with_class("module").with_class("statustray")
    }

    fn on_event(&mut self, ev: &Event) -> Reaction {
        let Event::Tray(items) = ev else {
            return Reaction::none();
        };
        // Backend re-publishes the full list on any item change; only show/hide visible items, and
        // only dirty when the visible set actually changed.
        let visible: Vec<TrayItem> = items.iter().filter(|i| i.status.visible()).cloned().collect();
        if visible == self.items {
            return Reaction::none();
        }
        self.items = visible;
        Reaction::dirty()
    }

    fn on_action(&mut self, action: &ActionId) -> Reaction {
        if let Some(key) = action.0.strip_prefix(ACTION_ACTIVATE) {
            return Reaction::tray(TrayCommand::Activate { key: key.to_string() });
        }
        if let Some(rest) = action.0.strip_prefix(ACTION_MENU) {
            // "<key>:<id>" — the key may contain ':'/'/', so the id is after the *last* ':'.
            if let Some((key, id)) = rest.rsplit_once(':') {
                if let Ok(id) = id.parse::<i32>() {
                    return Reaction::tray(TrayCommand::MenuClick { key: key.to_string(), id });
                }
            }
        }
        Reaction::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wafflebar_core::TrayStatus;

    fn item(key: &str, icon: Option<&str>, status: TrayStatus) -> TrayItem {
        TrayItem {
            key: key.into(),
            id: key.into(),
            title: key.into(),
            icon_name: icon.map(str::to_string),
            status,
            menu: Vec::new(),
        }
    }

    fn tray_keys(v: &View) -> Vec<String> {
        let View::Row { children, .. } = v else { panic!("expected row") };
        children
            .iter()
            .map(|c| match c {
                View::Button { key: Some(k), .. } => k.clone(),
                _ => panic!("expected keyed button"),
            })
            .collect()
    }

    #[test]
    fn renders_visible_items_as_keyed_buttons() {
        let mut t = StatusTray::new();
        let r = t.on_event(&Event::Tray(vec![
            item("a", Some("firefox"), TrayStatus::Active),
            item("b", None, TrayStatus::NeedsAttention),
        ]));
        assert!(r.dirty);
        assert_eq!(tray_keys(&t.view()), vec!["tray:a", "tray:b"]);
    }

    #[test]
    fn passive_items_are_hidden() {
        let mut t = StatusTray::new();
        t.on_event(&Event::Tray(vec![
            item("a", Some("x"), TrayStatus::Active),
            item("hidden", Some("y"), TrayStatus::Passive),
        ]));
        assert_eq!(tray_keys(&t.view()), vec!["tray:a"]);
    }

    #[test]
    fn empty_tray_renders_nothing() {
        let mut t = StatusTray::new();
        assert_eq!(t.view(), View::Empty);
        // A list of only-Passive items is also nothing visible.
        t.on_event(&Event::Tray(vec![item("p", Some("x"), TrayStatus::Passive)]));
        assert_eq!(t.view(), View::Empty);
    }

    #[test]
    fn republish_with_no_visible_change_does_not_dirty() {
        let mut t = StatusTray::new();
        let items = vec![item("a", Some("x"), TrayStatus::Active)];
        assert!(t.on_event(&Event::Tray(items.clone())).dirty);
        // Same visible set (a Passive item toggling elsewhere wouldn't change what's shown).
        let mut items2 = items.clone();
        items2.push(item("p", None, TrayStatus::Passive));
        assert!(!t.on_event(&Event::Tray(items2)).dirty, "adding a hidden item is no-op");
    }

    #[test]
    fn icon_change_dirties_and_updates() {
        let mut t = StatusTray::new();
        t.on_event(&Event::Tray(vec![item("a", Some("old"), TrayStatus::Active)]));
        assert!(t.on_event(&Event::Tray(vec![item("a", Some("new"), TrayStatus::Active)])).dirty);
    }

    #[test]
    fn missing_icon_uses_placeholder() {
        let mut t = StatusTray::new();
        t.on_event(&Event::Tray(vec![item("a", None, TrayStatus::Active)]));
        let View::Row { children, .. } = t.view() else { panic!("row") };
        let View::Button { child, .. } = &children[0] else { panic!("button") };
        assert!(matches!(&**child, View::Icon { name, .. } if name == "application-x-executable"));
    }

    #[test]
    fn click_activates_by_key() {
        let mut t = StatusTray::new();
        assert_eq!(
            t.on_action(&ActionId::new("activate:org.x.Item/StatusNotifierItem")).tray,
            vec![TrayCommand::Activate { key: "org.x.Item/StatusNotifierItem".into() }]
        );
        assert!(t.on_action(&ActionId::new("bogus")).tray.is_empty());
    }

    #[test]
    fn menu_click_parses_key_and_id() {
        let mut t = StatusTray::new();
        // The key contains ':' and '/'; the id is the integer after the *last* ':'.
        assert_eq!(
            t.on_action(&ActionId::new("menu:org.x:1/StatusNotifierItem:42")).tray,
            vec![TrayCommand::MenuClick { key: "org.x:1/StatusNotifierItem".into(), id: 42 }]
        );
        assert!(t.on_action(&ActionId::new("menu:no-id")).tray.is_empty());
    }

    #[test]
    fn item_menu_is_rendered_on_the_button() {
        use wafflebar_core::view::MenuItem;
        let mut t = StatusTray::new();
        let mut it = item("a", Some("x"), TrayStatus::Active);
        it.menu = vec![MenuItem::Item {
            label: "Quit".into(),
            action: ActionId::new("menu:a:7"),
        }];
        t.on_event(&Event::Tray(vec![it]));
        let View::Row { children, .. } = t.view() else { panic!("row") };
        match &children[0] {
            View::Button { menu, .. } => {
                assert_eq!(menu.len(), 1, "DBusMenu item rendered on the button's right-click menu");
            }
            _ => panic!("button"),
        }
    }
}
