//! Clock module: a label that re-renders the local time on a 1-second timer.
//!
//! Config options:
//! - `format` — a `chrono::format::strftime` string (default `"%a %d %b   %H:%M"`).

use chrono::Local;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Label, Widget};
use wafflebar_core::ModuleConfig;

const DEFAULT_FORMAT: &str = "%a %d %b   %H:%M";

/// Build the clock widget.
pub fn build(cfg: &ModuleConfig) -> Widget {
    let format = cfg.opt_str("format").unwrap_or(DEFAULT_FORMAT).to_string();

    let label = Label::new(None);
    label.add_css_class("module");
    label.add_css_class("clock");

    let render = {
        let format = format.clone();
        move || Local::now().format(&format).to_string()
    };
    label.set_text(&render());

    // Tick once a second. The closure holds a weak ref so the timer stops if the label dies.
    glib::timeout_add_seconds_local(1, {
        let label = label.downgrade();
        move || {
            let Some(label) = label.upgrade() else {
                return glib::ControlFlow::Break;
            };
            label.set_text(&render());
            glib::ControlFlow::Continue
        }
    });

    label.upcast()
}
