//! someblocks-style status-block intake (NATEWM_MODE step 1 — scaffold only).
//!
//! Compatible producer shape:
//!
//! ```text
//! one line per refresh, blocks separated by " | ":
//! ^fg(#d8dee9)^bg(#3b4252) 14:23  | mem 7421/64000Mi\n
//! ```
//!
//! `^fg(#…)`/`^bg(#…)` inline color escapes are recognized when the feed is started with the
//! color-commands flag (mirroring dwlb's `-status-commands`); bare text otherwise. The parser
//! lives next to the intake — not in core — so [`FeedBlock`] stays transport-agnostic.
//!
//! **Transport:** a UNIX `SOCK_STREAM` at the path configured via [`FeedSocketConfig::path`]
//! (default `$XDG_RUNTIME_DIR/wafflebar/feed.sock`). Producer connects, writes one line per
//! refresh, may close or stay connected. wafflebar accepts repeated connections so a feeder can
//! be restarted without bouncing the bar — the same model dwlb uses for its status-stdin.
//!
//! **Scaffold scope (this commit):** type signatures and the connect/listen shape. The actual
//! `listen()` loop, line buffering, and parser body land in step 2 once the structure is signed
//! off.

use std::os::fd::RawFd;
use std::path::PathBuf;

use anyhow::{bail, Result};
use wafflebar_core::{FeedEvent, FeedSocketConfig};

/// The listening socket. Owns its fd and a per-connection line buffer.
///
/// Lifecycle: created once at startup (or `None` if [`FeedSocketConfig::path`] is `None`);
/// torn down with the binary. The host integrates [`Self::fd`] into the GLib main loop just
/// like [`crate::wm::WmConnection::fd`] — same pattern, separate event source.
#[allow(dead_code)] // wire-up lands in NATEWM_MODE step 2
pub struct SomeblocksIntake {
    /// Listening socket fd.
    #[allow(dead_code)] // step-2 wire-up populates this
    fd: RawFd,
    /// Resolved socket path (kept for log messages and stale-socket cleanup on shutdown).
    #[allow(dead_code)]
    path: PathBuf,
    /// Whether to interpret `^fg(#…)`/`^bg(#…)` inline escapes. Mirror of dwlb's
    /// `-status-commands` flag.
    #[allow(dead_code)]
    parse_color_escapes: bool,
}

#[allow(dead_code)] // wire-up lands in NATEWM_MODE step 2
impl SomeblocksIntake {
    /// Bind the socket, creating its parent directory and removing any stale file. Returns `None`
    /// when [`FeedSocketConfig::path`] is `None` (the feed is disabled — the right-side strip
    /// stays empty unless another plugin populates it).
    pub fn bind(_cfg: &FeedSocketConfig, _parse_color_escapes: bool) -> Result<Option<Self>> {
        bail!("someblocks: scaffold only — listener + parser land in NATEWM_MODE step 2")
    }

    /// The readable fd the host watches.
    pub fn fd(&self) -> RawFd {
        self.fd
    }

    /// Drain ready bytes on a wake-up. Returns `FeedEvent::Frame` for each completed line
    /// received. Partial trailing input stays buffered between calls.
    pub fn dispatch(&mut self) -> Vec<FeedEvent> {
        // Step 2: accept(), read available bytes, line-buffer, parse each completed line into a
        // `FeedEvent::Frame { blocks }`. Empty in scaffold.
        Vec::new()
    }
}
