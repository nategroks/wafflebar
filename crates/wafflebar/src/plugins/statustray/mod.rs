//! StatusNotifier (system tray) plugin. D2a: each visible item renders as an icon button;
//! left-click emits `Activate`. Menus (DBusMenu) are D2b; pixmap icons, scroll, attention/overlay
//! and Passive→hidden polish are D2c.
//!
//! Pure reducer: it sees only the typed `Vec<TrayItem>` the host's SNI backend derives (all D-Bus
//! lives in `backend.rs`) and emits typed `TrayCommand`s. Items are keyed by their stable Watcher
//! key so the keyed diff reuses widgets across the frequent re-publishes of a busy tray.

pub mod backend;

use wafflebar_core::{
    ActionId, Event, Plugin, Reaction, Topic, TrayCommand, TrayItem, TrayStatus, View,
};

const ACTION_ACTIVATE: &str = "activate:";
const ACTION_SECONDARY: &str = "secondary:";
const ACTION_SCROLL_UP: &str = "scroll-up:";
const ACTION_SCROLL_DOWN: &str = "scroll-down:";
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
                let key = &item.key;
                // Themed name preferred; pixmap/theme-path fallbacks (renderer chooses); placeholder
                // if neither. Left = Activate, middle = SecondaryActivate, scroll = Scroll, right =
                // the DBusMenu (B2b Button.menu).
                let icon = item.icon_name.as_deref().unwrap_or("application-x-executable");
                let mut btn = View::icon(icon, 16)
                    .with_pixmap(item.icon_pixmap.clone())
                    .with_theme_path(item.icon_theme_path.clone())
                    .with_class("tray-icon")
                    .button(ActionId::new(format!("{ACTION_ACTIVATE}{key}")))
                    .with_menu(item.menu.clone())
                    .with_middle_click(ActionId::new(format!("{ACTION_SECONDARY}{key}")))
                    .with_scroll(
                        ActionId::new(format!("{ACTION_SCROLL_UP}{key}")),
                        ActionId::new(format!("{ACTION_SCROLL_DOWN}{key}")),
                    )
                    .with_key(format!("tray:{key}")) // stable identity for the keyed diff
                    .with_class("tray-item");
                if item.status == TrayStatus::NeedsAttention {
                    btn = btn.with_class("needs-attention");
                }
                btn
            })
            .collect();
        View::row(buttons, 2).with_class("module").with_class("statustray")
    }

    fn on_event(&mut self, ev: &Event) -> Reaction {
        let Event::Tray(items) = ev else {
            return Reaction::none();
        };
        // The backend re-publishes the full list on any change; dirty only when it actually moved.
        // TODO(F): hide Passive items behind a "show hidden" preference (needs Plugin::configure,
        // F3). Until then Passive renders like Active — without the toggle, hiding would make those
        // items unreachable, which is worse than showing them.
        if *items == self.items {
            return Reaction::none();
        }
        self.items = items.clone();
        Reaction::dirty()
    }

    fn on_action(&mut self, action: &ActionId) -> Reaction {
        let a = &action.0;
        if let Some(key) = a.strip_prefix(ACTION_ACTIVATE) {
            return Reaction::tray(TrayCommand::Activate { key: key.to_string() });
        }
        if let Some(key) = a.strip_prefix(ACTION_SECONDARY) {
            return Reaction::tray(TrayCommand::SecondaryActivate { key: key.to_string() });
        }
        // Scroll: SNI convention is up = -1, down = +1 (vertical); one step per accumulated tick.
        if let Some(key) = a.strip_prefix(ACTION_SCROLL_UP) {
            return Reaction::tray(TrayCommand::Scroll { key: key.to_string(), delta: -1, horizontal: false });
        }
        if let Some(key) = a.strip_prefix(ACTION_SCROLL_DOWN) {
            return Reaction::tray(TrayCommand::Scroll { key: key.to_string(), delta: 1, horizontal: false });
        }
        if let Some(rest) = a.strip_prefix(ACTION_MENU) {
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

    fn item(key: &str, icon: Option<&str>, status: TrayStatus) -> TrayItem {
        TrayItem {
            key: key.into(),
            id: key.into(),
            title: key.into(),
            icon_name: icon.map(str::to_string),
            icon_pixmap: None,
            icon_theme_path: None,
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
    fn passive_items_render_like_active_in_v1() {
        // v1 reversal: Passive is shown (no show-hidden toggle yet — TODO(F)).
        let mut t = StatusTray::new();
        t.on_event(&Event::Tray(vec![
            item("a", Some("x"), TrayStatus::Active),
            item("p", Some("y"), TrayStatus::Passive),
        ]));
        assert_eq!(tray_keys(&t.view()), vec!["tray:a", "tray:p"]);
    }

    #[test]
    fn empty_tray_renders_nothing() {
        let mut t = StatusTray::new();
        assert_eq!(t.view(), View::Empty);
        // Only-Passive is no longer "nothing" in v1 — it renders.
        t.on_event(&Event::Tray(vec![item("p", Some("x"), TrayStatus::Passive)]));
        assert_eq!(tray_keys(&t.view()), vec!["tray:p"]);
    }

    #[test]
    fn identical_republish_does_not_dirty() {
        let mut t = StatusTray::new();
        let items = vec![item("a", Some("x"), TrayStatus::Active)];
        assert!(t.on_event(&Event::Tray(items.clone())).dirty);
        assert!(!t.on_event(&Event::Tray(items)).dirty, "same list → no re-render");
    }

    #[test]
    fn needs_attention_adds_a_class() {
        let mut t = StatusTray::new();
        t.on_event(&Event::Tray(vec![item("a", Some("x"), TrayStatus::NeedsAttention)]));
        let View::Row { children, .. } = t.view() else { panic!("row") };
        let View::Button { classes, .. } = &children[0] else { panic!("button") };
        assert!(classes.iter().any(|c| c == "needs-attention"));
    }

    #[test]
    fn middle_and_scroll_actions_route_to_commands() {
        let mut t = StatusTray::new();
        assert_eq!(
            t.on_action(&ActionId::new("secondary:k")).tray,
            vec![TrayCommand::SecondaryActivate { key: "k".into() }]
        );
        assert_eq!(
            t.on_action(&ActionId::new("scroll-up:k")).tray,
            vec![TrayCommand::Scroll { key: "k".into(), delta: -1, horizontal: false }]
        );
        assert_eq!(
            t.on_action(&ActionId::new("scroll-down:k")).tray,
            vec![TrayCommand::Scroll { key: "k".into(), delta: 1, horizontal: false }]
        );
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
