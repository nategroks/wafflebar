//! Module widgets and the dispatcher that maps a config `type` to a `gtk::Widget`.
//!
//! M1 ships the `clock` module. Module kinds that aren't implemented yet render a dim
//! placeholder labelled with their type, so a forward-looking config still produces a
//! usable bar and the grid layout is visible. Real implementations land in M2/M3.

pub mod clock;

use gtk4::prelude::*;
use gtk4::Widget;
use tracing::warn;
use wafflebar_core::ModuleConfig;

/// Build the widget for a module of the given `kind`.
pub fn build(kind: &str, cfg: &ModuleConfig) -> Widget {
    match kind {
        "clock" => clock::build(cfg),
        other => placeholder(other),
    }
}

/// A dim placeholder for not-yet-implemented module kinds.
fn placeholder(kind: &str) -> Widget {
    warn!(kind, "module not implemented yet; rendering placeholder");
    let label = gtk4::Label::new(Some(kind));
    label.add_css_class("module");
    label.add_css_class("placeholder");
    label.upcast()
}
