//! StatusNotifierItem backend: the Watcher (server-side D-Bus interface), the Host (client proxy),
//! and per-item state. **All** D-Bus lives here; the plugin sees only typed `TrayItem`s/`TrayCommand`s.
//!
//! Architecture (mirrors xfce4-panel's sn-backend.c, bus-mediated so it composes with the GLib loop):
//!
//! - We **own** `org.kde.StatusNotifierWatcher` (with default flags — don't replace an existing
//!   one), exporting [`WatcherIface`]. If a Watcher already exists (e.g. Plasma), our request just
//!   queues and the export is dormant; we still function as a Host of theirs. So: become the
//!   Watcher if none exists, be a Host either way.
//! - The Watcher's `#[interface]` handlers run on zbus's executor (they must be `Send + Sync`), so
//!   they do the minimum: record the registration in an `Arc<Mutex<…>>` and emit `ItemRegistered`.
//!   Everything else — proxying the Watcher as a Host, the per-item futures, death-pruning — runs
//!   on the **main thread** via `glib::spawn_future_local`, coordinated entirely through bus
//!   signals. No manual cross-thread channel.
//! - **Zombie handling** (item crashes without unregistering): a main-thread task watches
//!   `NameOwnerChanged`; when a registered item's bus name vanishes it's pruned and
//!   `ItemUnregistered` emitted — the same external-lifetime discipline as M2's foreign-toplevel
//!   handles, tracking reality via bus watches rather than trusting items to unregister politely.
//!
//! KDE vs Ayatana: handled in [`parse_service`] — a service starting with `/` is Ayatana (the bus
//! is the D-Bus sender, the path is the service); otherwise KDE (bus = service, path =
//! `/StatusNotifierItem`). Both register on the same `org.kde.StatusNotifierWatcher`.
//!
//! D2a scope: items render from `IconName` + `Status` + `Title`; left-click → `Activate`. Pixmap
//! icons, attention/overlay, scroll, and DBusMenu menus are D2b/D2c.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gtk4::glib;
use tracing::{debug, warn};
use wafflebar_core::{TrayItem, TrayStatus};
use zbus::export::ordered_stream::OrderedStreamExt;
use zbus::fdo::DBusProxy;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;
use zbus::{Connection, Proxy};

const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const SNI_IFACE: &str = "org.kde.StatusNotifierItem";

/// Parse a `RegisterStatusNotifierItem` service string into (bus_name, object_path). A leading `/`
/// is the Ayatana form (object path; the bus is the caller's unique name); otherwise it's the KDE
/// form (the service is the bus name; the path is the conventional `/StatusNotifierItem`).
fn parse_service(service: &str, sender: &str) -> (String, String) {
    if service.starts_with('/') {
        (sender.to_string(), service.to_string())
    } else if let Some(slash) = service.find('/') {
        // Some senders pass "bus/path" combined.
        (service[..slash].to_string(), service[slash..].to_string())
    } else {
        (service.to_string(), "/StatusNotifierItem".to_string())
    }
}

// ---- Watcher (server-side interface) --------------------------------------------------------

/// Registered item keys, shared between the (zbus-thread) interface handlers and the (main-thread)
/// death-pruning task. The key is `bus_name + object_path`.
type Registry = Arc<Mutex<Vec<String>>>;

struct WatcherIface {
    items: Registry,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl WatcherIface {
    async fn register_status_notifier_item(
        &self,
        service: &str,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        let sender = hdr.sender().map(|s| s.to_string()).unwrap_or_default();
        let (bus, path) = parse_service(service, &sender);
        let key = format!("{bus}{path}");
        {
            let mut items = self.items.lock().unwrap();
            if items.contains(&key) {
                return;
            }
            items.push(key.clone());
        }
        debug!(key, "tray: item registered with watcher");
        let _ = WatcherIface::status_notifier_item_registered(&emitter, key).await;
    }

    async fn register_status_notifier_host(&self, _service: &str) {}

    #[zbus(property)]
    async fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.lock().unwrap().clone()
    }

