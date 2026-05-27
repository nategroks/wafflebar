//! Window module — shows the focused window's title for this output.
//!
//! Pure reducer over `WmEvent::ActiveWindow`. Display-only in v1 (no actions). Optional
//! `max_chars` config truncates long titles with an ellipsis (kept in the description rather
//! than relying on host CSS so the truncation is testable and toolkit-independent).

use wafflebar_core::{
    ActionId, ConfigField, Event, ModuleConfig, Plugin, Reaction, Topic, View, WmEvent,
};

/// Read the `max_chars` option (0 = no truncation). Shared by the registry (construction) and
/// `configure` (reload) so the extraction lives in one place.
pub(crate) fn read_max_chars(cfg: &ModuleConfig) -> usize {
    cfg.opt_i64("max_chars").unwrap_or(0).max(0) as usize
}

pub struct Window {
    output: String,
    title: String,
    app_id: String,
    max_chars: usize,
}

impl Window {
    pub fn new(output: &str, max_chars: usize) -> Self {
        Self {
            output: output.to_string(),
            title: String::new(),
            app_id: String::new(),
            max_chars,
        }
    }

    fn display(&self) -> String {
        if self.max_chars == 0 || self.title.chars().count() <= self.max_chars {
            return self.title.clone();
        }
        let truncated: String = self.title.chars().take(self.max_chars.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

impl Plugin for Window {
    fn id(&self) -> &str {
        "window"
    }

    fn subscribe(&self) -> Vec<Topic> {
        vec![Topic::Wm]
    }

    fn view(&self) -> View {
        if self.title.is_empty() {
            return View::Empty;
        }
        View::label(self.display()).with_class("window")
    }

    fn on_event(&mut self, ev: &Event) -> Reaction {
        if let Event::Wm(WmEvent::ActiveWindow {
            output,
            title,
            app_id,
        }) = ev
        {
            if *output == self.output {
                self.title = title.clone();
                self.app_id = app_id.clone();
                return Reaction::dirty();
            }
        }
        Reaction::none()
    }

    fn on_action(&mut self, _action: &ActionId) -> Reaction {
        Reaction::none()
    }

    fn configure(&mut self, cfg: &ModuleConfig) -> Reaction {
        // Re-read the option; `output` (placement) and runtime title/app_id are preserved.
        self.max_chars = read_max_chars(cfg);
        Reaction::dirty()
    }

    fn config_schema(&self) -> Vec<ConfigField> {
        vec![ConfigField::int("max_chars", "Max characters (0 = unlimited)", 0, 200, 0)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::test_module_config as cfg;
    use wafflebar_core::fake::FakeWm;

    #[test]
    fn configure_re_reads_max_chars_preserving_output() {
        let mut w = Window::new("eDP-1", 0);
        let r = w.configure(&cfg(&[("max_chars", toml::Value::from(12))]));
        assert!(r.dirty);
        assert_eq!(w.max_chars, 12);
        assert_eq!(w.output, "eDP-1", "placement output is preserved across reconfigure");
    }

    #[test]
    fn shows_active_title() {
        let mut wm = FakeWm::new("DP-1", 9);
        let mut w = Window::new("DP-1", 0);
        let ev = wm.set_active_window("Firefox — Mozilla", "firefox");
        assert!(w.on_event(&Event::Wm(ev)).dirty);
        assert_eq!(w.view(), View::label("Firefox — Mozilla").with_class("window"));
    }

    #[test]
    fn empty_title_renders_nothing() {
        let w = Window::new("DP-1", 0);
        assert_eq!(w.view(), View::Empty);
    }

    #[test]
    fn truncates_long_titles() {
        let mut w = Window::new("DP-1", 5);
        w.on_event(&Event::Wm(WmEvent::ActiveWindow {
            output: "DP-1".to_string(),
            title: "abcdefgh".to_string(),
            app_id: "x".to_string(),
        }));
        assert_eq!(w.view(), View::label("abcd…").with_class("window"));
    }

    #[test]
    fn ignores_other_output() {
        let mut w = Window::new("DP-1", 0);
        let r = w.on_event(&Event::Wm(WmEvent::ActiveWindow {
            output: "DP-2".to_string(),
            title: "nope".to_string(),
            app_id: "x".to_string(),
        }));
        assert!(!r.dirty);
        assert_eq!(w.view(), View::Empty);
    }
}
