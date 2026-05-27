//! NetworkManager backend for the network plugin. **All** zbus lives here; the reducer (`super`)
//! sees only the typed [`NetworkState`] this derives.
//!
//! Integration: this is an `async fn` driven by `glib::spawn_future_local` on the main context (see
//! `app.rs`), so the subscription wakes our existing loop and `emit` runs on the main thread — no
//! bridge thread in our code (zbus carries its own internal reactor, an implementation detail like
//! libpulse's). Lifecycle: connect to the system bus → derive once → subscribe to the NM object's
//! `PropertiesChanged` → re-derive on each. NM absent → one warn, the future returns, the plugin
//! stays `Empty`.
//!
//! The [`NetworkBackend`] trait is the plugin-internal seam: `NmBackend` is the only impl today; a
//! `ProcNetDevBackend` (`/proc/net/dev`, no NM) would implement the same `run` for NM-less systems.

use tracing::warn;
use wafflebar_core::NetworkState;
use zbus::export::ordered_stream::OrderedStreamExt;
use zbus::zvariant::OwnedObjectPath;
use zbus::{Connection, Proxy};

const NM_SVC: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_IFACE: &str = "org.freedesktop.NetworkManager";

/// Plugin-internal source of [`NetworkState`] changes. Runs on the GLib main context and pushes
/// each derived state to `emit`.
pub trait NetworkBackend {
    async fn run(self, emit: Box<dyn Fn(NetworkState)>);
}

/// NetworkManager-over-D-Bus backend (the v1 default).
pub struct NmBackend;

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
        // Initial state; failure here means NM isn't on the bus (v1 is NM-only).
        // TODO(network): fall back to a ProcNetDevBackend (/proc/net/dev) instead of hiding.
        match derive(&conn, &nm).await {
            Ok(state) => emit(state),
            Err(e) => {
                warn!(error = %e, "network: NetworkManager unavailable; network plugin stays hidden");
                return;
            }
        }
        // Re-derive on every change to the NM object (State / PrimaryConnection / connectivity).
        let props = match Proxy::new(&conn, NM_SVC, NM_PATH, "org.freedesktop.DBus.Properties").await
        {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "network: could not watch NM properties");
                return;
            }
        };
        let mut changes = match props.receive_signal("PropertiesChanged").await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "network: PropertiesChanged subscription failed");
                return;
            }
        };
        // TODO(network): also subscribe to the active AccessPoint's PropertiesChanged so wifi
        // signal strength updates live, not only on connection changes.
        while changes.next().await.is_some() {
            if let Ok(state) = derive(&conn, &nm).await {
                emit(state);
            }
        }
    }
}

/// Collapse NetworkManager's object graph to the displayed state.
async fn derive(conn: &Connection, nm: &Proxy<'_>) -> zbus::Result<NetworkState> {
    let primary: OwnedObjectPath = nm.get_property("PrimaryConnection").await?;
    if primary.as_str() == "/" {
        return Ok(NetworkState::Disconnected);
    }
    let active = Proxy::new(conn, NM_SVC, primary, "org.freedesktop.NetworkManager.Connection.Active")
        .await?;
    let name: String = active.get_property("Id").await?;
    let kind: String = active.get_property("Type").await?;
    if kind == "802-11-wireless" {
        let strength = wifi_strength(conn, &active).await.unwrap_or(0);
        Ok(NetworkState::Wireless { name, strength })
    } else {
        Ok(NetworkState::Wired { name })
    }
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