    #[zbus(property)]
    async fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    async fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: String,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: String,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

// ---- Host proxy (client side) ---------------------------------------------------------------

#[zbus::proxy(
    interface = "org.kde.StatusNotifierWatcher",
    default_service = "org.kde.StatusNotifierWatcher",
    default_path = "/StatusNotifierWatcher"
)]
trait Watcher {
    fn register_status_notifier_host(&self, service: &str) -> zbus::Result<()>;
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> zbus::Result<Vec<String>>;
    #[zbus(signal)]
    fn status_notifier_item_registered(&self, service: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn status_notifier_item_unregistered(&self, service: String) -> zbus::Result<()>;
}

// ---- Per-item main-thread state -------------------------------------------------------------

struct ItemEntry {
    item: TrayItem,
    /// SNI proxy, kept for `Activate`.
    proxy: Proxy<'static>,
    /// The item's polling/signal future; aborted on unregister.
    task: glib::JoinHandle<()>,
}

#[derive(Default)]
struct State {
    items: Vec<TrayItem>,         // publish order
    entries: HashMap<String, ItemEntry>,
}

/// The backend handle held by the host (keeps the connection + state alive; performs commands).
pub struct SniBackend {
    state: Rc<RefCell<State>>,
}

impl Default for SniBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SniBackend {
    pub fn new() -> Self {
        Self { state: Rc::new(RefCell::new(State::default())) }
    }

    /// Start the Watcher + Host on the GLib main context. `emit` receives the full item list on any
    /// change. Called after the host exists so `emit` can hold a `Weak<Host>`.
    pub fn start(&self, emit: Rc<dyn Fn(Vec<TrayItem>)>) {
        glib::spawn_future_local(run(self.state.clone(), emit));
    }

    /// `org.kde.StatusNotifierItem.Activate(0, 0)` on the item. Coords are nominal — most apps
    /// ignore them (and Wayland has no global coordinates to give). Fire-and-forget.
    pub fn activate(&self, key: &str) {
        let proxy = match self.state.borrow().entries.get(key) {
            Some(e) => e.proxy.clone(),
            None => return,
        };
        glib::spawn_future_local(async move {
            if let Err(e) = proxy.call_method("Activate", &(0_i32, 0_i32)).await {
                warn!(error = %e, "tray: Activate failed");
            }
        });
    }
}

/// Own the Watcher name, export the interface, then run as a Host of whoever owns it.
async fn run(state: Rc<RefCell<State>>, emit: Rc<dyn Fn(Vec<TrayItem>)>) {
    let conn = match Connection::session().await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "tray: no session bus; statustray hidden");
            return;
        }
    };

    // Provide a Watcher if none exists (default flags: don't replace an existing owner).
    let registry: Registry = Arc::new(Mutex::new(Vec::new()));
    if let Err(e) = conn
        .object_server()
        .at(WATCHER_PATH, WatcherIface { items: registry.clone() })
        .await
    {
        warn!(error = %e, "tray: could not export StatusNotifierWatcher");
    }
    match conn.request_name_with_flags(WATCHER_NAME, Default::default()).await {
        Ok(reply) => debug!(?reply, "tray: requested StatusNotifierWatcher name"),
        Err(e) => warn!(error = %e, "tray: request_name failed"),
    }
    // Prune zombie items (bus vanished without unregistering) on the main thread.
    glib::spawn_future_local(prune_dead_items(conn.clone(), registry.clone()));

    // Be a Host of whoever owns the Watcher (us or an external one).
    let watcher = match WatcherProxy::new(&conn).await {
        Ok(w) => w,
        Err(e) => {
            warn!(error = %e, "tray: could not proxy the watcher");
            return;
        }
    };
    let _ = watcher.register_status_notifier_host(&conn.unique_name().map(|n| n.to_string()).unwrap_or_default()).await;

    // Initial set.
    if let Ok(items) = watcher.registered_status_notifier_items().await {
        for service in items {
            add_item(&conn, &state, &emit, &service).await;
        }
    }

    // React to registrations/unregistrations as two independent main-thread stream loops.
    {
        let (conn, state, emit) = (conn.clone(), state.clone(), emit.clone());
        let watcher = watcher.clone();
        glib::spawn_future_local(async move {
            let Ok(mut stream) = watcher.receive_status_notifier_item_registered().await else {
                return;
            };
            while let Some(sig) = stream.next().await {
                if let Ok(args) = sig.args() {
                    add_item(&conn, &state, &emit, &args.service).await;
                }
            }
        });
    }
    {
        let (state, emit) = (state.clone(), emit.clone());
        glib::spawn_future_local(async move {
            let Ok(mut stream) = watcher.receive_status_notifier_item_unregistered().await else {
                return;
            };
            while let Some(sig) = stream.next().await {
                if let Ok(args) = sig.args() {
                    remove_item(&state, &emit, &args.service);
                }
            }
        });
    }
}

