//! Bluetooth plugin: an adapter indicator with a click-out menu. The icon reflects power +
//! connection state; clicking opens a popover (a `View::Col` of buttons) to toggle power and
//! connect/disconnect paired devices.
//!
//! Pure reducer: it sees only the typed [`BluetoothState`] the host's BlueZ backend derives and
//! emits [`BluetoothCommand`]s. The menu is a plain `View::Popover` of buttons (no host-rendered
//! marker, no keyboard) so it reuses the existing popover + `ActionId` routing — a click-only
//! popover works on dwl.

pub mod backend;

use wafflebar_core::{
    ActionId, BluetoothCommand, BluetoothState, Event, Plugin, Reaction, Topic, View,
};

/// Click on the power row → toggle the adapter.
const ACTION_POWER: &str = "bt-power";
/// A device row's action is `bt-dev:<object-path>`; clicking toggles its connection.
const PFX_DEV: &str = "bt-dev:";

pub struct Bluetooth {
    state: BluetoothState,
}

impl Bluetooth {
    pub fn new() -> Self {
        Self { state: BluetoothState::default() }
    }
}

impl Default for Bluetooth {
    fn default() -> Self {
        Self::new()
    }
}

/// Icon by adapter state: off / on-idle / connected.
fn icon_for(s: &BluetoothState) -> &'static str {
    if !s.powered {
        "wb-bt-off-symbolic"
    } else if s.any_connected() {
        "wb-bt-connected-symbolic"
    } else {
        "wb-bt-symbolic"
    }
}

/// The popover menu: a power toggle row, then one row per paired device (or a dim placeholder).
fn menu(s: &BluetoothState) -> View {
    let mut children = vec![View::label(if s.powered {
        "Bluetooth: On"
    } else {
        "Bluetooth: Off"
    })
    .with_class("bt-menu-power")
    .button(ActionId::new(ACTION_POWER))];

    if !s.powered {
        // Powered off → device rows aren't actionable; just the toggle.
    } else if s.devices.is_empty() {
        children.push(View::label("No paired devices").with_class("dim-label"));
    } else {
        for d in &s.devices {
            let mark = if d.connected { "● " } else { "○ " };
            children.push(
                View::label(format!("{mark}{}", d.name))
                    .with_class("bt-menu-item")
                    .button(ActionId::new(format!("{PFX_DEV}{}", d.path))),
            );
        }
    }

    View::Col { children, gap: 2, classes: vec!["bt-menu".to_string()] }
}

impl Plugin for Bluetooth {
    fn id(&self) -> &str {
        "bluetooth"
    }

    fn subscribe(&self) -> Vec<Topic> {
        vec![Topic::Bluetooth]
    }

    fn view(&self) -> View {
        if !self.state.present {
            return View::Empty; // no adapter — render nothing, not a dead button
        }
        View::Popover {
            trigger: Box::new(View::icon(icon_for(&self.state), 16).with_class("bt-icon")),
            content: Box::new(menu(&self.state)),
            classes: Vec::new(),
        }
        .with_class("module")
        .with_class("bluetooth")
    }

    fn on_event(&mut self, ev: &Event) -> Reaction {
        let Event::Bluetooth(state) = ev else {
            return Reaction::none();
        };
        if &self.state == state {
            return Reaction::none(); // value-compare: no-op polls don't re-render
        }
        self.state = state.clone();
        Reaction::dirty()
    }

    fn on_action(&mut self, action: &ActionId) -> Reaction {
        let a = action.0.as_str();
        if a == ACTION_POWER {
            return Reaction::bluetooth(BluetoothCommand::SetPowered(!self.state.powered));
        }
        if let Some(path) = a.strip_prefix(PFX_DEV) {
            let connected = self.state.devices.iter().any(|d| d.path == path && d.connected);
            return Reaction::bluetooth(if connected {
                BluetoothCommand::Disconnect(path.to_string())
            } else {
                BluetoothCommand::Connect(path.to_string())
            });
        }
        Reaction::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wafflebar_core::BtDevice;

    fn state(present: bool, powered: bool, devices: Vec<BtDevice>) -> BluetoothState {
        BluetoothState { present, powered, devices }
    }
    fn dev(path: &str, name: &str, connected: bool) -> BtDevice {
        BtDevice { path: path.into(), name: name.into(), connected }
    }

    #[test]
    fn absent_adapter_renders_nothing() {
        assert_eq!(Bluetooth::new().view(), View::Empty);
    }

    #[test]
    fn icon_tracks_power_and_connection() {
        let mut b = Bluetooth::new();
        b.on_event(&Event::Bluetooth(state(true, false, vec![])));
        assert_eq!(icon_for(&b.state), "wb-bt-off-symbolic");
        b.on_event(&Event::Bluetooth(state(true, true, vec![])));
        assert_eq!(icon_for(&b.state), "wb-bt-symbolic");
        b.on_event(&Event::Bluetooth(state(true, true, vec![dev("/d1", "Buds", true)])));
        assert_eq!(icon_for(&b.state), "wb-bt-connected-symbolic");
    }

    #[test]
    fn unchanged_state_does_not_dirty() {
        let mut b = Bluetooth::new();
        assert!(b.on_event(&Event::Bluetooth(state(true, true, vec![]))).dirty);
        assert!(!b.on_event(&Event::Bluetooth(state(true, true, vec![]))).dirty);
    }

    #[test]
    fn power_action_toggles() {
        let mut b = Bluetooth::new();
        b.on_event(&Event::Bluetooth(state(true, true, vec![])));
        assert_eq!(
            b.on_action(&ActionId::new(ACTION_POWER)).bluetooth,
            vec![BluetoothCommand::SetPowered(false)]
        );
    }

    #[test]
    fn device_action_connects_or_disconnects_by_current_state() {
        let mut b = Bluetooth::new();
        b.on_event(&Event::Bluetooth(state(
            true,
            true,
            vec![dev("/d1", "Buds", false), dev("/d2", "Mouse", true)],
        )));
        assert_eq!(
            b.on_action(&ActionId::new("bt-dev:/d1")).bluetooth,
            vec![BluetoothCommand::Connect("/d1".into())],
            "disconnected device → Connect"
        );
        assert_eq!(
            b.on_action(&ActionId::new("bt-dev:/d2")).bluetooth,
            vec![BluetoothCommand::Disconnect("/d2".into())],
            "connected device → Disconnect"
        );
    }
}
