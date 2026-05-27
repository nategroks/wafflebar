//! Memory plugin: RAM usage percent. First poll-based system module (mirrors what
//! xfce4-systemload-plugin shows for memory). v1 is text-only — no graph, no swap readout.
//!
//! Pure reducer: it sees only typed [`MemoryState`]s (raw kB) from `backend` (the sole `/proc`
//! site) and derives the **rounded** display percent. Value-compare is on that derived display, so
//! two ticks rounding to the same integer percent produce zero dirty cycles (derive-to-display).

pub mod backend;

use wafflebar_core::{ActionId, Event, MemoryState, Plugin, Reaction, Topic, View};

pub struct Memory {
    display: Option<Display>,
}

/// The rendered representation: the rounded percent + the pressure class (the things a tick can
/// change). Sub-percent drift collapses here.
#[derive(Clone, PartialEq, Eq)]
struct Display {
    pct: u8,
    pressure: &'static str,
    swapping: bool,
}

impl Memory {
    pub fn new() -> Self {
        Self { display: None }
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

fn round_pct(used_kb: u64, total_kb: u64) -> u8 {
    if total_kb == 0 {
        return 0;
    }
    // f64 only to round; the *comparison* is on the resulting integer, never on a float.
    (used_kb as f64 / total_kb as f64 * 100.0).round().clamp(0.0, 100.0) as u8
}

fn derive(state: &MemoryState) -> Display {
    let pct = round_pct(state.used_kb, state.total_kb);
    let pressure = if pct >= 90 {
        "critical"
    } else if pct >= 75 {
        "high"
    } else {
        "normal"
    };
    Display {
        pct,
        pressure,
        swapping: state.swap_used_kb > 0,
    }
}

impl Plugin for Memory {
    fn id(&self) -> &str {
        "memory"
    }

    fn subscribe(&self) -> Vec<Topic> {
        vec![Topic::Memory]
    }

    fn view(&self) -> View {
        let Some(d) = &self.display else {
            return View::Empty; // no reading yet / /proc unreadable — render nothing
        };
        let mut row = View::row(
            vec![
                View::icon("utilities-system-monitor", 16).with_class("mem-icon"),
                View::label(format!("{}%", d.pct)).with_class("mem-label"),
            ],
            4,
        )
        .with_class("module")
        .with_class("memory")
        .with_class(d.pressure);
        if d.swapping {
            row = row.with_class("swapping");
        }
        row
    }

    fn on_event(&mut self, ev: &Event) -> Reaction {
        let Event::Memory(state) = ev else {
            return Reaction::none();
        };
        // Compare derived display: 34.7% and 34.9% both render 34%/35%, so jitter is no-op.
        let next = derive(state);
        if self.display.as_ref() == Some(&next) {
            return Reaction::none();
        }
        self.display = Some(next);
        Reaction::dirty()
    }

    fn on_action(&mut self, _action: &ActionId) -> Reaction {
        // v1 has no interaction (no click target). TODO(memory): click → a system monitor.
        Reaction::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_pct(pct_x10: u64) -> Event {
        // total = 100_000 kB; used set so used/total*100 = pct_x10/10 (e.g. 347 → 34.7%).
        Event::Memory(MemoryState {
            total_kb: 100_000,
            used_kb: pct_x10 * 100, // (pct_x10/10)% of 100_000 = pct_x10*100
            swap_total_kb: 0,
            swap_used_kb: 0,
        })
    }

    #[test]
    fn sub_percent_jitter_does_not_dirty() {
        let mut m = Memory::new();
        assert!(m.on_event(&at_pct(347)).dirty); // 34.7% → 35%
        assert!(!m.on_event(&at_pct(349)).dirty, "34.9% also rounds to 35% → no re-render");
        assert!(!m.on_event(&at_pct(350)).dirty, "35.0% → 35% → no re-render");
    }

    #[test]
    fn crossing_an_integer_percent_dirties() {
        let mut m = Memory::new();
        m.on_event(&at_pct(344)); // 34.4% → 34%
        assert!(m.on_event(&at_pct(349)).dirty, "34% → 35% repaints");
    }

    #[test]
    fn pressure_class_tracks_usage() {
        let mut m = Memory::new();
        m.on_event(&at_pct(500)); // 50%
        assert!(matches!(&m.display, Some(d) if d.pressure == "normal"));
        m.on_event(&at_pct(800)); // 80%
        assert!(matches!(&m.display, Some(d) if d.pressure == "high"));
        m.on_event(&at_pct(950)); // 95%
        assert!(matches!(&m.display, Some(d) if d.pressure == "critical"));
    }

    #[test]
    fn no_reading_renders_nothing() {
        assert_eq!(Memory::new().view(), View::Empty);
    }

    #[test]
    fn first_reading_paints() {
        let mut m = Memory::new();
        assert!(m.on_event(&at_pct(500)).dirty);
        match m.view() {
            View::Row { children, .. } => {
                assert!(matches!(&children[1], View::Label { text, .. } if text == "50%"));
            }
            _ => panic!("expected row"),
        }
    }
}
