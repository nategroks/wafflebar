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
    label: String,
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
        0..=15 => "network-wireless-signal-none",
        16..=40 => "network-wireless-signal-weak",
        41..=65 => "network-wireless-signal-ok",
        66..=85 => "network-wireless-signal-good",
        _ => "network-wireless-signal-excellent",
    }
}

fn derive(state: &NetworkState) -> Display {
    match state {
        NetworkState::Disconnected => Display {
            icon: "network-offline",
            label: "offline".to_string(),
        },
        NetworkState::Wired { name } => Display {
            icon: "network-wired",
            label: name.clone(),
        },
        NetworkState::Wireless { name, strength } => Display {
            icon: wifi_icon(*strength),
            label: name.clone(),
        },
    }
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
        View::row(
            vec![
                View::icon(d.icon, 16).with_class("net-icon"),
                View::label(self.truncate(&d.label)).with_class("net-label"),
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

    fn wireless(name: &str, strength: u8) -> Event {
        Event::Network(NetworkState::Wireless { name: name.into(), strength })
    }

    #[test]
    fn first_event_paints_then_identical_is_noop() {
        let mut n = Network::new(0);
        assert!(n.on_event(&Event::Network(NetworkState::Wired { name: "eth0".into() })).dirty);
        assert!(!n
            .on_event(&Event::Network(NetworkState::Wired { name: "eth0".into() }))
            .dirty);
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
        n.on_event(&Event::Network(NetworkState::Wired { name: "eth0".into() }));
        assert!(n.on_event(&Event::Network(NetworkState::Disconnected)).dirty);
        match n.view() {
            View::Button { child, .. } => {
                let View::Row { children, .. } = *child else { panic!("row") };
                assert!(matches!(&children[0], View::Icon { name, .. } if name == "network-offline"));
            }
            _ => panic!("expected button"),
        }
    }

    #[test]
    fn no_state_renders_nothing() {
        assert_eq!(Network::new(0).view(), View::Empty);
    }

    #[test]
    fn long_name_truncates_at_max_chars() {
        let mut n = Network::new(5);
        n.on_event(&Event::Network(NetworkState::Wired {
            name: "Wired connection 1".into(),
        }));
        match n.view() {
            View::Button { child, .. } => {
                let View::Row { children, .. } = *child else { panic!("row") };
                assert!(matches!(&children[1], View::Label { text, .. } if text == "Wire…"));
            }
            _ => panic!("expected button"),
        }
        // Reconfigure to unlimited re-renders the full name from the stored display.
        n.configure(&crate::plugins::test_module_config(&[("max_chars", toml::Value::from(0))]));
        match n.view() {
            View::Button { child, .. } => {
                let View::Row { children, .. } = *child else { panic!("row") };
                assert!(matches!(&children[1], View::Label { text, .. } if text == "Wired connection 1"));
            }
            _ => panic!("expected button"),
        }
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
