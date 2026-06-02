//! Vanilla-dwl `-s` stdin backend (NATEWM_MODE step 1 — scaffold only).
//!
//! Speaks the **stdout status protocol** dwl already emits when invoked as `dwl -s <cmd>` — no
//! IPC patch required. dwl forks `<cmd>` (us) with our stdin attached to dwl's status pipe, and
//! writes one record per line:
//!
//! ```text
//! <MON> title  <s>          /* focused-window title on monitor <MON> */
//! <MON> appid  <s>          /* focused-window app_id */
//! <MON> fullscreen  <0|1>
//! <MON> floating    <0|1>
//! <MON> selmon      <0|1>   /* is <MON> the currently-selected monitor */
//! <MON> tags <occ> <sel> <urgsel> <urgocc>      /* four 32-bit tag bitmasks */
//! <MON> layout <sym>        /* current layout symbol, e.g. (@) or []= */
//! ```
//!
//! This is the seam that makes [`crate::wm::dwl::DwlBackend`] (which requires
//! `dwl-ipc-unstable-v2` + `wlr-foreign-toplevel`) **optional**. The trade-off is no window list
//! and no [`WmCommand`] execution back into dwl — stdin is a one-way wire from compositor to bar.
//! That matches what dwlb does today and is the contract floor for this backend.
//!
//! **Scaffold scope (this commit):** types, signatures, lifecycle plumbing, no parser body and
//! no event emission. The parser, the bitmask → [`Tag`] translation, and the
//! [`WindowManager::execute`] no-op behavior land in step 2 once the structure is signed off.

use std::os::fd::RawFd;

use anyhow::{bail, Result};
use wafflebar_core::{Tag, WindowManager, WmCommand, WmEvent};

use super::WmConnection;

/// Per-monitor state, accumulated from successive status lines. dwl emits one record per field
/// per change; a full repaint requires all fields. We reduce them into `tags`/`symbol`/`title`/
/// `appid` and emit a [`WmEvent`] only when a *committed* field actually changes.
#[allow(dead_code)] // step-2 wire-up populates these
struct MonitorState {
    /// Connector name as dwl reports it (`WL-1`, `eDP-1`, …).
    name: String,
    /// Cached focused-window title.
    title: String,
    /// Cached focused-window app_id.
    appid: String,
    /// Cached layout symbol (e.g. `(@)`).
    symbol: String,
    /// Cached tag list (translated from the four bitmasks).
    tags: Vec<Tag>,
    /// Cached selmon flag (which monitor is selected). Drives "active window" emission targeting.
    selmon: bool,
}

/// The backend itself. Owns the stdin handle and the per-monitor cache.
///
/// Lifetime: lives for the whole process. `connect()` consumes stdin so no other code can race
/// us; we hold the `RawFd` for the event-loop integration in [`WmConnection::fd`].
pub struct DwlStdinBackend {
    /// The fd we read status records from (typically `0`, dwl's pipe to our stdin).
    fd: RawFd,
    /// Per-monitor reducer state, keyed by connector name.
    #[allow(dead_code)] // step-2 wire-up populates this
    monitors: Vec<MonitorState>,
    /// Initial snapshot for [`WindowManager::snapshot`]. Empty until first `frame` line.
    #[allow(dead_code)] // step-2 wire-up populates this
    initial_snapshot: Vec<WmEvent>,
}

impl DwlStdinBackend {
    /// Try to bind this backend.
    ///
    /// Refuses if our stdin is a TTY — that's a clear sign we were *not* spawned by `dwl -s`
    /// (interactive shell, `cargo run` from a terminal, etc.). When the explicit
    /// `--dwl-status-stdin` CLI flag is set, the caller can force this through; the unforced
    /// path errors so the binary can fall back to the IPC backend cleanly.
    pub fn connect(forced: bool) -> Result<Self> {
        let _ = forced;
        bail!("dwl_stdin: scaffold only — parser + lifecycle land in NATEWM_MODE step 2")
    }
}

impl WindowManager for DwlStdinBackend {
    fn snapshot(&self) -> Vec<WmEvent> {
        // Step 2: return cached events derived from the per-monitor reducer.
        Vec::new()
    }

    fn execute(&mut self, _cmd: &WmCommand) {
        // The `-s` channel is one-way (compositor → bar). FocusTag/SetLayout/ActivateWindow have
        // no path back into vanilla dwl without the IPC patch. Plugins can still emit commands;
        // we just drop them here. Contract-correct: parity with dwlb's read-only status feed.
    }
}

impl WmConnection for DwlStdinBackend {
    fn fd(&self) -> RawFd {
        self.fd
    }

    fn dispatch(&mut self) -> Vec<WmEvent> {
        // Step 2: read available bytes from `self.fd`, line-buffer them, parse each completed
        // line via the protocol grammar above, fold the deltas into `self.monitors`, and return
        // the resulting `WmEvent`s. Partial trailing lines stay buffered between dispatch calls.
        Vec::new()
    }
}
