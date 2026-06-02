//! someblocks-style status-block intake.
//!
//! **Producer shape:** one line per refresh; blocks separated by `" | "`. Optional inline
//! `^fg(#…)`/`^bg(#…)` color escapes when started with the equivalent of dwlb's
//! `-status-commands` flag. The producer owns block ordering and timing; we render whatever
//! arrives most recently.
//!
//! Example wire bytes (verbatim what a dwlb-compatible feeder writes):
//!
//! ```text
//! ^fg(#d8dee9)^bg(#3b4252) 14:23  | mem 7421/64000Mi\n
//! ```
//!
//! **Transport.** UNIX `SOCK_STREAM` at [`FeedSocketConfig::path`]. The host (main.rs) resolves
//! the path — explicit `--status-socket PATH` or `$XDG_RUNTIME_DIR/wafflebar/feed.sock`; if
//! `XDG_RUNTIME_DIR` is unset *and* no override was given, `cfg.path` is `None` and
//! [`bind`](SomeblocksIntake::bind) returns `Ok(None)` (feed disabled — no silent fallback to
//! `/tmp`, which would be a multi-user trap).
//!
//! **Bind policy (NATEWM_MODE flag 4).**
//! - `path` doesn't exist → bind.
//! - `path` exists and is NOT a socket → error (refuse to clobber a regular file).
//! - `path` exists, is a socket, `connect()` succeeds → error (another wafflebar owns it;
//!   singleton).
//! - `path` exists, is a socket, `connect()` fails `ECONNREFUSED` → previous owner died →
//!   `unlink()` + bind.
//!
//! **Cleanup.** [`cleanup`](SomeblocksIntake::cleanup) `unlink()`s the socket; safe to call
//! multiple times. The [`Drop`] impl invokes it on graceful drop; the SIGTERM handler and the
//! EOF-exit path (step 3) call it explicitly so a session restart finds a clean slate.
//!
//! **Backpressure / coalesce-latest.** [`dispatch`](SomeblocksIntake::dispatch) drains every
//! ready byte from every connected producer and parses every complete line, but the per-conn
//! buffer **overwrites** the candidate output instead of appending — at most one
//! [`FeedEvent::Frame`] returns per dispatch cycle, holding the last fully-formed line from any
//! producer. A producer spamming 100 frames/sec doesn't unbounded-queue work for the renderer;
//! the renderer sees ≤1 frame per cycle. Symmetric with the dwl_stdin reducer's policy (flag 6).

// All items in this module are step-3 wire-points (the host hasn't grown a feed-fd integration
// yet, so cargo flags the whole API surface as unused). Tests exercise them; the wire-up lands
// next phase.
#![allow(dead_code)]

