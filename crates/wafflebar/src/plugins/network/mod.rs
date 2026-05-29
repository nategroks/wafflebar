//! Network plugin: primary-connection indicator (icon + name). Read-only in v1 — a click opens
//! the connection editor. Mirrors third-party xfce4 network plugins' panel button shape.
//!
//! Pure reducer: it sees only the typed [`NetworkState`] the host's backend derives from
//! NetworkManager (all zbus lives in `backend`). The value-compare guard is on the *derived
//! display* (icon bucket + label), so NM's burst `PropertiesChanged` — wifi signal especially —
//! only re-renders when the displayed state actually moves (the C2 reducer discipline).

pub mod backend;

use wafflebar_core::{
    ActionId, ConfigField, Event, ModuleConfig, NetworkState, Plugin, Reaction, Topic, View,
};

/// Read the `max_chars` option (0 = no truncation). Shared by the registry and `configure`.
pub(crate) fn read_max_chars(cfg: &ModuleConfig) -> usize {
    cfg.opt_i64("max_chars").unwrap_or(0).max(0) as usize
}

const ACTION_OPEN: &str = "open-editor";
// TODO(network): make the editor command configurable; fall back to nmtui-in-a-terminal.
const EDITOR_CMD: &str = "nm-connection-editor";

pub struct Network {
    /// Derived display, `None` until the first event (or if NM is absent → backend never emits).
    display: Option<Display>,
    /// Truncate the connection-name label to this many characters (0 = unlimited). Applied at
    /// render so a reconfigure re-truncates the same stored name without needing the raw state.
    max_chars: usize,
}

/// What actually drives the rendered widgets. Coarser than [`NetworkState`]: wifi strength is
/// bucketed into the icon, so sub-bucket fluctuation compares equal and doesn't dirty.
#[derive(Clone, PartialEq, Eq)]
struct Display {
    icon: &'static str,
    /// Kernel interface name (`eth0`/`wlan0`); empty when disconnected. Truncated at render.
    interface: String,
    /// The throughput readout (`↓12.3 ↑1.2`) or `offline`. Changes each active tick → re-renders.
    rate: String,
}

impl Network {
    pub fn new(max_chars: usize) -> Self {
        Self { display: None, max_chars }
    }

