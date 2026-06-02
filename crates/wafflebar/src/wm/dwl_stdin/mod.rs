//! Vanilla-dwl `-s` stdin backend.
//!
//! Speaks the **stdout status protocol** dwl already emits when invoked as `dwl -s <cmd>` — no
//! IPC patch required. dwl forks `<cmd>` (us) with our stdin attached to dwl's status pipe, and
//! writes one record per line. **Grammar pinned to dwl v0.8 `9b11a49`** (the natewm-asm vendored
//! pin), as emitted by `printstatus()` in `dwl.c`:
//!
//! ```text
//! <MON> title <s>           /* focused-window title; empty if no focused client */
//! <MON> appid <s>           /* focused-window app_id; empty if no focused client */
//! <MON> fullscreen <d>      /* 0|1; empty if no focused client */
//! <MON> floating <d>        /* 0|1; empty if no focused client */
//! <MON> selmon <u>          /* 1 iff this is the selected monitor, else 0 */
//! <MON> tags <occ> <sel-tagset> <focused-client-tags> <urg>   /* four uint32 bitmasks */
//! <MON> layout <sym>        /* current layout symbol, e.g. "(@)" or "[]=" */
//! ```
//!
//! The four bitmasks emitted by the `tags` line, **in this exact order** (rederive from
//! `printstatus()` after any dwl-tag bump — the field order has shifted across dwl versions and a
//! wrong-version parser compiles fine and then mis-parses live):
//!
//! 1. `occ` — occupied: union of `c->tags` for every client on this monitor.
//! 2. `m->tagset[m->seltags]` — the selected (viewed) tagset for this monitor.
//! 3. `c->tags` for the focused client (or `0` if no focused client).
//! 4. `urg` — urgent: union of `c->tags` for every client with `isurgent` on this monitor.
//!
//! dwl emits a complete 7-line block on every state change and `fflush()`es immediately. The
//! parser is line-oriented with no framing protocol — accumulate until the next `printstatus()`
//! block lands.
//!
//! **Backpressure / coalesce-latest by construction.** `dispatch()` reads until `EAGAIN`,
//! feeding everything into [`parser::StatusReducer`]. The reducer **overwrites** per-monitor
//! state on each line — intermediate states never queue, only the latest is retained. The fast
//! producer (dwl during a tag-switch burst) cannot back-pressure us *and* cannot accumulate
//! work for the renderer: rendering is at most one snapshot per dirty monitor per dispatch
//! cycle. "Never blocks dwl" is true by construction, not by timing luck.
//!
//! **Read-only channel.** `execute()` drops `WmCommand`s (FocusTag/SetLayout/ActivateWindow have
//! no path back into vanilla dwl without the IPC patch). To make the read-only constraint
//! diagnosable without spam, the first dropped command per session logs once at `info` with the
//! cause. See [`DwlStdinBackend::execute`] for the exact message.
//!
//! **EOF semantics.** dwl exiting closes the pipe; our next read returns 0. We flip
//! [`DwlStdinBackend::closed`] to `true` and stop reading. The host observes via the trait method
//! (default `false` on other backends) and triggers a clean exit — under `dwl -s`, dwl itself is
//! gone, so the session is unrecoverable; exit non-zero, run the same socket cleanup the SIGTERM
//! handler runs (NATEWM_MODE flag 5), and return.

mod parser;

#[cfg(test)]
mod tests;

use std::io;
use std::os::fd::RawFd;
use std::os::unix::io::AsRawFd;

use anyhow::{bail, Context, Result};
use tracing::{info, warn};
use wafflebar_core::{WindowManager, WmCommand, WmEvent};

use super::WmConnection;
use parser::StatusReducer;

/// Vanilla-dwl `-s` stdin backend.
pub struct DwlStdinBackend {
    /// The fd we read status records from (typically `0`, dwl's pipe to our stdin).
    fd: RawFd,
    /// Line-buffering reducer; coalesces fast bursts into per-monitor snapshots.
    reducer: StatusReducer,
    /// Whether we've already logged the one-time "WM command dropped" diagnostic.
    drop_logged: bool,
    /// True after `read()` returned `0` (dwl exited). Observed via [`WmConnection::closed`].
    closed: bool,
}

