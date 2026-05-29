//! Network types crossing the plugin boundary. GTK-free and serializable like the rest.
//!
//! The host owns the actual backend (NetworkManager over zbus, on the GLib loop — see the binary's
//! `plugins/network/backend.rs`); the plugin only ever sees this derived state. There is no command
//! type: v1 network is read-only display (a click opens an editor via `Reaction::spawn`), so unlike
//! audio there's no outbound channel.

use serde::{Deserialize, Serialize};

/// The derived network state the plugin renders. The backend walks NetworkManager's object graph
/// and collapses it to this; the reducer value-compares it so burst property changes that don't
/// move the displayed state produce no re-render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkState {
    /// No primary connection (NM up but nothing connected).
    Disconnected,
    /// A wired (or otherwise non-wireless) primary connection. `interface` is the kernel device name
    /// (`eth0`/`enp…`, as in `ifconfig`/`ip`); `rx_bps`/`tx_bps` are the current down/up throughput in
    /// **bits per second** (sampled from `/sys/class/net/<iface>/statistics`).
    Wired { interface: String, rx_bps: u64, tx_bps: u64 },
    /// A wireless primary connection. `interface` is the kernel device name (`wlan0`/`wlp…`),
    /// `strength` is 0..=100 (NM `AccessPoint.Strength`), `rx_bps`/`tx_bps` as above.
    Wireless { interface: String, strength: u8, rx_bps: u64, tx_bps: u64 },
}