    /// Truncate `s` to `self.max_chars` characters, ellipsizing. Mirrors the `window` plugin.
    fn truncate(&self, s: &str) -> String {
        if self.max_chars == 0 || s.chars().count() <= self.max_chars {
            return s.to_string();
        }
        let t: String = s.chars().take(self.max_chars.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

impl Default for Network {
    fn default() -> Self {
        Self::new(0)
    }
}

/// Wireless signal icon by strength bucket (freedesktop `network-wireless-signal-*`).
fn wifi_icon(strength: u8) -> &'static str {
    match strength {
        0..=15 => "wb-net-wifi-none-symbolic",
        16..=40 => "wb-net-wifi-weak-symbolic",
        41..=65 => "wb-net-wifi-ok-symbolic",
        66..=85 => "wb-net-wifi-good-symbolic",
        _ => "wb-net-wifi-excellent-symbolic",
    }
}

fn derive(state: &NetworkState) -> Display {
    match state {
        NetworkState::Disconnected => Display {
            icon: "wb-net-offline-symbolic",
            interface: String::new(),
            rate: "offline".to_string(),
        },
        NetworkState::Wired { interface, rx_bps, tx_bps } => Display {
            icon: "wb-net-wired-symbolic",
            interface: interface.clone(),
            rate: rate_label(*rx_bps, *tx_bps),
        },
        NetworkState::Wireless { interface, strength, rx_bps, tx_bps } => Display {
            icon: wifi_icon(*strength),
            interface: interface.clone(),
            rate: rate_label(*rx_bps, *tx_bps),
        },
    }
}

/// Down/up throughput as `↓<rx> ↑<tx>` in Mbps (one decimal). Bits/s → Mbps = /1e6.
fn rate_label(rx_bps: u64, tx_bps: u64) -> String {
    format!("↓{:.1} ↑{:.1}", rx_bps as f64 / 1e6, tx_bps as f64 / 1e6)
}

impl Plugin for Network {
    fn id(&self) -> &str {
        "network"
    }

    fn subscribe(&self) -> Vec<Topic> {
        vec![Topic::Network]
    }

    fn view(&self) -> View {
        let Some(d) = &self.display else {
            return View::Empty; // NM absent / no state yet — render nothing, not a dead button
        };
        // `eth0 ↓12.3 ↑1.2` when connected; just the rate ("offline") otherwise. Truncation applies
        // to the interface name only (kept short), never the throughput.
        let label = if d.interface.is_empty() {
            d.rate.clone()
        } else {
            format!("{} {}", self.truncate(&d.interface), d.rate)
        };
        View::row(
            vec![
                View::icon(d.icon, 16).with_class("net-icon"),
                View::label(label).with_class("net-label"),
            ],
            4,
        )
        .button(ActionId::new(ACTION_OPEN))
        .with_class("module")
        .with_class("network")
    }

    fn on_event(&mut self, ev: &Event) -> Reaction {
        let Event::Network(state) = ev else {
            return Reaction::none();
        };
        // Compare derived display: bucketed wifi strength means burst PropertiesChanged that don't
        // move the bucket produce zero dirty cycles.
        let next = derive(state);
        if self.display.as_ref() == Some(&next) {
            return Reaction::none();
        }
        self.display = Some(next);
        Reaction::dirty()
    }

    fn on_action(&mut self, action: &ActionId) -> Reaction {
        if action.0 == ACTION_OPEN {
            return Reaction::spawn(vec![EDITOR_CMD.to_string()]);
        }
        Reaction::none()
    }

    fn configure(&mut self, cfg: &ModuleConfig) -> Reaction {
        // Re-read the limit; the stored display is re-truncated at render, so no state is needed.
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

    fn wireless(iface: &str, strength: u8) -> Event {
        Event::Network(NetworkState::Wireless { interface: iface.into(), strength, rx_bps: 0, tx_bps: 0 })
    }
    fn wired(iface: &str, rx_bps: u64, tx_bps: u64) -> Event {
        Event::Network(NetworkState::Wired { interface: iface.into(), rx_bps, tx_bps })
    }

    #[test]
    fn first_event_paints_then_identical_is_noop() {
        let mut n = Network::new(0);
        assert!(n.on_event(&wired("eth0", 0, 0)).dirty);
        assert!(!n.on_event(&wired("eth0", 0, 0)).dirty);
    }

    #[test]
    fn throughput_change_dirties_and_formats_mbps() {
        let mut n = Network::new(0);
        n.on_event(&wired("eth0", 0, 0));
        assert!(n.on_event(&wired("eth0", 12_300_000, 1_200_000)).dirty, "new rate repaints");
        match n.view() {
            View::Button { child, .. } => {
                let View::Row { children, .. } = *child else { panic!("row") };
                assert!(matches!(&children[1], View::Label { text, .. } if text == "eth0 ↓12.3 ↑1.2"));
            }
            _ => panic!("expected button"),
        }
    }

    #[test]
    fn wifi_strength_jitter_within_bucket_does_not_dirty() {
        // NM bursts strength updates; within a signal bucket the displayed state is unchanged.
        let mut n = Network::new(0);
        assert!(n.on_event(&wireless("home", 70)).dirty);
        assert!(!n.on_event(&wireless("home", 72)).dirty, "70 and 72 are both 'good'");
        assert!(!n.on_event(&wireless("home", 85)).dirty, "85 still 'good'");
    }

    #[test]
    fn crossing_a_bucket_dirties() {
        let mut n = Network::new(0);
        n.on_event(&wireless("home", 70)); // good
        assert!(n.on_event(&wireless("home", 90)).dirty, "good -> excellent repaints");
    }

    #[test]
    fn state_transitions_dirty_and_change_the_view() {
        let mut n = Network::new(0);
        n.on_event(&wired("eth0", 0, 0));
        assert!(n.on_event(&Event::Network(NetworkState::Disconnected)).dirty);
        match n.view() {
            View::Button { child, .. } => {
                let View::Row { children, .. } = *child else { panic!("row") };
                assert!(matches!(&children[0], View::Icon { name, .. } if name == "wb-net-offline-symbolic"));
            }
            _ => panic!("expected button"),
        }
    }

    #[test]
    fn no_state_renders_nothing() {
        assert_eq!(Network::new(0).view(), View::Empty);
    }

    #[test]
    fn long_interface_truncates_but_throughput_is_kept() {
        // Truncation applies to the interface name only — never the throughput readout.
        let mut n = Network::new(5);
        n.on_event(&wired("enp0s31f6", 0, 0));
        let label_text = |n: &Network| match n.view() {
            View::Button { child, .. } => match *child {
                View::Row { children, .. } => match &children[1] {
                    View::Label { text, .. } => text.clone(),
                    _ => panic!("label"),
                },
                _ => panic!("row"),
            },
            _ => panic!("button"),
        };
        assert_eq!(label_text(&n), "enp0… ↓0.0 ↑0.0", "interface truncated, rate intact");
        // Reconfigure to unlimited re-renders the full interface from the stored display.
        n.configure(&crate::plugins::test_module_config(&[("max_chars", toml::Value::from(0))]));
        assert_eq!(label_text(&n), "enp0s31f6 ↓0.0 ↑0.0");
    }

    #[test]
    fn click_opens_the_editor() {
        let mut n = Network::new(0);
        assert_eq!(
            n.on_action(&ActionId::new(ACTION_OPEN)).spawn,
            vec![vec![EDITOR_CMD.to_string()]]
        );
    }
}
