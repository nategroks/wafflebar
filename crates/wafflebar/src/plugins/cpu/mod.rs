//! CPU plugin: aggregate load percent. Second poll-based system module (mirrors what
//! xfce4-systemload-plugin shows for CPU). v1 is aggregate text only.
//!
//! Pure reducer: sees only typed [`CpuState`] deltas from `backend` (the sole `/proc` site) and
//! derives the **rounded** aggregate percent + an icon bucket. Value-compare is on that derived
//! display, so sub-percent drift and within-bucket movement produce zero dirty cycles.
//!
//! v1 shows one aggregate percent. Per-core display is a v2 candidate blocked on the
//! graph/sparkline design call (a 16+-core text row is its own UX problem); the keyed-diff
//! per-core canary test arrives with it, since v1 has no per-core View to reconcile.

pub mod backend;

use wafflebar_core::{ActionId, CpuState, Event, Plugin, Reaction, Topic, View};

pub struct Cpu {
    display: Option<Display>,
}

/// Rendered representation: rounded percent + load bucket (a CSS class). Sub-percent / within-bucket
/// drift collapses here.
#[derive(Clone, PartialEq, Eq)]
struct Display {
    pct: u8,
    bucket: &'static str,
}

impl Cpu {
    pub fn new() -> Self {
        Self { display: None }
    }
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

fn round_pct(busy: u64, total: u64) -> u8 {
    if total == 0 {
        return 0; // genuinely-idle tick (no jiffies elapsed) — not a divide-by-zero
    }
    // f64 only to round; the comparison downstream is on the resulting integer.
    (busy as f64 / total as f64 * 100.0).round().clamp(0.0, 100.0) as u8
}

/// Load bucket → CSS class (same bucket-as-value-compare technique as wifi strength). A CSS class,
/// not a per-bucket icon name: unlike `network-wireless-signal-*`, there's no standard freedesktop
/// `cpu-load-*` icon, so the icon stays constant and the bucket tints it via CSS.
fn bucket_for(pct: u8) -> &'static str {
    match pct {
        0..=25 => "low",
        26..=60 => "medium",
        61..=85 => "high",
        _ => "critical",
    }
}

fn derive(state: &CpuState) -> Display {
    let pct = round_pct(state.busy_delta, state.total_delta);
    Display { pct, bucket: bucket_for(pct) }
}

impl Plugin for Cpu {
    fn id(&self) -> &str {
        "cpu"
    }

    fn subscribe(&self) -> Vec<Topic> {
        vec![Topic::Cpu]
    }

    fn view(&self) -> View {
        let Some(d) = &self.display else {
            return View::Empty; // no delta yet (first tick is silent) / /proc unreadable
        };
        View::row(
            vec![
                View::icon("utilities-system-monitor", 16).with_class("cpu-icon"),
                View::label(format!("{}%", d.pct)).with_class("cpu-label"),
            ],
            4,
        )
        .with_class("module")
        .with_class("cpu")
        .with_class(d.bucket)
    }

    fn on_event(&mut self, ev: &Event) -> Reaction {
        let Event::Cpu(state) = ev else {
            return Reaction::none();
        };
        let next = derive(state);
        if self.display.as_ref() == Some(&next) {
            return Reaction::none();
        }
        self.display = Some(next);
        Reaction::dirty()
    }

    fn on_action(&mut self, _action: &ActionId) -> Reaction {
        // v1 has no interaction. TODO(cpu): click → a system monitor; scroll → nothing.
        Reaction::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A delta yielding `busy/total` ≈ the given per-mille (e.g. 114 → 11.4%).
    fn at_permille(permille: u64) -> Event {
        Event::Cpu(CpuState { busy_delta: permille, total_delta: 1000 })
    }

    #[test]
    fn sub_percent_drift_does_not_dirty() {
        let mut c = Cpu::new();
        assert!(c.on_event(&at_permille(110)).dirty); // 11.0% → 11%
        assert!(!c.on_event(&at_permille(114)).dirty, "11.4% also rounds to 11%");
        assert!(!c.on_event(&at_permille(109)).dirty, "10.9% also rounds to 11%");
    }

    #[test]
    fn crossing_an_integer_percent_dirties() {
        let mut c = Cpu::new();
        c.on_event(&at_permille(114)); // 11%
        assert!(c.on_event(&at_permille(124)).dirty, "11% → 12% repaints");
    }

    #[test]
    fn crossing_an_icon_bucket_dirties() {
        let mut c = Cpu::new();
        c.on_event(&at_permille(240)); // 24% → low
        assert!(matches!(&c.display, Some(d) if d.bucket == "low"));
        assert!(c.on_event(&at_permille(260)).dirty, "24% → 26% crosses low/medium");
        assert!(matches!(&c.display, Some(d) if d.bucket == "medium"));
    }

    #[test]
    fn zero_delta_is_zero_percent_not_a_panic() {
        let mut c = Cpu::new();
        let r = c.on_event(&Event::Cpu(CpuState { busy_delta: 0, total_delta: 0 }));
        assert!(r.dirty);
        assert!(matches!(&c.display, Some(d) if d.pct == 0));
    }

    #[test]
    fn no_delta_yet_renders_nothing() {
        assert_eq!(Cpu::new().view(), View::Empty);
    }

    #[test]
    fn first_reading_paints_aggregate_percent() {
        let mut c = Cpu::new();
        c.on_event(&Event::Cpu(CpuState { busy_delta: 170, total_delta: 1000 })); // 17%
        match c.view() {
            View::Row { children, .. } => {
                assert!(matches!(&children[1], View::Label { text, .. } if text == "17%"));
            }
            _ => panic!("expected row"),
        }
    }
}