use std::fs;
use std::io::{ErrorKind, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use tracing::{info, warn};
use wafflebar_core::{FeedBlock, FeedEvent, FeedSocketConfig};

/// Hard ceiling on a single line. Real producers emit ~100–300 bytes per frame; anything past
/// this is almost certainly a misbehaving feeder. Beyond the limit, we resync at the next
/// newline rather than unbounded-buffering.
const MAX_LINE: usize = 4096;

/// Per-connected-producer state. We accept multiple producers (debugger plus daemon, say) but
/// always emit the latest frame from *any* of them.
struct Conn {
    stream: UnixStream,
    /// Trailing partial-line bytes carried between read() calls.
    buf: Vec<u8>,
}

/// The listening socket plus active producer connections.
pub struct SomeblocksIntake {
    listener: UnixListener,
    /// Absolute socket path. Owned for cleanup-on-drop / cleanup-on-signal / cleanup-on-EOF.
    path: PathBuf,
    /// Currently-connected producers. Reaped on EOF / read error.
    conns: Vec<Conn>,
    /// Whether to interpret `^fg(#…)`/`^bg(#…)` inline escapes. Today the raw text rides through
    /// untouched in [`FeedBlock::text`]; the eventual escape parser is a plugin/renderer concern
    /// and reads this flag from config.
    #[allow(dead_code)] // step-3 plugin wire-up reads this
    parse_color_escapes: bool,
    /// Set after `cleanup()` runs to make repeat calls / Drop idempotent.
    cleaned: bool,
}

impl SomeblocksIntake {
    /// Bind per the policy in the module docstring. Returns `Ok(None)` when
    /// [`FeedSocketConfig::path`] is `None` (feed deliberately disabled).
    pub fn bind(cfg: &FeedSocketConfig, parse_color_escapes: bool) -> Result<Option<Self>> {
        let Some(path) = cfg.path.clone() else {
            info!("someblocks: feed disabled (no socket path configured)");
            return Ok(None);
        };

        if let Some(parent) = path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating socket parent {}", parent.display()))?;
            }
        }

        // Existing-entry policy. symlink_metadata avoids following a symlink — if someone planted
        // a symlink at our path, we treat it as the symlink itself, not its target.
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                if !meta.file_type().is_socket() {
                    bail!(
                        "someblocks: {} exists and is not a socket — refusing to clobber",
                        path.display()
                    );
                }
                match UnixStream::connect(&path) {
                    Ok(_) => bail!(
                        "someblocks: socket at {} already has a live owner; \
                         refusing to start (singleton)",
                        path.display()
                    ),
                    Err(e) if e.kind() == ErrorKind::ConnectionRefused => {
                        info!(?path, "someblocks: stale socket from prior owner — unlinking");
                        fs::remove_file(&path).with_context(|| {
                            format!("unlinking stale socket {}", path.display())
                        })?;
                    }
                    Err(e) => bail!(
                        "someblocks: probing existing socket {}: {}",
                        path.display(),
                        e
                    ),
                }
            }
            Err(e) if e.kind() == ErrorKind::NotFound => { /* clean slate */ }
            Err(e) => bail!("someblocks: stat {}: {}", path.display(), e),
        }

        let listener = UnixListener::bind(&path)
            .with_context(|| format!("binding someblocks socket {}", path.display()))?;
        listener
            .set_nonblocking(true)
            .context("set listener non-blocking")?;
        info!(?path, "someblocks: bound");
        Ok(Some(Self {
            listener,
            path,
            conns: Vec::new(),
            parse_color_escapes,
            cleaned: false,
        }))
    }

    /// The listening fd for the host's main-loop integration.
    pub fn fd(&self) -> RawFd {
        self.listener.as_raw_fd()
    }

    /// Absolute path the socket is bound at. Surfaced for log messages and for the SIGTERM
    /// handler in step 3.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Unlink the socket. Idempotent — safe to call from SIGTERM, EOF-exit, and `Drop`.
    pub fn cleanup(&mut self) {
        if self.cleaned {
            return;
        }
        self.cleaned = true;
        match fs::remove_file(&self.path) {
            Ok(()) => info!(?self.path, "someblocks: socket unlinked"),
            Err(e) if e.kind() == ErrorKind::NotFound => { /* already gone */ }
            Err(e) => warn!(?self.path, error = %e, "someblocks: cleanup unlink"),
        }
    }

    /// Drain ready I/O. Accepts new producer connections, reads available bytes, parses complete
    /// lines, and returns *at most one* `FeedEvent::Frame` per call (the latest line seen — see
    /// the coalesce-latest note in the module docstring).
    pub fn dispatch(&mut self) -> Vec<FeedEvent> {
        // 1. Accept any new producers.
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Err(e) = stream.set_nonblocking(true) {
                        warn!(error = %e, "someblocks: set_nonblocking on accepted conn");
                        continue;
                    }
                    info!("someblocks: producer connected");
                    self.conns.push(Conn {
                        stream,
                        buf: Vec::new(),
                    });
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => {
                    warn!(error = %e, "someblocks: accept");
                    break;
                }
            }
        }

        // 2. Drain every conn; parse every complete line; keep only the LATEST event.
        let mut latest: Option<FeedEvent> = None;
        let mut dead: Vec<usize> = Vec::new();
        for (idx, conn) in self.conns.iter_mut().enumerate() {
            let mut tmp = [0u8; 4096];
            loop {
                match conn.stream.read(&mut tmp) {
                    Ok(0) => {
                        info!("someblocks: producer disconnected (EOF)");
                        dead.push(idx);
                        break;
                    }
                    Ok(n) => conn.buf.extend_from_slice(&tmp[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) => {
                        warn!(error = %e, "someblocks: read error; closing producer");
                        dead.push(idx);
                        break;
                    }
                }
            }

            // Resync on runaway lines (misbehaving producer): if no newline and we're past
            // MAX_LINE, drop the partial buffer entirely.
            if conn.buf.len() > MAX_LINE && !conn.buf.contains(&b'\n') {
                warn!(
                    "someblocks: line >{} bytes without newline; resyncing",
                    MAX_LINE
                );
                conn.buf.clear();
            }

            // Parse complete lines; coalesce-latest by overwriting `latest` each iteration.
            let mut last_terminator = 0;
            for i in 0..conn.buf.len() {
                if conn.buf[i] == b'\n' {
                    if let Ok(line) = std::str::from_utf8(&conn.buf[last_terminator..i]) {
                        latest = Some(parse_line(line));
                    }
                    last_terminator = i + 1;
                }
            }
            if last_terminator > 0 {
                conn.buf.drain(..last_terminator);
            }
        }
        for idx in dead.into_iter().rev() {
            self.conns.swap_remove(idx);
        }

        latest.into_iter().collect()
    }
}

