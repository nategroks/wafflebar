//! `/sys/class/net/*` polling backend for the interface plugin. Reads every kernel interface's
//! statistics once per second, computes throughput as the delta over elapsed time, emits a single
//! [`InterfaceState`] containing every link. Plugins filter by name.
//!
//! Same shape as the memory backend: `glib::timeout_add_local` on the main context, no thread,
//! no async — all `/sys` reads are non-blocking.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use gtk4::glib;
use tracing::warn;
use wafflebar_core::{InterfaceLink, InterfaceState};

const SYS_NET: &str = "/sys/class/net";
const POLL: Duration = Duration::from_secs(1);

pub trait InterfaceBackend {
    fn start(self, emit: Box<dyn Fn(InterfaceState)>) -> glib::SourceId;
}

pub struct SysNetBackend;

impl InterfaceBackend for SysNetBackend {
    fn start(self, emit: Box<dyn Fn(InterfaceState)>) -> glib::SourceId {
        // last per-interface sample: (rx_bytes, tx_bytes, when). Kept across ticks for rate calc.
        let mut last: HashMap<String, (u64, u64, Instant)> = HashMap::new();
        // Emit once now so the plugin isn't blank until the first interval elapses.
        emit(snapshot(&mut last));
        glib::timeout_add_local(POLL, move || {
            emit(snapshot(&mut last));
            glib::ControlFlow::Continue
        })
    }
}

/// One poll: read every entry under `/sys/class/net`, compute deltas vs the previous tick.
fn snapshot(last: &mut HashMap<String, (u64, u64, Instant)>) -> InterfaceState {
    let mut links = Vec::new();
    let now = Instant::now();
    let entries = match std::fs::read_dir(SYS_NET) {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "interface: cannot list /sys/class/net; skipping");
            return InterfaceState { links };
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // Skip loopback + bridges + virtual tunnels — the user almost never wants those on the bar.
        // (They can still configure interface=lo if they really do.)
        if matches!(name.as_str(), "lo") {
            // honour lo iff the user explicitly asked for it via config — emit it so the plugin
            // can find it, but it lands in the snapshot like anyone else.
        }
        let base = entry.path();
        let rx = read_u64(&base.join("statistics/rx_bytes"));
        let tx = read_u64(&base.join("statistics/tx_bytes"));
        let up = read_string(&base.join("operstate"))
            .map(|s| s.trim() == "up")
            .unwrap_or(false);
        let wireless = base.join("wireless").exists();
        let (rx_bps, tx_bps) = match (rx, tx, last.get(&name)) {
            (Some(rx), Some(tx), Some((lrx, ltx, lt))) => {
                let dt = now.duration_since(*lt).as_secs_f64().max(0.001);
                let down = (rx.saturating_sub(*lrx) as f64 / dt * 8.0) as u64;
                let up = (tx.saturating_sub(*ltx) as f64 / dt * 8.0) as u64;
                (down, up)
            }
            _ => (0, 0),
        };
        if let (Some(rx), Some(tx)) = (rx, tx) {
            last.insert(name.clone(), (rx, tx, now));
        }
        links.push(InterfaceLink {
            name,
            up,
            wireless,
            rx_bps,
            tx_bps,
        });
    }
    // Drop entries from `last` that are no longer present (interface removed).
    let present: std::collections::HashSet<&str> =
        links.iter().map(|l| l.name.as_str()).collect();
    last.retain(|k, _| present.contains(k.as_str()));
    InterfaceState { links }
}

fn read_u64(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_string(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}
