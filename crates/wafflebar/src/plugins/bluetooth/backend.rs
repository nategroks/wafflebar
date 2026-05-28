//! BlueZ backend for the Bluetooth plugin. **All** zbus lives here; the reducer (`super`) sees only
//! the typed [`BluetoothState`] this derives and issues [`BluetoothCommand`]s back through `execute`.
//!
//! Like the network backend it's poll-based (a ~2s `GetManagedObjects` walk of BlueZ's object tree),
//! driven by `glib::spawn_future_local` on the main context — Bluetooth changes are infrequent and
//! the reducer's value-compare guard suppresses no-op re-renders. It keeps the system-bus
//! `Connection` (and the adapter path) so `execute` can perform power/connect/disconnect calls; the
//! poll loop's `JoinHandle` is the cancellation handle (abort drops the future at its await point).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gtk4::glib;
use tracing::warn;
use wafflebar_core::{BluetoothCommand, BluetoothState, BtDevice};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

const BLUEZ: &str = "org.bluez";
const ADAPTER_IFACE: &str = "org.bluez.Adapter1";
const DEVICE_IFACE: &str = "org.bluez.Device1";
const POLL: Duration = Duration::from_secs(2);

/// All managed objects: path → interface → property → value (the ObjectManager reply shape).
type Managed = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

/// The host-owned BlueZ backend: shares the system-bus connection + adapter path for `execute`, and
/// owns the poll future's handle so it can be stopped.
pub struct BtBackend {
    conn: RefCell<Option<Connection>>,
    adapter: RefCell<Option<String>>,
    handle: RefCell<Option<glib::JoinHandle<()>>>,
}

impl BtBackend {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            conn: RefCell::new(None),
            adapter: RefCell::new(None),
            handle: RefCell::new(None),
        })
    }

    /// Start the poll loop: connect to the system bus, then every [`POLL`] derive + `emit` the state.
    pub fn start(self: &Rc<Self>, emit: Rc<dyn Fn(BluetoothState)>) {
        let this = self.clone();
        let handle = glib::spawn_future_local(async move {
            let conn = match Connection::system().await {
                Ok(c) => c,
                Err(e) => return warn!(error = %e, "bluetooth: no system bus; plugin stays hidden"),
            };
            let om = match Proxy::new(&conn, BLUEZ, "/", "org.freedesktop.DBus.ObjectManager").await {
                Ok(p) => p,
                Err(e) => return warn!(error = %e, "bluetooth: BlueZ ObjectManager proxy failed"),
            };
            *this.conn.borrow_mut() = Some(conn);
            loop {
                if let Ok(objs) = om.call::<_, _, Managed>("GetManagedObjects", &()).await {
                    let (state, adapter) = derive(&objs);
                    *this.adapter.borrow_mut() = adapter;
                    emit(state);
                }
                glib::timeout_future(POLL).await;
            }
        });
        *self.handle.borrow_mut() = Some(handle);
    }

    /// Stop the poll loop (cancellation via the future's handle). Idempotent.
    pub fn stop(&self) {
        if let Some(h) = self.handle.borrow_mut().take() {
            h.abort();
        }
    }

    /// Perform a command against BlueZ (fire-and-forget on the main context).
    pub fn execute(&self, cmd: &BluetoothCommand) {
        let Some(conn) = self.conn.borrow().clone() else { return };
        let adapter = self.adapter.borrow().clone();
        let cmd = cmd.clone();
        glib::spawn_future_local(async move {
            let result = match &cmd {
                BluetoothCommand::SetPowered(on) => match adapter {
                    Some(path) => set_powered(&conn, &path, *on).await,
                    None => return,
                },
                BluetoothCommand::Connect(path) => device_call(&conn, path, "Connect").await,
                BluetoothCommand::Disconnect(path) => device_call(&conn, path, "Disconnect").await,
            };
            if let Err(e) = result {
                warn!(?cmd, error = %e, "bluetooth: command failed");
            }
        });
    }
}

/// Collapse the ObjectManager reply to (state, adapter-path). Lists only *paired* devices.
fn derive(objs: &Managed) -> (BluetoothState, Option<String>) {
    let mut state = BluetoothState::default();
    let mut adapter = None;
    for (path, ifaces) in objs {
        if let Some(props) = ifaces.get(ADAPTER_IFACE) {
            state.present = true;
            state.powered = get_bool(props, "Powered").unwrap_or(false);
            adapter = Some(path.to_string());
        }
        if let Some(props) = ifaces.get(DEVICE_IFACE) {
            if get_bool(props, "Paired").unwrap_or(false) {
                let name = get_str(props, "Alias")
                    .or_else(|| get_str(props, "Name"))
                    .unwrap_or_else(|| path.to_string());
                let connected = get_bool(props, "Connected").unwrap_or(false);
                state.devices.push(BtDevice { path: path.to_string(), name, connected });
            }
        }
    }
    // Stable, useful order: connected first, then by name.
    state.devices.sort_by(|a, b| b.connected.cmp(&a.connected).then(a.name.cmp(&b.name)));
    (state, adapter)
}

async fn set_powered(conn: &Connection, adapter_path: &str, on: bool) -> zbus::Result<()> {
    let props = Proxy::new(conn, BLUEZ, adapter_path, "org.freedesktop.DBus.Properties").await?;
    props.call("Set", &(ADAPTER_IFACE, "Powered", Value::from(on))).await
}

async fn device_call(conn: &Connection, device_path: &str, method: &str) -> zbus::Result<()> {
    let dev = Proxy::new(conn, BLUEZ, device_path.to_string(), DEVICE_IFACE).await?;
    dev.call(method, &()).await
}

fn get_bool(props: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    props.get(key).and_then(|v| bool::try_from(v.clone()).ok())
}
fn get_str(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props.get(key).and_then(|v| String::try_from(v.clone()).ok())
}
