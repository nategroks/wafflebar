//! External-feed events — the someblocks-style channel.
//!
//! This is the **second** input wafflebar consumes (the first being the [`WindowManager`] event
//! stream from a compositor backend). It's deliberately separate: the natewm contract calls for
//! two distinct intakes — dwl's `-s` status protocol on stdin, and an external block feed (clock,
//! mem, anything someblocks-shaped) on its own socket. Collapsing them into one pipe was the
//! dwlb near-miss and would lose both sides; see `docs/NATEWM_MODE.md` for the long-form
//! rationale.
//!
//! GTK-free, serializable, same discipline as [`crate::wm`]: a host transports these to plugins;
//! plugins are pure reducers over them.
//!
//! **Scaffold status:** type signatures + structure only. The actual parser, transport, and
//! plugin wiring land in NATEWM_MODE step 2.
//!
//! [`WindowManager`]: crate::wm::WindowManager

use serde::{Deserialize, Serialize};

/// One block on the bar (the unit a someblocks-style status program emits).
///
/// A producer (e.g. `natewm-status`) writes a sequence of these — one per refresh cycle for the
/// whole right-side strip — to wafflebar's status socket. The producer owns block ordering and
/// timing; wafflebar just renders what arrives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedBlock {
    /// Stable identifier for the block (e.g. `"clock"`, `"mem"`). Stable across refreshes so the
    /// renderer can diff and update widgets in place rather than rebuilding the whole strip.
    pub name: String,
    /// Display text. Inline `^fg(#…)`/`^bg(#…)` escapes are honored when the producer was started
    /// with the equivalent of dwlb's `-status-commands` flag; bare text otherwise. The parser
    /// lives next to the intake — not here — so this type stays transport-agnostic.
    pub text: String,
}

/// A frame from the external feed — a full snapshot of the right-side strip.
///
/// Snapshot-shaped (not diff-shaped) for the same reason [`crate::wm::WmEvent`] is: plugins stay
/// pure reducers, no in-band state to reconcile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeedEvent {
    /// Replace the whole right-side strip with these blocks, in order.
    Frame { blocks: Vec<FeedBlock> },
}

/// Where wafflebar listens for [`FeedEvent`]s.
///
/// The default mirrors dwlb's pattern (`$XDG_RUNTIME_DIR/wafflebar/feed.sock`) so a feeder script
/// can be written the same way `natewm-status` is today. A CLI override lets the user point at a
/// different path for tests or multi-instance setups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedSocketConfig {
    /// Absolute path to the UNIX socket. `None` disables the feed entirely (the right strip is
    /// empty unless another plugin populates it).
    pub path: Option<std::path::PathBuf>,
}

impl Default for FeedSocketConfig {
    fn default() -> Self {
        Self { path: None }
    }
}
