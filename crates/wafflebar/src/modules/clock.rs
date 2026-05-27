//! Clock module — a `Module` that re-renders on each timer tick.
//!
//! Canary for the `View` boundary (see `docs/ARCHITECTURE.md`): a read-only module should be
//! trivial on the trait. It is — `view()` is one `View::label(formatted_time)`.

use chrono::Local;
use wafflebar_core::{ActionId, Event, Module, ModuleConfig, Reaction, Topic, View};

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

impl Module for Clock {
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
}
