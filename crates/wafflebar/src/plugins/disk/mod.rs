//! Disk plugin: shows used-percent for one mount point (default `/`). Mirrors the memory plugin's
//! shape — rounded display percent + pressure class — but the source is `statvfs()` instead of
//! `/proc/meminfo`, and the period is 30 s (disk usage doesn't change at 1 Hz).
//!
//! Pure reducer: it sees only typed [`DiskState`]s for its configured `path`; off-path events are
//! ignored so two instances (root + /home, say) don't cross-contaminate.

pub mod backend;

use wafflebar_core::{
    ActionId, ConfigField, DiskState, Event, ModuleConfig, Plugin, Reaction, Topic, View,
};

pub(crate) fn read_path(cfg: &ModuleConfig) -> String {
    let p = cfg.opt_str("path").unwrap_or("/");
    if p.is_empty() { "/".to_string() } else { p.to_string() }
}

pub struct Disk {
    path: String,
    display: Option<Display>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Display {
    pct: u8,
    pressure: &'static str,
}

impl Disk {
    pub fn new(path: String) -> Self {
        Self { path, display: None }
    }
}

fn round_pct(used: u64, total: u64) -> u8 {
    if total == 0 { return 0; }
    ((used as u128 * 100 + (total as u128) / 2) / total as u128).min(100) as u8
}

fn pressure_class(pct: u8) -> &'static str {
    if pct >= 95 { "critical" }
    else if pct >= 85 { "high" }
    else { "normal" }
}

fn derive(state: &DiskState) -> Display {
    let pct = round_pct(state.used_bytes(), state.total_bytes);
    Display { pct, pressure: pressure_class(pct) }
}

impl Plugin for Disk {
    fn id(&self) -> &str { "disk" }

    fn subscribe(&self) -> Vec<Topic> { vec![Topic::Disk] }

    fn view(&self) -> View {
        let Some(d) = self.display else { return View::Empty; };
        View::row(
            vec![
                View::icon("drive-harddisk-symbolic", 16).with_class("disk-icon"),
                View::label(format!("{}%", d.pct)).with_class("disk-label"),
            ],
            4,
        )
        .with_class("module")
        .with_class("disk")
        .with_class(d.pressure)
    }

    fn on_event(&mut self, ev: &Event) -> Reaction {
        let Event::Disk { path, state } = ev else { return Reaction::none(); };
        if *path != self.path { return Reaction::none(); }
        let next = derive(state);
        if self.display == Some(next) { return Reaction::none(); }
        self.display = Some(next);
        Reaction::dirty()
    }

    fn on_action(&mut self, _action: &ActionId) -> Reaction { Reaction::none() }

    fn configure(&mut self, cfg: &ModuleConfig) -> Reaction {
        let new_path = read_path(cfg);
        if new_path == self.path { return Reaction::none(); }
        self.path = new_path;
        self.display = None;
        Reaction::dirty()
    }

    fn config_schema(&self) -> Vec<ConfigField> {
        vec![ConfigField::text("path", "Mount point to monitor (e.g. /, /home)", "/")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(path: &str, total: u64, avail: u64) -> Event {
        Event::Disk { path: path.into(), state: DiskState { total_bytes: total, avail_bytes: avail } }
    }

    #[test]
    fn renders_percent_for_matching_path() {
        let mut d = Disk::new("/".into());
        assert!(d.on_event(&ev("/", 100, 25)).dirty);
        match d.view() {
            View::Row { children, .. } => {
                assert!(matches!(&children[1], View::Label { text, .. } if text == "75%"));
            }
            _ => panic!("expected row"),
        }
    }

    #[test]
    fn off_path_event_is_ignored() {
        let mut d = Disk::new("/".into());
        d.on_event(&ev("/", 100, 50));
        assert!(!d.on_event(&ev("/home", 100, 0)).dirty);
        match d.view() {
            View::Row { children, .. } => {
                assert!(matches!(&children[1], View::Label { text, .. } if text == "50%"));
            }
            _ => panic!("expected row"),
        }
    }

    #[test]
    fn sub_percent_drift_does_not_dirty() {
        let mut d = Disk::new("/".into());
        d.on_event(&ev("/", 1_000_000, 500_000));     // 50%
        assert!(!d.on_event(&ev("/", 1_000_000, 499_500)).dirty); // still rounds to 50%
    }

    #[test]
    fn pressure_class_buckets() {
        let mut d = Disk::new("/".into());
        d.on_event(&ev("/", 100, 14)); // 86% → high
        assert!(matches!(d.view(), View::Row { classes, .. } if classes.contains(&"high".to_string())));
        d.on_event(&ev("/", 100, 4));  // 96% → critical
        assert!(matches!(d.view(), View::Row { classes, .. } if classes.contains(&"critical".to_string())));
    }
}
