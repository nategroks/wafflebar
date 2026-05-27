//! Network plugin: primary-connection indicator (icon + name). Read-only in v1 — a click opens
//! the connection editor. Mirrors third-party xfce4 network plugins' panel button shape.
//!
//! Pure reducer: it sees only the typed [`NetworkState`] the host's backend derives from
//! NetworkManager (all zbus lives in `backend`). The value-compare guard is on the *derived
//! display* (icon bucket + label), so NM's burst `PropertiesChanged` — wifi signal especially —
//! only re-renders when the displayed state actually moves (the C2 reducer discipline).

pub mod backend;

use wafflebar_core::{ActionId, Event, NetworkState, Plugin, Reaction, Topic, View};

const ACTION_OPEN: &str = "open-editor";
// TODO(network): make the editor command configurable; fall back to nmtui-in-a-terminal.
const EDITOR_CMD: &str = "nm-connection-editor";

pub struct Network {
    /// Derived display, `None` until the first event (or if NM is absent → backend never emits).
    display: Option<Display>,
}

/// What actually drives the rendered widgets. Coarser than [`NetworkState`]: wifi strength is
/// bucketed into the icon, so sub-bucket fluctuation compares equal and doesn't dirty.
#[derive(Clone, PartialEq, Eq)]
struct Display {
    icon: &'static str,
    label: String,
}

impl Network {
    pub fn new() -> Self {
        Self { display: None }
    }
}

impl Default for Network {
    fn default() -> Self {
        Self::new()
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
                View::label(&d.label).with_class("net-label"),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wireless(name: &str, strength: u8) -> Event {
        Event::Network(NetworkState::Wireless { name: name.into(), strength })
    }

    #[test]
    fn first_event_paints_then_identical_is_noop() {
        let mut n = Network::new();
        assert!(n.on_event(&Event::Network(NetworkState::Wired { name: "eth0".into() })).dirty);
        assert!(!n
            .on_event(&Event::Network(NetworkState::Wired { name: "eth0".into() }))
            .dirty);
    }

    #[test]
    fn wifi_strength_jitter_within_bucket_does_not_dirty() {
        // NM bursts strength updates; within a signal bucket the displayed state is unchanged.
        let mut n = Network::new();
        assert!(n.on_event(&wireless("home", 70)).dirty);
        assert!(!n.on_event(&wireless("home", 72)).dirty, "70 and 72 are both 'good'");
        assert!(!n.on_event(&wireless("home", 85)).dirty, "85 still 'good'");
    }

    #[test]
    fn crossing_a_bucket_dirties() {
        let mut n = Network::new();
        n.on_event(&wireless("home", 70)); // good
        assert!(n.on_event(&wireless("home", 90)).dirty, "good -> excellent repaints");
    }

    #[test]
    fn state_transitions_dirty_and_change_the_view() {
        let mut n = Network::new();
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
        assert_eq!(Network::new().view(), View::Empty);
    }

    #[test]
    fn click_opens_the_editor() {
        let mut n = Network::new();
        assert_eq!(
            n.on_action(&ActionId::new(ACTION_OPEN)).spawn,
            vec![vec![EDITOR_CMD.to_string()]]
        );
    }
}
