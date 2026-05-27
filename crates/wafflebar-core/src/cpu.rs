//! CPU state crossing the plugin boundary. GTK-free and serializable like the rest.
//!
//! Carries the **jiffy deltas** between two `/proc/stat` reads (summed across cores), not a
//! pre-rounded percent: the reducer derives the display percent and value-compares that, so
//! sub-percent drift produces no re-render (the derive-to-display rule). The host owns the polling
//! backend (see the binary's `plugins/cpu/backend.rs`); read-only, so there's no command type.
//!
//! v1 is aggregate-only. Per-core data is computed in the backend (the aggregate is the per-core
//! sum) but not carried here yet — per-core *display* is a v2 candidate blocked on the
//! graph/sparkline design call, and a per-core field would be dead weight until then.

use serde::{Deserialize, Serialize};

/// Busy/total jiffy deltas between the previous and current `/proc/stat` read, summed over all
/// cores. `busy = total − idle − iowait` (iowait counts as *idle*: the core was free, a task was
/// merely waiting on I/O). Percent-busy = `busy_delta / total_delta`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuState {
    pub busy_delta: u64,
    pub total_delta: u64,
}
