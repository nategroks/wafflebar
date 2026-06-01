//! Disk usage state. GTK-free, serializable like the rest of the boundary.

use serde::{Deserialize, Serialize};

/// A single mount point's usage, captured by the backend per tick. The reducer derives a rounded
/// display percent from `used_bytes / total_bytes` — sub-percent drift collapses there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskState {
    /// Total capacity of the mount point in bytes.
    pub total_bytes: u64,
    /// Free space available to the unprivileged user (i.e. `f_bavail`, not `f_bfree`).
    pub avail_bytes: u64,
}

impl DiskState {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.avail_bytes)
    }
}
