//! BlueZ backend for the Bluetooth plugin. **All** zbus lives here; the reducer (`super`) sees only
//! the typed [`BluetoothState`] this derives and issues [`BluetoothCommand`]s back through `execute`.
//!
//! Like the network backend it's poll-based (a ~2s `GetManagedObjects` walk of BlueZ's object tree),
//! driven by `glib::spawn_future_local` on the main context. It keeps the system-bus `Connection`
//! (and adapter path) so `execute` can perform power/discovery/connect/pair calls; the poll
//! `JoinHandle` is the cancellation handle.
//!
//! **Pairing agent.** To pair, BlueZ needs an `org.bluez.Agent1`. We register a `NoInputNoOutput`
//! agent that auto-accepts authorization (the "Just Works" model — headphones/speakers/mice). It
//! cannot supply a PIN/passkey, so devices that demand one fail rather than pair silently. NOTE:
//! while registered, a remote-initiated pairing (if the adapter is left pairable) is auto-accepted —
//! the standard `bluetoothctl agent on` trade-off; a prompting agent would need passkey UI (v2).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gtk4::glib;
use tracing::warn;
use wafflebar_core::{BluetoothCommand, BluetoothState, BtDevice};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

const BLUEZ: &str = "org.bluez";
const ADAPTER_IFACE: &str = "org.bluez.Adapter1";
const DEVICE_IFACE: &str = "org.bluez.Device1";
const AGENT_PATH: &str = "/dev/wafflebar/btagent";
const POLL: Duration = Duration::from_secs(2);

type Managed = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

/// Auto-accepting "Just Works" pairing agent (`NoInputNoOutput`). Methods returning `()` accept;
/// PIN/passkey requests fail (we have no input). See the module note on the trade-off.
struct Agent;

#[zbus::interface(name = "org.bluez.Agent1")]
impl Agent {
    async fn release(&self) {}
    async fn request_authorization(&self, _device: OwnedObjectPath) {}
    async fn authorize_service(&self, _device: OwnedObjectPath, _uuid: String) {}
    async fn request_confirmation(&self, _device: OwnedObjectPath, _passkey: u32) {}
    async fn display_passkey(&self, _device: OwnedObjectPath, _passkey: u32, _entered: u16) {}
    async fn display_pin_code(&self, _device: OwnedObjectPath, _pincode: String) {}
    async fn request_passkey(&self, _device: OwnedObjectPath) -> zbus::fdo::Result<u32> {
        Err(zbus::fdo::Error::Failed("no input device".into()))
    }
    async fn request_pin_code(&self, _device: OwnedObjectPath) -> zbus::fdo::Result<String> {
        Err(zbus::fdo::Error::Failed("no input device".into()))
    }
    async fn cancel(&self) {}
}

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

    /// Connect, register the pairing agent, then poll every [`POLL`] and `emit` the derived state.
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
            register_agent(&conn).await; // best-effort; pairing needs it, the rest works without
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
                BluetoothCommand::SetDiscovering(on) => match adapter {
                    Some(path) => {
                        adapter_call(&conn, &path, if *on { "StartDiscovery" } else { "StopDiscovery" }).await
                    }
                    None => return,
                },
                BluetoothCommand::Connect(path) => device_call(&conn, path, "Connect").await,
                BluetoothCommand::Disconnect(path) => device_call(&conn, path, "Disconnect").await,
                BluetoothCommand::Pair(path) => {
                    // Pair, then connect (BlueZ doesn't auto-connect after pairing).
                    let r = device_call(&conn, path, "Pair").await;
                    if r.is_ok() {
                        let _ = device_call(&conn, path, "Connect").await;
                    }
                    r
                }
            };
            if let Err(e) = result {
                warn!(?cmd, error = %e, "bluetooth: command failed");
            }
        });
    }
}

/// Collapse the ObjectManager reply to (state, adapter-path). Lists all *paired* devices, plus
/// *unpaired* ones while the adapter is discovering (so a scan surfaces nearby devices to pair).
fn derive(objs: &Managed) -> (BluetoothState, Option<String>) {
    let mut state = BluetoothState::default();
    let mut adapter = None;
    let mut devices = Vec::new();
    for (path, ifaces) in objs {
        if let Some(props) = ifaces.get(ADAPTER_IFACE) {
            state.present = true;
            state.powered = get_bool(props, "Powered").unwrap_or(false);
            state.discovering = get_bool(props, "Discovering").unwrap_or(false);
            adapter = Some(path.to_string());
        }
        if let Some(props) = ifaces.get(DEVICE_IFACE) {
            let name = get_str(props, "Alias")
                .or_else(|| get_str(props, "Name"))
                .unwrap_or_else(|| path.to_string());
            devices.push(BtDevice {
                path: path.to_string(),
                name,
                paired: get_bool(props, "Paired").unwrap_or(false),
                connected: get_bool(props, "Connected").unwrap_or(false),
            });
        }
    }
    // Show paired devices always; unpaired ones only while scanning. Order: connected, then paired,
    // then by name.
    devices.retain(|d| d.paired || state.discovering);
    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.paired.cmp(&a.paired))
            .then(a.name.cmp(&b.name))
    });
    state.devices = devices;
    (state, adapter)
}

/// Register the auto-accept pairing agent (best-effort — warn and carry on if BlueZ refuses).
async fn register_agent(conn: &Connection) {
    if let Err(e) = conn.object_server().at(AGENT_PATH, Agent).await {
        return warn!(error = %e, "bluetooth: could not export pairing agent");
    }
    let am = match Proxy::new(conn, BLUEZ, "/org/bluez", "org.bluez.AgentManager1").await {
        Ok(p) => p,
        Err(e) => return warn!(error = %e, "bluetooth: AgentManager1 proxy failed"),
    };
    let path = match ObjectPath::try_from(AGENT_PATH) {
        Ok(p) => p,
        Err(_) => return,
    };
    if let Err(e) = am.call::<_, _, ()>("RegisterAgent", &(&path, "NoInputNoOutput")).await {
        return warn!(error = %e, "bluetooth: RegisterAgent failed; pairing may not work");
    }
    let _ = am.call::<_, _, ()>("RequestDefaultAgent", &(&path,)).await;
}

async fn set_powered(conn: &Connection, adapter_path: &str, on: bool) -> zbus::Result<()> {
    let props = Proxy::new(conn, BLUEZ, adapter_path, "org.freedesktop.DBus.Properties").await?;
    props.call("Set", &(ADAPTER_IFACE, "Powered", Value::from(on))).await
}

async fn adapter_call(conn: &Connection, adapter_path: &str, method: &str) -> zbus::Result<()> {
    let ad = Proxy::new(conn, BLUEZ, adapter_path.to_string(), ADAPTER_IFACE).await?;
    ad.call(method, &()).await
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
