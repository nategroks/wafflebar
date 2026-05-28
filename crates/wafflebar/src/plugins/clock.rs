//! Clock module — a `Plugin` that re-renders on each timer tick.
//!
//! Canary for the `View` boundary (see `docs/ARCHITECTURE.md`): a read-only module should be
//! trivial on the trait. It is — `view()` is one `View::label(formatted_time)`.

use chrono::{Local, Utc};
use chrono_tz::Tz;
use tracing::warn;
use wafflebar_core::{ActionId, ConfigField, Event, Plugin, ModuleConfig, Reaction, Topic, View};

const DEFAULT_FORMAT: &str = "%a %d %b   %H:%M";

pub struct Clock {
    format: String,
    /// Optional IANA timezone (e.g. `America/Chicago`); `None` = system local time. Handles
    /// DST automatically (so "CST" is correct year-round via `America/Chicago`).
    timezone: Option<Tz>,
}

impl Clock {
    pub fn new(cfg: &ModuleConfig) -> Self {
        let timezone = cfg.opt_str("timezone").filter(|s| !s.is_empty()).and_then(|s| {
            s.parse::<Tz>()
                .map_err(|_| warn!(timezone = s, "clock: unknown timezone; using system local"))
                .ok()
        });
        Self {
            format: cfg.opt_str("format").unwrap_or(DEFAULT_FORMAT).to_string(),
            timezone,
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
        let text = match self.timezone {
            Some(tz) => Utc::now().with_timezone(&tz).format(&self.format).to_string(),
            None => Local::now().format(&self.format).to_string(),
        };
        View::label(text).with_class("clock")
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
        vec![
            ConfigField::text("format", "Time format (strftime)", DEFAULT_FORMAT),
            ConfigField::text("timezone", "Timezone (IANA, e.g. America/Chicago; blank = system)", ""),
            // `show_calendar` / `show_week_numbers` are consumed host-side (the host attaches a
            // GtkCalendar popover to the clock's slot container — see app.rs::attach_clock_calendar
            // — so the per-minute tick can't close an open calendar). Declared here for the prefs UI.
            ConfigField::bool("show_calendar", "Calendar popover on click", true),
            ConfigField::bool("show_week_numbers", "Show week numbers in the calendar", false),
        ]
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

    #[test]
    fn parses_timezone_and_falls_back_on_unknown() {
        let c = Clock::new(&cfg(&[("timezone", toml::Value::from("America/Chicago"))]));
        assert_eq!(c.timezone, Some(chrono_tz::America::Chicago));
        let bad = Clock::new(&cfg(&[("timezone", toml::Value::from("Nowhere/Bogus"))]));
        assert_eq!(bad.timezone, None, "unknown tz → system local");
        let none = Clock::new(&cfg(&[]));
        assert_eq!(none.timezone, None);
    }
}
