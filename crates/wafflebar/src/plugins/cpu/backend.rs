//! `/proc/stat` polling backend for the CPU plugin. All `/proc` reads + parsing live here; the
//! reducer (`super`) sees only typed [`CpuState`] deltas.
//!
//! **Second `/proc` polling backend** (after memory). Per the rule from before C1, it is kept
//! independent — *no* shared `ProcPollingBackend<T>` extracted. The two differ in the real way that
//! matters: memory derives from a single stateless read, CPU needs **delta-tracking** (jiffy counts
//! are cumulative, so percent-busy is a difference between consecutive reads). A disk-I/O poller
//! would be the third instance and the moment to look at all three for genuine shared shape.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gtk4::glib;
use tracing::warn;
use wafflebar_core::CpuState;

/// 1 Hz: the canonical desktop cadence, and the rate where rounded-percent value-compare hides
/// sub-1%/s drift. TODO(cpu): expose cadence in plugin config when Phase F lands.
pub const INTERVAL: Duration = Duration::from_secs(1);

/// Plugin-internal source of [`CpuState`] deltas (the polling seam; only impl is `ProcStatBackend`).
pub trait CpuBackend {
    fn start(self, emit: Box<dyn Fn(CpuState)>) -> glib::SourceId;
}

pub struct ProcStatBackend;

impl CpuBackend for ProcStatBackend {
    fn start(self, emit: Box<dyn Fn(CpuState)>) -> glib::SourceId {
        // Previous read's cumulative per-core (busy, total), carried across ticks. The timeout
        // closure is FnMut, so plain ownership works — no interior mutability needed.
        let mut prev: Option<Vec<CoreTimes>> = None;
        glib::timeout_add_local(INTERVAL, move || {
            if let Some(cur) = read_stat() {
                match &prev {
                    // First tick has no delta to report — store and stay silent. Same on a core
                    // count change (CPU hotplug): rebuild rather than diff mismatched vectors.
                    Some(p) if p.len() == cur.len() => emit(sum_deltas(p, &cur)),
                    _ => {}
                }
                prev = Some(cur);
            }
            // TODO(cpu): after a long suspend the cached prev is stale, so the first post-resume
            // delta reads ~100% for one tick. Accepted for v1 (one tick at 1 Hz); detect the clock
            // gap and discard that tick if it ever becomes visibly annoying.
            glib::ControlFlow::Continue
        })
    }
}

/// Cumulative busy/total jiffies for one core (the running counters `/proc/stat` reports).
#[derive(Clone, Copy)]
pub struct CoreTimes {
    busy: u64,
    total: u64,
}

fn read_stat() -> Option<Vec<CoreTimes>> {
    // /proc is kernel-generated per-read; opening fresh is standard and no slower than caching.
    match std::fs::read_to_string("/proc/stat") {
        Ok(content) => parse_stat(&content),
        Err(e) => {
            warn_once(&READ_WARNED, || warn!(error = %e, "cpu: cannot read /proc/stat"));
            None
        }
    }
}

/// Parse per-core cumulative times from `/proc/stat`. Skips the aggregate `cpu ` line — we sum the
/// per-core lines ourselves, which is the source of truth (the `cpu` line can disagree by rounding
/// under some virtualization). `None` if no usable core lines (transient bad read).
fn parse_stat(content: &str) -> Option<Vec<CoreTimes>> {
    let mut cores = Vec::new();
    for line in content.lines() {
        let Some(rest) = line.strip_prefix("cpu") else {
            break; // cpu* lines are first in /proc/stat; stop at the first non-cpu line
        };
        // Per-core line is "cpuN ..." → a digit follows the prefix; the aggregate "cpu  ..." line
        // has whitespace there. Skip the aggregate (we sum per-core ourselves).
        if !rest.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        // rest is "N <user> <nice> <system> <idle> <iowait> …"; drop the core index, take the rest.
        let vals: Vec<u64> = rest.split_whitespace().skip(1).filter_map(|v| v.parse().ok()).collect();
        if vals.len() < 4 {
            return None; // truncated line: user/nice/system/idle are the minimum
        }
        let idle = vals[3] + vals.get(4).copied().unwrap_or(0); // idle + iowait (iowait = idle)
        let total: u64 = vals.iter().sum();
        cores.push(CoreTimes { busy: total.saturating_sub(idle), total });
    }
    (!cores.is_empty()).then_some(cores)
}

/// Sum per-core deltas between two reads into an aggregate [`CpuState`]. Per-core, not the `cpu`
/// line, because that's the data v2's per-core display will reuse.
fn sum_deltas(prev: &[CoreTimes], cur: &[CoreTimes]) -> CpuState {
    let mut busy_delta = 0;
    let mut total_delta = 0;
    for (p, c) in prev.iter().zip(cur) {
        busy_delta += c.busy.saturating_sub(p.busy);
        total_delta += c.total.saturating_sub(p.total);
    }
    CpuState { busy_delta, total_delta }
}

static READ_WARNED: AtomicBool = AtomicBool::new(false);

fn warn_once(flag: &AtomicBool, f: impl FnOnce()) {
    if !flag.swap(true, Ordering::Relaxed) {
        f();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Two cores; second value set is one tick later.
    const T0: &str = "\
cpu  100 0 100 800 0 0 0 0 0 0
cpu0 50 0 50 400 0 0 0 0 0 0
cpu1 50 0 50 400 0 0 0 0 0 0
intr 12345
ctxt 67890
";
    // cpu0 fully busy (+100 busy, +100 total), cpu1 fully idle (+0 busy, +100 total).
    const T1: &str = "\
cpu  200 0 100 900 0 0 0 0 0 0
cpu0 150 0 50 400 0 0 0 0 0 0
cpu1 50 0 50 500 0 0 0 0 0 0
intr 99999
";

    #[test]
    fn parses_per_core_skipping_aggregate() {
        let cores = parse_stat(T0).unwrap();
        assert_eq!(cores.len(), 2, "two cpuN lines, aggregate 'cpu' skipped");
        // cpu0: total = 50+0+50+400 = 500, idle = 400+0 = 400, busy = 100.
        assert_eq!(cores[0].busy, 100);
        assert_eq!(cores[0].total, 500);
    }

    #[test]
    fn sums_deltas_across_cores() {
        let p = parse_stat(T0).unwrap();
        let c = parse_stat(T1).unwrap();
        let d = sum_deltas(&p, &c);
        // cpu0 busy +100; cpu1 busy +0 → busy_delta 100. Each core total +100 → total_delta 200.
        assert_eq!(d.busy_delta, 100);
        assert_eq!(d.total_delta, 200); // → 50% aggregate, which is correct for 1-of-2 cores pinned
    }

    #[test]
    fn iowait_counts_as_idle_not_busy() {
        // A core with all its non-idle time in iowait should read as idle (busy stays low).
        let s = "cpu0 0 0 0 100 900 0 0 0 0 0\n";
        let cores = parse_stat(s).unwrap();
        // total = 1000, idle = 100 + 900 (iowait) = 1000, busy = 0.
        assert_eq!(cores[0].busy, 0);
        assert_eq!(cores[0].total, 1000);
    }

    #[test]
    fn malformed_or_empty_is_none() {
        assert!(parse_stat("").is_none());
        assert!(parse_stat("intr 1\nctxt 2\n").is_none()); // no cpu lines
        assert!(parse_stat("cpu0 1 2\n").is_none()); // truncated (<4 fields)
    }
}