/// Create the SNI proxy for `service`, do the first property fetch, and spawn its refresh future.
async fn add_item(
    conn: &Connection,
    state: &Rc<RefCell<State>>,
    emit: &Rc<dyn Fn(Vec<TrayItem>)>,
    service: &str,
) {
    if state.borrow().entries.contains_key(service) {
        return;
    }
    let (bus, path) = split_key(service);
    let proxy = match Proxy::new(conn, bus.clone(), path.clone(), SNI_IFACE).await {
        Ok(p) => p,
        Err(e) => {
            warn!(service, error = %e, "tray: could not proxy item");
            return;
        }
    };
    // Spawn the per-item refresh future (initial GetAll + re-fetch on any SNI signal).
    let task = glib::spawn_future_local(run_item(
        proxy.clone(),
        service.to_string(),
        state.clone(),
        emit.clone(),
    ));
    state.borrow_mut().entries.insert(
        service.to_string(),
        ItemEntry {
            item: TrayItem {
                key: service.to_string(),
                id: String::new(),
                title: String::new(),
                icon_name: None,
                status: TrayStatus::Passive,
            },
            proxy,
            task,
        },
    );
}

fn remove_item(state: &Rc<RefCell<State>>, emit: &Rc<dyn Fn(Vec<TrayItem>)>, service: &str) {
    let removed = {
        let mut s = state.borrow_mut();
        if let Some(entry) = s.entries.remove(service) {
            entry.task.abort();
            s.items.retain(|i| i.key != service);
            true
        } else {
            false
        }
    };
    if removed {
        publish(state, emit);
    }
}

/// One item's lifetime: initial property fetch, then re-fetch on any SNI change signal.
async fn run_item(
    proxy: Proxy<'static>,
    key: String,
    state: Rc<RefCell<State>>,
    emit: Rc<dyn Fn(Vec<TrayItem>)>,
) {
    refresh(&proxy, &key, &state, &emit).await;
    let mut signals = match proxy.receive_all_signals().await {
        Ok(s) => s,
        Err(e) => {
            warn!(key, error = %e, "tray: could not subscribe to item signals");
            return;
        }
    };
    while signals.next().await.is_some() {
        refresh(&proxy, &key, &state, &emit).await;
    }
}

/// `Properties.GetAll(org.kde.StatusNotifierItem)` → update this item's derived state, then publish.
async fn refresh(
    proxy: &Proxy<'static>,
    key: &str,
    state: &Rc<RefCell<State>>,
    emit: &Rc<dyn Fn(Vec<TrayItem>)>,
) {
    let props: HashMap<String, OwnedValue> = match get_all(proxy).await {
        Ok(p) => p,
        Err(e) => {
            warn!(key, error = %e, "tray: GetAll failed (transient)"); // keep last state
            return;
        }
    };

    let get_str = |k: &str| -> Option<String> {
        props.get(k).and_then(|v| String::try_from(v.clone()).ok()).filter(|s| !s.is_empty())
    };
    let status = match get_str("Status").as_deref() {
        Some("Active") => TrayStatus::Active,
        Some("NeedsAttention") => TrayStatus::NeedsAttention,
        _ => TrayStatus::Passive,
    };
    let item = TrayItem {
        key: key.to_string(),
        id: get_str("Id").unwrap_or_default(),
        title: get_str("Title").unwrap_or_default(),
        icon_name: get_str("IconName"),
        status,
    };

    {
        let mut s = state.borrow_mut();
        let Some(entry) = s.entries.get_mut(key) else {
            return; // removed while we were fetching
        };
        if entry.item == item {
            return; // no change — don't republish
        }
        entry.item = item.clone();
        // Upsert into the ordered publish list.
        if let Some(existing) = s.items.iter_mut().find(|i| i.key == key) {
            *existing = item;
        } else {
            s.items.push(item);
        }
    }
    publish(state, emit);
}

