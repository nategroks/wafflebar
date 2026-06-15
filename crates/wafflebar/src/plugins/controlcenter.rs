//! Control-center plugin (the quick-settings button). A thin reducer: it contributes only the bar
//! button (an icon), while the panel itself — sliders, Bluetooth/Wi-Fi cards, timers, session
//! actions — is host-rendered and attached to this button's container by
//! [`Host::attach_controlcenters`](crate::render::Host), exactly like the volume mixer.
//!
//! It subscribes to Audio/Bluetooth/Network purely so the host starts those backends and mirrors
//! their state; the button's own `View` never changes, so it reports no dirty on those events. The
//! click is handled host-side (the container gesture pops the panel), so `on_action` is inert.

use wafflebar_core::{ActionId, ConfigField, Event, ModuleConfig, Plugin, Reaction, Topic, View};

/// Default button glyph — a freedesktop "system settings" symbolic; override via `icon`.
const DEFAULT_ICON: &str = "preferences-system-symbolic";

pub struct ControlCenter {
    icon: String,
    label: String,
}

impl ControlCenter {
    pub fn new(cfg: &ModuleConfig) -> Self {
        Self {
            icon: cfg.opt_str("icon").unwrap_or(DEFAULT_ICON).to_string(),
            label: cfg.opt_str("label").unwrap_or_default().to_string(),
        }
    }
}

impl Plugin for ControlCenter {
    fn id(&self) -> &str {
        "controlcenter"
    }

    fn subscribe(&self) -> Vec<Topic> {
        // Start the audio/Bluetooth/network backends so the panel has live state to render; the
        // button itself is static, so `on_event` never dirties.
        vec![Topic::Audio, Topic::Bluetooth, Topic::Network]
    }

    fn view(&self) -> View {
        // A plain icon (not a Button): the host attaches the panel popover + click gesture to this
        // module's container, so the click is handled host-side (like the volume mixer).
        if self.label.is_empty() {
            View::icon(&self.icon, 18).with_class("module").with_class("controlcenter")
        } else {
            View::row(vec![View::icon(&self.icon, 18), View::label(&self.label)], 6)
                .with_class("module")
                .with_class("controlcenter")
        }
    }

    fn on_event(&mut self, _ev: &Event) -> Reaction {
        Reaction::none() // static button; live state is the host panel's concern
    }

    fn on_action(&mut self, _action: &ActionId) -> Reaction {
        Reaction::none() // click handled host-side (container gesture pops the panel)
    }

    fn configure(&mut self, cfg: &ModuleConfig) -> Reaction {
        *self = Self::new(cfg);
        Reaction::dirty()
    }

    fn config_schema(&self) -> Vec<ConfigField> {
        vec![
            ConfigField::file("icon", "Button icon (theme name or PNG/SVG path)", DEFAULT_ICON),
            ConfigField::text("label", "Button text (blank = icon only)", ""),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::test_module_config;

    #[test]
    fn static_button_never_dirties_on_events() {
        let mut cc = ControlCenter::new(&test_module_config(&[]));
        assert!(!cc.on_event(&Event::Bluetooth(Default::default())).dirty);
        assert!(cc.on_action(&ActionId::new("anything")).bluetooth.is_empty());
    }

    #[test]
    fn subscribes_to_the_backends_it_needs_started() {
        let cc = ControlCenter::new(&test_module_config(&[]));
        let topics = cc.subscribe();
        assert!(topics.contains(&Topic::Audio));
        assert!(topics.contains(&Topic::Bluetooth));
        assert!(topics.contains(&Topic::Network));
    }

    #[test]
    fn label_switches_view_shape() {
        // Icon-only by default; a configured label switches to an icon+label row.
        let icon_only = ControlCenter::new(&test_module_config(&[]));
        assert!(matches!(icon_only.view(), View::Icon { .. }));
        let with_label =
            ControlCenter::new(&test_module_config(&[("label", toml::Value::from("Settings"))]));
        assert!(matches!(with_label.view(), View::Row { .. }));
    }
}
