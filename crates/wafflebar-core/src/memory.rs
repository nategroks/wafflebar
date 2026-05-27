//! Memory state crossing the plugin boundary. GTK-free and serializable like the rest.
//!
//! Carries **raw kB** (as `/proc/meminfo` reports), not a pre-rounded percent: the reducer derives
//! the display percent and value-compares *that*, so two ticks rounding to the same integer percent
//! produce no re-render (the derive-to-display rule). The host owns the polling backend (see the
//! binary's `plugins/memory/backend.rs`); read-only, so there's no command type.

use serde::{Deserialize, Serialize};

/// A memory snapshot in kB. `used_kb` is `MemTotal - MemAvailable` (the kernel's own estimate of
/// what a process can actually allocate — not `MemFree`, which reads alarmingly low because it
/// excludes reclaimable cache).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryState {
    pub total_kb: u64,
    pub used_kb: u64,
    pub swap_total_kb: u64,
    pub swap_used_kb: u64,
}
