//! Clock module — a `Plugin` that re-renders on each timer tick.
//!
//! Canary for the `View` boundary (see `docs/ARCHITECTURE.md`): a read-only module should be
//! trivial on the trait. It is — `view()` is one `View::label(formatted_time)`.

use chrono::Local;
use wafflebar_core::{ActionId, ConfigField, Event, Plugin, ModuleConfig, Reaction, Topic, View};

const DEFAULT_FORMAT: &str = "%a %d %b   %H:%M";

pub struct Clock {
    format: String,
}

impl Clock {
    pub fn new(cfg: &ModuleConfig) -> Self {
        Self {
            format: cfg.opt_str("format").unwrap_or(DEFAULT_FORMAT).to_string(),
        }
    }
}

impl Plugin for Clock {
    fn id(&self) -> &str {
        "clock"
    }

    fn subscribe(&self) -> Vec<Topic> {
        vec![Topic::Timer { secs: 1 }]
    }

    fn view(&self) -> View {
        View::label(Local::now().format(&self.format).to_string()).with_class("clock")
    }

    fn on_event(&mut self, ev: &Event) -> Reaction {
        // Re-render on any tick (the only topic we subscribe to).
        if matches!(ev, Event::Tick { .. }) {
            Reaction::dirty()
        } else {
            Reaction::none()
        }
    }

    fn on_action(&mut self, _action: &ActionId) -> Reaction {
        Reaction::none()
    }

    fn configure(&mut self, cfg: &ModuleConfig) -> Reaction {
        // Config-only plugin: reconstruct from the new config (dedupes new()/configure()).
        *self = Self::new(cfg);
        Reaction::dirty()
    }

    fn config_schema(&self) -> Vec<ConfigField> {
        vec![ConfigField::text("format", "Time format (strftime)", DEFAULT_FORMAT)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::test_module_config as cfg;

    #[test]
    fn configure_re_reads_format() {
        let mut c = Clock::new(&cfg(&[("format", toml::Value::from("%H:%M"))]));
        assert_eq!(c.format, "%H:%M");
        let r = c.configure(&cfg(&[("format", toml::Value::from("%H:%M:%S"))]));
        assert!(r.dirty);
        assert_eq!(c.format, "%H:%M:%S");
    }
}
