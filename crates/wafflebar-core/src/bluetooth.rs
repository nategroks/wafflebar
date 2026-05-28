//! Bluetooth types crossing the plugin boundary. GTK-free and serializable like the rest.
//!
//! The host owns the actual backend (BlueZ over zbus, on the GLib loop — see the binary's
//! `plugins/bluetooth/backend.rs`); the plugin only ever sees this derived state and emits the
//! typed commands. Mirrors the audio split (`VolumeEvent`/`VolumeCommand`).

use serde::{Deserialize, Serialize};

/// The derived Bluetooth state the plugin renders. The backend walks BlueZ's object graph (the
/// adapter + its paired devices) and collapses it to this; the reducer value-compares it so no-op
/// polls don't re-render.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BluetoothState {
    /// Whether a Bluetooth adapter exists at all. `false` → the plugin renders nothing.
    pub present: bool,
    /// Whether the adapter is powered on.
    pub powered: bool,
    /// Whether the adapter is actively scanning for nearby devices.
    pub discovering: bool,
    /// Devices worth listing: all *paired* devices, plus *discovered* (unpaired) ones while scanning.
    pub devices: Vec<BtDevice>,
}

/// A Bluetooth device — either a paired one (connect/disconnect) or a discovered one (pair).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BtDevice {
    /// BlueZ object path (`/org/bluez/hci0/dev_XX_…`) — the stable id used to address commands.
    pub path: String,
    /// Display name (BlueZ `Alias`, falling back to `Name`/address).
    pub name: String,
    /// Whether it is paired (already known). Unpaired entries appear only while scanning.
    pub paired: bool,
    /// Whether it is currently connected.
    pub connected: bool,
}

/// A command the Bluetooth plugin issues; the host's BlueZ backend performs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BluetoothCommand {
    /// Power the adapter on/off (`Adapter1.Powered`).
    SetPowered(bool),
    /// Connect the device at this object path (`Device1.Connect`).
    Connect(String),
    /// Disconnect the device at this object path (`Device1.Disconnect`).
    Disconnect(String),
    /// Start/stop scanning for nearby devices (`Adapter1.StartDiscovery`/`StopDiscovery`).
    SetDiscovering(bool),
    /// Pair (then connect) the discovered device at this object path (`Device1.Pair`).
    Pair(String),
}

impl BluetoothState {
    /// Whether any paired device is currently connected (drives the "connected" icon).
    pub fn any_connected(&self) -> bool {
        self.devices.iter().any(|d| d.connected)
    }
}
