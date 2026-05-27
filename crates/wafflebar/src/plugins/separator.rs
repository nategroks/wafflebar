//! Separator plugin: blank space, a rule, a handle grip, or dots — with an optional `expand` that
//! makes it eat all available space. Mirrors xfce4-panel's `plugins/separator/separator.c` (which
//! cairo-draws the four styles); here it's a [`View::Separator`] + CSS, no drawing in the plugin.
//!
//! Deliberately tiny: a separator is pure presentation, so this reducer is stateless and emits the
//! same `View` forever. The `expand` flag is what lets `[left] | spacer | [right]` layouts work.

use wafflebar_core::{ActionId, Event, ModuleConfig, Plugin, Reaction, SeparatorStyle, Topic, View};

pub struct Separator {
    style: SeparatorStyle,
    expand: bool,
}

impl Separator {
    /// Config: `style = "transparent"|"line"|"handle"|"dots"` (default `line`), `expand = bool`.
    pub fn new(cfg: &ModuleConfig) -> Self {
        Self {
            style: cfg.opt_str("style").map(SeparatorStyle::parse).unwrap_or_default(),
            expand: cfg.opt_bool("expand").unwrap_or(false),
        }
    }
}

impl Plugin for Separator {
    fn id(&self) -> &str {
        "separator"
    }
    fn subscribe(&self) -> Vec<Topic> {
        Vec::new()
    }
    fn view(&self) -> View {
        View::Separator { style: self.style, expand: self.expand }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::test_module_config as cfg;

    #[test]
    fn configure_re_reads_style_and_expand() {
        let mut s = Separator::new(&cfg(&[]));
        assert!(matches!(s.style, SeparatorStyle::Line) && !s.expand);
        let r = s.configure(&cfg(&[
            ("style", toml::Value::from("dots")),
            ("expand", toml::Value::from(true)),
        ]));
        assert!(r.dirty);
        assert!(matches!(s.style, SeparatorStyle::Dots) && s.expand);
    }
}
