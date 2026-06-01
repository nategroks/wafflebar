//! Per-interface link state from `/sys/class/net/<name>/`. Sibling to [`net`](crate::net): where
//! that one follows NetworkManager's *primary* connection, this is name-keyed — each plugin
//! instance tracks a specific kernel interface (`eth0`, `wlan0`, `wlp3s0`…) regardless of which is
//! the default route.
//!
//! GTK-free, serializable like the rest of the boundary.

use serde::{Deserialize, Serialize};

/// What the host's interface backend reports per tick for ONE kernel interface.
///
/// Throughput is bits-per-second, derived by the backend from successive `statistics/{rx,tx}_bytes`
/// reads — same shape as [`crate::net`], but here keyed by name, not by primary-ness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceLink {
    /// Kernel interface name, e.g. `wlan0`. The plugin uses this to match against its config.
    pub name: String,
    /// `true` when the link is administratively up AND has an IP / operational state.
    pub up: bool,
    /// `/sys/class/net/<name>/wireless/` exists → wireless. Drives icon choice.
    pub wireless: bool,
    /// Down/up throughput in bits per second; 0 on the first sample or while the link is down.
    pub rx_bps: u64,
    pub tx_bps: u64,
}

/// One tick's snapshot of every kernel interface the backend can see. Plugins filter by name.
///
/// The backend emits the **full set** each poll so plugins for different interfaces share one
/// reader; the per-plugin filter is cheap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct InterfaceState {
    pub links: Vec<InterfaceLink>,
}

impl InterfaceState {
    /// Find the link for an interface by name, if present.
    pub fn find(&self, name: &str) -> Option<&InterfaceLink> {
        self.links.iter().find(|l| l.name == name)
    }
}
