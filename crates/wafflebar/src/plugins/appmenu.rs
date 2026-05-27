//! Applications menu plugin (E): a bar button that opens the host-rendered applications menu.
//!
//! A thin reducer — it owns only its config (button icon/text, and the menu's favorites/recents
//! settings) and emits a `View::Popover` whose content is the `View::AppMenu` marker. The menu UI
//! itself (two-pane category/app lists, search, launching) is host-rendered, because its state is
//! immediate-mode interactive, not reducer-derived. See `crate::menu` and `View::AppMenu`.

use wafflebar_core::{ActionId, ConfigField, Event, ModuleConfig, Plugin, Reaction, Topic, View};

const DEFAULT_ICON: &str = "view-app-grid-symbolic";
const DEFAULT_MAX_RECENTS: i64 = 10;

pub struct AppMenu {
    icon: String,
    label: String,
    favorites: Vec<String>,
    show_recents: bool,
    max_recents: u32,
}

impl AppMenu {
    pub fn new(cfg: &ModuleConfig) -> Self {
        Self {
            icon: cfg.opt_str("icon").unwrap_or(DEFAULT_ICON).to_string(),
            label: cfg.opt_str("label").unwrap_or_default().to_string(),
            favorites: cfg.opt_str_list("favorites"),
            show_recents: cfg.opt_bool("show_recents").unwrap_or(true),
            max_recents: cfg.opt_i64("max_recents").unwrap_or(DEFAULT_MAX_RECENTS).max(0) as u32,
        }
    }
}

impl Plugin for AppMenu {
    fn id(&self) -> &str {
        "appmenu"
    }

    fn subscribe(&self) -> Vec<Topic> {
        Vec::new() // static button; the menu is host-driven
    }

    fn view(&self) -> View {
        let trigger = if self.label.is_empty() {
            View::icon(&self.icon, 18)
        } else {
            View::row(vec![View::icon(&self.icon, 18), View::label(&self.label)], 6)
        };
        View::Popover {
            trigger: Box::new(trigger),
            content: Box::new(View::AppMenu {
                favorites: self.favorites.clone(),
                show_recents: self.show_recents,
                max_recents: self.max_recents,
            }),
            classes: Vec::new(),
        }
        .with_class("appmenu")
    }

    fn on_event(&mut self, _ev: &Event) -> Reaction {
        Reaction::none()
    }

    fn on_action(&mut self, _action: &ActionId) -> Reaction {
        Reaction::none()
    }

    fn configure(&mut self, cfg: &ModuleConfig) -> Reaction {
        *self = Self::new(cfg); // config-only: reconstruct
        Reaction::dirty()
    }

    fn config_schema(&self) -> Vec<ConfigField> {
        vec![
            ConfigField::text("icon", "Button icon", DEFAULT_ICON),
            ConfigField::text("label", "Button text (blank = icon only)", ""),
            ConfigField::bool("show_recents", "Show recent applications", true),
            ConfigField::int("max_recents", "Recent applications shown", 0, 50, DEFAULT_MAX_RECENTS),
            // Reuses F4's DesktopList editor (the third consumer of the field type).
            ConfigField::desktop_list("favorites", "Favorites"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::test_module_config;

    #[test]
    fn view_wraps_appmenu_marker_carrying_config() {
        let cfg = test_module_config(&[
            ("show_recents", toml::Value::Boolean(false)),
            ("max_recents", toml::Value::Integer(5)),
            ("favorites", toml::Value::Array(vec!["firefox".into()])),
        ]);
        match AppMenu::new(&cfg).view() {
            View::Popover { content, .. } => match *content {
                // The reducer carries only its config into the marker; the host renders the UI.
                View::AppMenu { favorites, show_recents, max_recents } => {
                    assert_eq!(favorites, vec!["firefox".to_string()]);
                    assert!(!show_recents);
                    assert_eq!(max_recents, 5);
                }
                other => panic!("popover content should be AppMenu, got {}", other.variant_tag()),
            },
            other => panic!("view should be a Popover, got {}", other.variant_tag()),
        }
    }

    #[test]
    fn defaults_when_unconfigured() {
        match AppMenu::new(&test_module_config(&[])).view() {
            View::Popover { content, .. } => match *content {
                View::AppMenu { show_recents, max_recents, favorites } => {
                    assert!(show_recents && max_recents == 10 && favorites.is_empty());
                }
                _ => panic!("expected AppMenu"),
            },
            _ => panic!("expected Popover"),
        }
    }
}