/// `org.freedesktop.DBus.Properties.GetAll(org.kde.StatusNotifierItem)` via the fdo helper.
async fn get_all(proxy: &Proxy<'static>) -> zbus::Result<HashMap<String, OwnedValue>> {
    let props = zbus::fdo::PropertiesProxy::builder(proxy.connection())
        .destination(proxy.destination().to_owned())?
        .path(proxy.path().to_owned())?
        .build()
        .await?;
    Ok(props.get_all(SNI_IFACE.try_into()?).await?)
}

fn publish(state: &Rc<RefCell<State>>, emit: &Rc<dyn Fn(Vec<TrayItem>)>) {
    let snapshot = state.borrow().items.clone();
    emit(snapshot);
}

/// `bus_name + object_path` → (bus, path). The path is the trailing `/…` component.
fn split_key(key: &str) -> (String, String) {
    match key.find('/') {
        Some(i) => (key[..i].to_string(), key[i..].to_string()),
        None => (key.to_string(), "/StatusNotifierItem".to_string()),
    }
}

/// Watch `NameOwnerChanged`; when a registered item's bus name loses its owner, prune it from the
/// Watcher registry and emit `ItemUnregistered` (so our Host — and any external host — drops it).
async fn prune_dead_items(conn: Connection, registry: Registry) {
    let dbus = match DBusProxy::new(&conn).await {
        Ok(d) => d,
        Err(e) => {
            warn!(error = %e, "tray: could not watch NameOwnerChanged");
            return;
        }
    };
    let mut changes = match dbus.receive_name_owner_changed().await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "tray: NameOwnerChanged subscription failed");
            return;
        }
    };
    while let Some(sig) = changes.next().await {
        let Ok(args) = sig.args() else { continue };
        if args.new_owner.is_some() {
            continue; // appeared/changed, not vanished
        }
        let gone = args.name.to_string();
        let vanished: Vec<String> = {
            let mut items = registry.lock().unwrap();
            let (dead, alive): (Vec<String>, Vec<String>) =
                items.drain(..).partition(|k| k.starts_with(&gone));
            *items = alive;
            dead
        };
        for key in vanished {
            debug!(key, "tray: item bus vanished; unregistering");
            if let Ok(iface) = conn
                .object_server()
                .interface::<_, WatcherIface>(WATCHER_PATH)
                .await
            {
                let _ = WatcherIface::status_notifier_item_unregistered(iface.signal_emitter(), key)
                    .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_service_kde_and_ayatana() {
        // KDE: a bus name → conventional /StatusNotifierItem path.
        assert_eq!(
            parse_service("org.kde.StatusNotifierItem-1234-1", ":1.50"),
            ("org.kde.StatusNotifierItem-1234-1".to_string(), "/StatusNotifierItem".to_string())
        );
        // Ayatana: a leading-/ path → bus is the caller's sender.
        assert_eq!(
            parse_service("/org/ayatana/NotificationItem/nm_applet", ":1.77"),
            (":1.77".to_string(), "/org/ayatana/NotificationItem/nm_applet".to_string())
        );
    }

    #[test]
    fn split_key_roundtrips() {
        assert_eq!(
            split_key("org.kde.StatusNotifierItem-1/StatusNotifierItem"),
            ("org.kde.StatusNotifierItem-1".to_string(), "/StatusNotifierItem".to_string())
        );
    }
}