impl DwlStdinBackend {
    /// Try to bind this backend.
    ///
    /// Refuses if our stdin is a TTY — that's a clear sign we were *not* spawned by `dwl -s`
    /// (interactive shell, `cargo run` from a terminal, etc.). When the explicit
    /// `--dwl-status-stdin` CLI flag is set, the caller can force this through; the unforced
    /// path errors so the binary can fall back to the IPC backend cleanly.
    pub fn connect(forced: bool) -> Result<Self> {
        let fd = io::stdin().as_raw_fd();
        // SAFETY: `isatty` is a pure read of fd metadata; either return value is well-defined.
        let is_tty = unsafe { libc::isatty(fd) } == 1;
        if is_tty && !forced {
            bail!(
                "dwl_stdin: stdin is a TTY (not spawned by `dwl -s`); \
                 pass --dwl-status-stdin to force"
            );
        }
        set_nonblocking(fd).context("dwl_stdin: making stdin non-blocking")?;
        Ok(Self {
            fd,
            reducer: StatusReducer::new(),
            drop_logged: false,
            closed: false,
        })
    }
}

impl WindowManager for DwlStdinBackend {
    fn snapshot(&self) -> Vec<WmEvent> {
        self.reducer.full_snapshot()
    }

    fn execute(&mut self, _cmd: &WmCommand) {
        // Read-only channel: the `-s` pipe goes compositor → bar, not the other way. Plugins can
        // still emit `WmCommand`s (clicks, scrolls); we drop them. Log the cause once per session
        // so future-you reading the journal sees the *constraint*, not just "dropped something."
        if !self.drop_logged {
            info!(
                "WM command dropped: dwl `-s` is a one-way status channel, \
                 clicks/scrolls can't route back without the IPC patch"
            );
            self.drop_logged = true;
        }
    }
}

impl WmConnection for DwlStdinBackend {
    fn fd(&self) -> RawFd {
        self.fd
    }

    fn dispatch(&mut self) -> Vec<WmEvent> {
        if self.closed {
            return Vec::new();
        }
        let mut buf = [0u8; 4096];
        loop {
            // SAFETY: `buf` is valid for `buf.len()` writable bytes; `self.fd` is a raw fd we own
            // for the duration of the process and never close.
            let n = unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n > 0 {
                self.reducer.feed(&buf[..n as usize]);
                // Loop: more bytes may be ready. Stop on EAGAIN.
                continue;
            }
            if n == 0 {
                // EOF — dwl's write end closed. Compositor is gone; session is unrecoverable.
                // Host will observe via `closed()` and exit (after running SIGTERM cleanup).
                self.closed = true;
                warn!("dwl_stdin: EOF on stdin; dwl exited");
                break;
            }
            // n < 0: check errno.
            let err = io::Error::last_os_error();
            // On Linux `EAGAIN == EWOULDBLOCK`; checking either covers both.
            match err.raw_os_error() {
                Some(libc::EAGAIN) => break, // drained
                Some(libc::EINTR) => continue, // signal; retry
                _ => {
                    warn!(error = %err, "dwl_stdin: read error; closing");
                    self.closed = true;
                    break;
                }
            }
        }
        self.reducer.drain_events()
    }

    fn closed(&self) -> bool {
        self.closed
    }
}

fn set_nonblocking(fd: RawFd) -> Result<()> {
    // SAFETY: `fcntl` with `F_GETFL`/`F_SETFL` on a valid fd is sound; the call is single-arg.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        bail!("fcntl F_GETFL: {}", io::Error::last_os_error());
    }
    let r = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if r < 0 {
        bail!("fcntl F_SETFL: {}", io::Error::last_os_error());
    }
    Ok(())
}
