//! `/proc/meminfo` polling backend for the memory plugin. All `/proc` reads + parsing live here;
//! the reducer (`super`) sees only typed [`MemoryState`]s.
//!
//! This is the **polling** shape (vs the subscription shapes of volume/network): a
//! `glib::timeout_add_local` on the main context ticks at the configured interval, reads + parses
//! `/proc/meminfo`, and emits. Everything stays on the GLib loop — no thread, no async.
//!
//! The [`MemoryBackend`] trait is the plugin-internal seam; `ProcMemBackend` is the only impl. A
//! future CPU/disk poller is structurally similar but parses different files and carries different
//! between-tick state, so they stay independent backends — no shared `PollingBackend<T>` until a
//! third instance proves the duplication is real.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gtk4::glib;
use tracing::warn;
use wafflebar_core::MemoryState;

/// Plugin-internal source of [`MemoryState`] ticks.
pub trait MemoryBackend {
    /// Begin polling every `interval`, pushing each reading to `emit`. Returns the GLib source id
    /// (kept by the caller; the source itself owns the closure and outlives the id).
    fn start(self, interval: Duration, emit: Box<dyn Fn(MemoryState)>) -> glib::SourceId;
}

pub struct ProcMemBackend;

impl MemoryBackend for ProcMemBackend {
    fn start(self, interval: Duration, emit: Box<dyn Fn(MemoryState)>) -> glib::SourceId {
        tick(&*emit); // emit once now so the bar isn't blank until the first interval elapses
        glib::timeout_add_local(interval, move || {
            tick(&*emit);
            glib::ControlFlow::Continue
        })
    }
}

/// One poll. On a read/parse failure we **skip** (the plugin keeps its last state — don't flash
/// Empty on a single bad read); a missing file warns once so a broken environment doesn't storm
/// the log. (We don't downgrade to Empty after a successful read: `/proc/meminfo` permanently
/// vanishing mid-run isn't a real Linux scenario, and flashing Empty would be worse than stale.)
fn tick(emit: &dyn Fn(MemoryState)) {
    if let Some(state) = read_meminfo() {
        emit(state);
    }
}

fn read_meminfo() -> Option<MemoryState> {
    // /proc files are kernel-generated per-read; opening fresh is the standard pattern and not
    // slower than caching the handle.
    match std::fs::read_to_string("/proc/meminfo") {
        Ok(content) => parse_meminfo(&content),
        Err(e) => {
            warn_once(&READ_WARNED, || warn!(error = %e, "memory: cannot read /proc/meminfo"));
            None
        }
    }
}

/// Parse the fields we use from `/proc/meminfo` (line-oriented `Key:   value kB`). `None` if the
/// content is malformed enough that we can't compute usage (treated as a transient bad read).
fn parse_meminfo(content: &str) -> Option<MemoryState> {
    let (mut total, mut avail, mut free, mut buffers, mut cached) = (None, None, None, None, None);
    let (mut swap_total, mut swap_free) = (None, None);
    for line in content.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue; // skip a malformed line, not the whole file
        };
        let val = rest.split_whitespace().next().and_then(|v| v.parse::<u64>().ok());
        match key {
            "MemTotal" => total = val,
            "MemAvailable" => avail = val,
            "MemFree" => free = val,
            "Buffers" => buffers = val,
            "Cached" => cached = val,
            "SwapTotal" => swap_total = val,
            "SwapFree" => swap_free = val,
            _ => {}
        }
    }
    let total_kb = total?;
    if total_kb == 0 {
        return None;
    }
    // Prefer MemAvailable; pre-3.14 kernels lack it → fall back to the old free+buffers+cached.
    let available = match avail {
        Some(a) => a,
        None => {
            warn_once(&MEMAVAIL_WARNED, || {
                warn!("memory: MemAvailable absent (old kernel); using MemFree+Buffers+Cached")
            });
            free? + buffers.unwrap_or(0) + cached.unwrap_or(0)
        }
    };
    let swap_total_kb = swap_total.unwrap_or(0);
    let swap_used_kb = swap_total_kb.saturating_sub(swap_free.unwrap_or(swap_total_kb));
    Some(MemoryState {
        total_kb,
        used_kb: total_kb.saturating_sub(available),
        swap_total_kb,
        swap_used_kb,
    })
}

static READ_WARNED: AtomicBool = AtomicBool::new(false);
static MEMAVAIL_WARNED: AtomicBool = AtomicBool::new(false);

/// Run `f` only the first time for a given flag, so recurring failures don't storm the log.
fn warn_once(flag: &AtomicBool, f: impl FnOnce()) {
    if !flag.swap(true, Ordering::Relaxed) {
        f();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
MemTotal:       16000000 kB
MemFree:         1000000 kB
MemAvailable:   10000000 kB
Buffers:          200000 kB
Cached:          3000000 kB
SwapTotal:       2000000 kB
SwapFree:        1500000 kB
";

    #[test]
    fn parses_known_values() {
        let s = parse_meminfo(SAMPLE).unwrap();
        assert_eq!(s.total_kb, 16_000_000);
        assert_eq!(s.used_kb, 6_000_000); // 16M - 10M available
        assert_eq!(s.swap_total_kb, 2_000_000);
        assert_eq!(s.swap_used_kb, 500_000); // 2M - 1.5M free
    }

    #[test]
    fn falls_back_to_old_formula_without_memavailable() {
        let no_avail = "\
MemTotal:       16000000 kB
MemFree:         1000000 kB
Buffers:          200000 kB
Cached:          3000000 kB
";
        let s = parse_meminfo(no_avail).unwrap();
        // available = free+buffers+cached = 4.2M → used = 16M - 4.2M
        assert_eq!(s.used_kb, 16_000_000 - 4_200_000);
    }

    #[test]
    fn malformed_or_truncated_is_none_not_panic() {
        assert!(parse_meminfo("not meminfo at all").is_none());
        assert!(parse_meminfo("").is_none());
        // Has MemTotal but nothing to compute availability from → None (transient bad read).
        assert!(parse_meminfo("MemTotal:  16000000 kB\n").is_none());
        // Zero total → None (no divide-by-zero downstream).
        assert!(parse_meminfo("MemTotal: 0 kB\nMemAvailable: 0 kB\n").is_none());
    }
}
