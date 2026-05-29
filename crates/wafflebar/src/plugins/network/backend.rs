//! NetworkManager backend for the network plugin. **All** zbus lives here; the reducer (`super`)
//! sees only the typed [`NetworkState`] this derives.
//!
//! Integration: an `async fn` driven by `glib::spawn_future_local` on the main context (see
//! `app.rs`), so it runs on our existing loop and `emit` runs on the main thread — no bridge thread
//! (zbus carries its own internal reactor, like libpulse's).
//!
//! **Poll, not signals.** Throughput (Mbps up/down) is inherently periodic — it's the *rate* of
//! `/sys/class/net/<iface>/statistics/{rx,tx}_bytes` — so the backend runs a ~1s poll loop: each tick
//! it re-derives the primary connection's identity (kernel interface name, type, wifi strength) from
//! NM *and* samples throughput, then emits. NM property reads are cheap and the reducer's
//! value-compare guard suppresses no-op re-renders, so idle cost stays low; this replaced the older
//! `PropertiesChanged` subscription (which couldn't drive the throughput tick). The sleep is part of
//! the future, so an F2c backend abort cancels it cleanly (no leaked GLib source).
//!
//! The [`NetworkBackend`] trait is the plugin-internal seam: `NmBackend` is the only impl today; a
//! `ProcNetDevBackend` (`/proc/net/dev`, no NM) would implement the same `run` for NM-less systems.

use std::time::{Duration, Instant};

use gtk4::glib;
use tracing::warn;
use wafflebar_core::NetworkState;
use zbus::zvariant::OwnedObjectPath;
use zbus::{Connection, Proxy};

const NM_SVC: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_IFACE: &str = "org.freedesktop.NetworkManager";
const POLL: Duration = Duration::from_secs(1);

/// Plugin-internal source of [`NetworkState`] changes. Runs on the GLib main context and pushes
/// each derived state to `emit`.
pub trait NetworkBackend {
    async fn run(self, emit: Box<dyn Fn(NetworkState)>);
}

/// NetworkManager-over-D-Bus backend (the v1 default).
pub struct NmBackend;

/// The primary connection's identity (everything but throughput), from NetworkManager.
struct Identity {
    interface: String,
    wireless: bool,
    strength: u8,
}

impl NetworkBackend for NmBackend {
    async fn run(self, emit: Box<dyn Fn(NetworkState)>) {
        let conn = match Connection::system().await {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "network: no system bus; network plugin stays hidden");
                return;
            }
        };
        let nm = match Proxy::new(&conn, NM_SVC, NM_PATH, NM_IFACE).await {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "network: NetworkManager proxy failed; plugin stays hidden");
                return;
            }
        };
        // Probe once so an NM-less system hides the plugin rather than emitting Disconnected forever.
        // TODO(network): fall back to a ProcNetDevBackend (/proc/net/dev) instead of hiding.
        if let Err(e) = identity(&conn, &nm).await {
            warn!(error = %e, "network: NetworkManager unavailable; network plugin stays hidden");
            return;
        }

        // last sample for the throughput rate: (interface, rx_bytes, tx_bytes, when).
        let mut last: Option<(String, u64, u64, Instant)> = None;
        loop {
            match identity(&conn, &nm).await {
                Ok(None) => {
                    last = None; // dropped connection — reset the rate baseline
                    emit(NetworkState::Disconnected);
                }
                Ok(Some(id)) => {
                    let (rx_bps, tx_bps) = sample_throughput(&id.interface, &mut last);
                    emit(if id.wireless {
                        NetworkState::Wireless {
                            interface: id.interface,
                            strength: id.strength,
                            rx_bps,
                            tx_bps,
                        }
                    } else {
                        NetworkState::Wired { interface: id.interface, rx_bps, tx_bps }
                    });
                }
                Err(_) => { /* transient D-Bus hiccup — skip this tick, keep the last view */ }
            }
            glib::timeout_future(POLL).await;
        }
    }
}

/// The primary connection's identity, or `Ok(None)` when nothing is connected.
async fn identity(conn: &Connection, nm: &Proxy<'_>) -> zbus::Result<Option<Identity>> {
    let primary: OwnedObjectPath = nm.get_property("PrimaryConnection").await?;
    if primary.as_str() == "/" {
        return Ok(None);
    }
    let active = Proxy::new(conn, NM_SVC, primary, "org.freedesktop.NetworkManager.Connection.Active")
        .await?;
    let kind: String = active.get_property("Type").await?;
    let interface = device_interface(conn, &active).await.unwrap_or_default();
    let wireless = kind == "802-11-wireless";
    let strength = if wireless { wifi_strength(conn, &active).await.unwrap_or(0) } else { 0 };
    Ok(Some(Identity { interface, wireless, strength }))
}

/// The kernel interface name (`eth0`/`wlan0`/`enp…`) of the active connection's first device — what
/// `ifconfig`/`ip` show, which the user reads as the interface, not NM's connection `Id`.
async fn device_interface(conn: &Connection, active: &Proxy<'_>) -> Option<String> {
    let devices: Vec<OwnedObjectPath> = active.get_property("Devices").await.ok()?;
    let dev_path = devices.into_iter().next()?;
    let dev = Proxy::new(conn, NM_SVC, dev_path, "org.freedesktop.NetworkManager.Device").await.ok()?;
    dev.get_property::<String>("Interface").await.ok()
}

/// Strength (0..=100) of the active connection's wireless access point, if any.
async fn wifi_strength(conn: &Connection, active: &Proxy<'_>) -> Option<u8> {
    let devices: Vec<OwnedObjectPath> = active.get_property("Devices").await.ok()?;
    for dev_path in devices {
        let dev = Proxy::new(conn, NM_SVC, dev_path, "org.freedesktop.NetworkManager.Device.Wireless")
            .await
            .ok()?;
        if let Ok(ap) = dev.get_property::<OwnedObjectPath>("ActiveAccessPoint").await {
            if ap.as_str() != "/" {
                let ap = Proxy::new(conn, NM_SVC, ap, "org.freedesktop.NetworkManager.AccessPoint")
                    .await
                    .ok()?;
                if let Ok(strength) = ap.get_property::<u8>("Strength").await {
                    return Some(strength);
                }
            }
        }
    }
    None
}

/// Down/up throughput in **bits per second** from `/sys/class/net/<iface>/statistics`, as the delta
/// since the previous sample over elapsed time. Returns `(0, 0)` on the first sample or when the
/// interface changed (no baseline yet); `last` is updated to this sample.
fn sample_throughput(iface: &str, last: &mut Option<(String, u64, u64, Instant)>) -> (u64, u64) {
    let now = Instant::now();
    let rx = read_counter(iface, "rx_bytes");
    let tx = read_counter(iface, "tx_bytes");
    let rate = match (rx, tx, last.as_ref()) {
        (Some(rx), Some(tx), Some((li, lrx, ltx, lt))) if li == iface => {
            let dt = now.duration_since(*lt).as_secs_f64().max(0.001);
            let down = (rx.saturating_sub(*lrx) as f64 / dt * 8.0) as u64;
            let up = (tx.saturating_sub(*ltx) as f64 / dt * 8.0) as u64;
            (down, up)
        }
        _ => (0, 0),
    };
    if let (Some(rx), Some(tx)) = (rx, tx) {
        *last = Some((iface.to_string(), rx, tx, now));
    }
    rate
}

fn read_counter(iface: &str, stat: &str) -> Option<u64> {
    std::fs::read_to_string(format!("/sys/class/net/{iface}/statistics/{stat}"))
        .ok()?
        .trim()
        .parse()
        .ok()
}