impl Drop for SomeblocksIntake {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Split a producer line into [`FeedBlock`]s. someblocks tradition: blocks separated by `" | "`,
/// the producer owns ordering. Block names are synthesized positional (`b0`, `b1`, …); the
/// renderer keys on position-stable order, not the name itself.
fn parse_line(line: &str) -> FeedEvent {
    let blocks = line
        .split(" | ")
        .enumerate()
        .map(|(i, text)| FeedBlock {
            name: format!("b{i}"),
            text: text.to_string(),
        })
        .collect();
    FeedEvent::Frame { blocks }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;

    /// Unique per-test path under TMPDIR. Tests run in parallel; we can't share one socket.
    fn temp_socket(suffix: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        p.push(format!("wafflebar-test-{}-{}.sock", pid, suffix));
        // Best-effort cleanup of any leftover from a prior crashed test.
        let _ = fs::remove_file(&p);
        p
    }

    #[test]
    fn bind_returns_none_when_path_is_none() {
        let cfg = FeedSocketConfig { path: None };
        let bound = SomeblocksIntake::bind(&cfg, false).expect("bind");
        assert!(bound.is_none(), "no path → feed disabled");
    }

    #[test]
    fn bind_succeeds_on_clean_slate() {
        let path = temp_socket("clean");
        let cfg = FeedSocketConfig {
            path: Some(path.clone()),
        };
        let bound = SomeblocksIntake::bind(&cfg, false)
            .expect("bind")
            .expect("Some");
        assert_eq!(bound.path(), &path);
        assert!(path.exists());
        drop(bound);
        assert!(!path.exists(), "Drop should cleanup");
    }

    #[test]
    fn bind_refuses_when_a_live_owner_holds_socket() {
        let path = temp_socket("live");
        let cfg = FeedSocketConfig {
            path: Some(path.clone()),
        };
        let _first = SomeblocksIntake::bind(&cfg, false)
            .expect("first bind")
            .expect("Some");
        // Second bind must refuse (singleton).
        let err = match SomeblocksIntake::bind(&cfg, false) {
            Err(e) => e,
            Ok(_) => panic!("expected bind to refuse"),
        };
        let s = format!("{err:#}");
        assert!(
            s.contains("live owner"),
            "error mentions live owner: {s}"
        );
    }

    #[test]
    fn bind_unlinks_stale_socket_with_dead_owner() {
        let path = temp_socket("stale");
        // Create a socket file with no listener → connect will be ECONNREFUSED → treated as stale
        // and reaped. We simulate this by binding then closing immediately, so the file remains
        // but no process accepts on it. UnixListener::bind without binding it to a listener still
        // creates the inode; we drop without unlinking by leaking via mem::forget.
        let listener = UnixListener::bind(&path).expect("seed listener");
        drop(listener); // drop closes the fd; the inode is gone now via implicit close handling?
        // Actually UnixListener::drop does NOT unlink the file. Verify:
        assert!(path.exists(), "dropping UnixListener leaves the inode");
        // Now bind via SomeblocksIntake — should unlink + bind.
        let cfg = FeedSocketConfig {
            path: Some(path.clone()),
        };
        let bound = SomeblocksIntake::bind(&cfg, false)
            .expect("rebind on stale")
            .expect("Some");
        assert_eq!(bound.path(), &path);
    }

    #[test]
    fn bind_refuses_when_path_is_a_regular_file() {
        let path = temp_socket("regular");
        fs::write(&path, b"not a socket").expect("write");
        let cfg = FeedSocketConfig {
            path: Some(path.clone()),
        };
        let err = match SomeblocksIntake::bind(&cfg, false) {
            Err(e) => e,
            Ok(_) => panic!("expected bind to refuse"),
        };
        let s = format!("{err:#}");
        assert!(
            s.contains("not a socket"),
            "error mentions non-socket: {s}"
        );
        // Cleanup the test file (Drop didn't run, no bind succeeded).
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn dispatch_returns_latest_frame_on_burst() {
        let path = temp_socket("burst");
        let cfg = FeedSocketConfig {
            path: Some(path.clone()),
        };
        let mut intake = SomeblocksIntake::bind(&cfg, false)
            .expect("bind")
            .expect("Some");

        // Connect a producer, write three frames, dispatch — coalesce-latest must yield one
        // event whose blocks come from the LAST frame.
        let mut producer = UnixStream::connect(&path).expect("connect");
        producer
            .write_all(b"frame-one\nframe-two\nframe-three\n")
            .expect("write");
        producer.flush().expect("flush");

        // Give the kernel a beat to make the writes readable.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let events = intake.dispatch();
        assert_eq!(events.len(), 1, "coalesce-latest: one event per dispatch");
        let FeedEvent::Frame { blocks } = &events[0];
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "frame-three");
    }

    #[test]
    fn parse_line_splits_blocks_on_pipe_separator() {
        let FeedEvent::Frame { blocks } = parse_line("alpha | beta | gamma");
        let texts: Vec<&str> = blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, vec!["alpha", "beta", "gamma"]);
        let names: Vec<&str> = blocks.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, vec!["b0", "b1", "b2"]);
    }

    #[test]
    fn cleanup_is_idempotent() {
        let path = temp_socket("idemp");
        let cfg = FeedSocketConfig {
            path: Some(path.clone()),
        };
        let mut intake = SomeblocksIntake::bind(&cfg, false)
            .expect("bind")
            .expect("Some");
        intake.cleanup();
        intake.cleanup(); // must not panic / log noise
        assert!(!path.exists());
    }
}
