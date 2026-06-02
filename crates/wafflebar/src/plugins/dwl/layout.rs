//! Layout symbol — the dwl tiling-layout indicator (e.g. `[]=`, `(@)`).
//!
//! Renders `WmEvent::Layout { output, symbol }` for *this bar's* output as a single label.
//! Matches the dwlb right-strip's layout indicator. Read-only; no click handler today (the dwl
//! `-s` channel is one-way; cycling the layout via click would require the IPC patch).

use wafflebar_core::{ActionId, Event, Plugin, Reaction, Topic, View, WmEvent};

pub struct Layout {
    output: String,
    symbol: String,
}

impl Layout {
    pub fn new(output: &str) -> Self {
        Self {
            output: output.to_string(),
            symbol: String::new(),
        }
    }
}

impl Plugin for Layout {
    fn id(&self) -> &str {
        "layout"
    }

    fn subscribe(&self) -> Vec<Topic> {
        vec![Topic::Wm]
    }

    fn view(&self) -> View {
        if self.symbol.is_empty() {
            return View::Empty;
        }
        View::label(self.symbol.clone()).with_class("layout")
    }

    fn on_event(&mut self, ev: &Event) -> Reaction {
        if let Event::Wm(WmEvent::Layout { output, symbol }) = ev {
            if output == &self.output && symbol != &self.symbol {
                self.symbol = symbol.clone();
                return Reaction::dirty();
            }
        }
        Reaction::none()
    }

    fn on_action(&mut self, _action: &ActionId) -> Reaction {
        Reaction::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updates_on_matching_output_only() {
        let mut l = Layout::new("WL-1");
        // Mismatched output is ignored.
        let r = l.on_event(&Event::Wm(WmEvent::Layout {
            output: "WL-2".into(),
            symbol: "[]=".into(),
        }));
        assert!(!r.dirty);
        assert!(matches!(l.view(), View::Empty));

        // Matching output applies.
        let r = l.on_event(&Event::Wm(WmEvent::Layout {
            output: "WL-1".into(),
            symbol: "(@)".into(),
        }));
        assert!(r.dirty);
        if let View::Label { text, .. } = l.view() {
            assert_eq!(text, "(@)");
        } else {
            panic!("expected Label");
        }
    }

    #[test]
    fn coalesces_identical_symbol_repeats() {
        let mut l = Layout::new("WL-1");
        l.on_event(&Event::Wm(WmEvent::Layout {
            output: "WL-1".into(),
            symbol: "(@)".into(),
        }));
        // Same symbol again → no re-render.
        let r = l.on_event(&Event::Wm(WmEvent::Layout {
            output: "WL-1".into(),
            symbol: "(@)".into(),
        }));
        assert!(!r.dirty);
    }
}
