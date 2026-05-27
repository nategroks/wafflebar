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
use wafflebar_core::view::{ActionId, MenuItem};
use wafflebar_core::{Pixmap, TrayItem, TrayStatus};
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

// ---- DBusMenu client ------------------------------------------------------------------------

/// A DBusMenu layout node: `(id, properties, children-as-variants)` — D-Bus type `(ia{sv}av)`.
/// `children` is an array of *variants*, each wrapping another node, so it's parsed by hand.
type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

#[zbus::proxy(interface = "com.canonical.dbusmenu")]
trait DbusMenu {
    /// `(revision, layout)`. `parent_id` 0 = root; `depth` 1 = just its immediate children.
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: &[&str],
    ) -> zbus::Result<(u32, Layout)>;

    fn event(
        &self,
        id: i32,
        event_id: &str,
        data: &zbus::zvariant::Value<'_>,
        timestamp: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    fn layout_updated(&self, revision: u32, parent: i32) -> zbus::Result<()>;
}

const MENU_PROPS: &[&str] = &["type", "label", "enabled", "visible", "children-display", "toggle-type"];

/// Translate a DBusMenu top-level layout into flat [`MenuItem`]s. Submenus and toggles are
/// flattened in v1 (rendered as plain items); `*saw_*` flag the first time we drop that detail so
/// the caller can warn once — the field signal that "a real app needs nested menus" has arrived.
/// Unwrap one `av` element (a variant wrapping a `(ia{sv}av)` node) into its id + properties.
fn child_node(child: &OwnedValue) -> Option<(i32, HashMap<String, OwnedValue>)> {
    use zbus::zvariant::Value;
    let st = match &**child {
        Value::Value(inner) => match &**inner {
            Value::Structure(s) => s,
            _ => return None,
        },
        Value::Structure(s) => s,
        _ => return None,
    };
    let fields = st.fields();
    let id = i32::try_from(fields.first()?.try_clone().ok()?).ok()?;
    let props = HashMap::<String, OwnedValue>::try_from(fields.get(1)?.try_clone().ok()?).ok()?;
    Some((id, props))
}

fn parse_menu(layout: &Layout, key: &str, saw_submenu: &mut bool, saw_toggle: &mut bool) -> Vec<MenuItem> {
    let mut out = Vec::new();
    for child in &layout.2 {
        let Some((id, props)) = child_node(child) else {
            continue;
        };
        let get = |k: &str| props.get(k).and_then(|v| String::try_from(v.clone()).ok());
        let visible = props.get("visible").and_then(|v| bool::try_from(v.clone()).ok()).unwrap_or(true);
        if !visible {
            continue;
        }
        if get("type").as_deref() == Some("separator") {
            out.push(MenuItem::Separator);
            continue;
        }
        if get("children-display").as_deref() == Some("submenu") {
            *saw_submenu = true; // TODO(statustray): render submenus (MenuItem::Submenu) when needed
        }
        if props.contains_key("toggle-type") {
            *saw_toggle = true; // TODO(statustray): show toggle state (MenuItem::Toggle) when needed
        }
        let label = get("label").unwrap_or_default().replace('_', ""); // strip mnemonics
        out.push(MenuItem::Item {
            label,
            action: ActionId::new(format!("menu:{key}:{id}")),
        });
    }
    out
}

// ---- Icon pixmap (IconPixmap → Pixmap) ------------------------------------------------------

/// The bar's tray icon size; pixmap selection targets this. (Configurable cadence/size is Phase F.)
const TRAY_ICON_PX: u32 = 16;

/// One `IconPixmap` entry: `(width, height, ARGB32 bytes)` — D-Bus type `(iiay)`.
type RawPixmap = (i32, i32, Vec<u8>);

/// Pick the best pixmap for `target`px: the largest whose width ≤ target (downscaling beats
/// upscaling); if none fit, the smallest available. Skips malformed entries (non-positive dims or
/// `len != w*h*4`). Mirrors xfce4-panel's selection.
fn select_pixmap(pixmaps: &[RawPixmap], target: u32) -> Option<&RawPixmap> {
    let valid = pixmaps.iter().filter(|(w, h, b)| {
        *w > 0 && *h > 0 && b.len() == (*w as usize) * (*h as usize) * 4
    });
    // Largest width ≤ target.
    let best_fit = valid
        .clone()
        .filter(|(w, _, _)| *w as u32 <= target)
        .max_by_key(|(w, _, _)| *w);
    // Else smallest available.
    best_fit.or_else(|| valid.min_by_key(|(w, _, _)| *w))
}

/// SNI ships ARGB32 in network (big-endian) byte order → bytes are `[A,R,G,B]` per pixel. GdkPixbuf
/// wants RGBA, so rotate each 4-byte group left by one: `[A,R,G,B]` → `[R,G,B,A]`.
fn argb_to_rgba(argb: &[u8]) -> Vec<u8> {
    let mut out = argb.to_vec();
    for px in out.chunks_exact_mut(4) {
        px.rotate_left(1);
    }
    out
}

/// Parse `IconPixmap` (`a(iiay)`) → the best-size [`Pixmap`] (RGBA), or `None` if absent/all bad.
fn build_pixmap(value: &OwnedValue) -> Option<Pixmap> {
    let pixmaps: Vec<RawPixmap> = Vec::try_from(value.clone()).ok()?;
    let (w, h, argb) = select_pixmap(&pixmaps, TRAY_ICON_PX)?;
    Some(Pixmap {
        width: *w as u32,
        height: *h as u32,
        rgba: argb_to_rgba(argb),
    })
}

// ---- Per-item main-thread state -------------------------------------------------------------

struct ItemEntry {
    item: TrayItem,
    /// SNI proxy, kept for `Activate`.
    proxy: Proxy<'static>,
    /// DBusMenu proxy (the item's `Menu` path), kept for `Event` clicks. `None` until set up / if
    /// the item has no menu.
    menu_proxy: Option<DbusMenuProxy<'static>>,
    /// Last DBusMenu layout revision we fetched (re-fetch when a `LayoutUpdated` differs).
    menu_revision: u32,
    /// True once we've warned about flattening a submenu/toggle for this item (warn once).
    menu_degraded_warned: bool,
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

    /// `SecondaryActivate(0, 0)` (middle-click). Fire-and-forget.
    pub fn secondary_activate(&self, key: &str) {
        let proxy = match self.state.borrow().entries.get(key) {
            Some(e) => e.proxy.clone(),
            None => return,
        };
        glib::spawn_future_local(async move {
            if let Err(e) = proxy.call_method("SecondaryActivate", &(0_i32, 0_i32)).await {
                warn!(error = %e, "tray: SecondaryActivate failed");
            }
        });
    }

    /// `Scroll(delta, orientation)`. Fire-and-forget; errors swallowed (item may have gone zombie).
    pub fn scroll(&self, key: &str, delta: i32, horizontal: bool) {
        let proxy = match self.state.borrow().entries.get(key) {
            Some(e) => e.proxy.clone(),
            None => return,
        };
        let orientation = if horizontal { "horizontal" } else { "vertical" };
        glib::spawn_future_local(async move {
            if let Err(e) = proxy.call_method("Scroll", &(delta, orientation)).await {
                warn!(error = %e, "tray: Scroll failed");
            }
        });
    }

    /// `com.canonical.dbusmenu.Event(id, "clicked", …)` on the item's menu. Fire-and-forget; if the
    /// item's bus just vanished the call errors and is swallowed (zombie-prune removes it shortly).
    pub fn menu_click(&self, key: &str, id: i32) {
        let menu = match self.state.borrow().entries.get(key).and_then(|e| e.menu_proxy.clone()) {
            Some(p) => p,
            None => return,
        };
        glib::spawn_future_local(async move {
            let data = zbus::zvariant::Value::from(0_i32); // "clicked" carries no data
            if let Err(e) = menu.event(id, "clicked", &data, 0).await {
                warn!(error = %e, "tray: menu Event(clicked) failed");
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
                icon_pixmap: None,
                icon_theme_path: None,
                status: TrayStatus::Passive,
                menu: Vec::new(),
            },
            proxy,
            menu_proxy: None,
            menu_revision: 0,
            menu_degraded_warned: false,
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

/// One item's lifetime: SNI property fetch + (if it has a `Menu`) DBusMenu layout, then re-fetch on
/// the respective change signals — two independent streams, two simple loops.
async fn run_item(
    proxy: Proxy<'static>,
    key: String,
    state: Rc<RefCell<State>>,
    emit: Rc<dyn Fn(Vec<TrayItem>)>,
) {
    let menu_path = refresh(&proxy, &key, &state, &emit).await;

    // Set up the DBusMenu side if the item advertises a Menu, on the item's bus.
    if let Some(path) = menu_path {
        let built = DbusMenuProxy::builder(proxy.connection())
            .destination(proxy.destination().to_owned())
            .and_then(|b| b.path(path))
            .map(|b| b.build());
        if let Ok(fut) = built {
            if let Ok(menu) = fut.await {
                if let Some(e) = state.borrow_mut().entries.get_mut(&key) {
                    e.menu_proxy = Some(menu.clone());
                }
                refresh_menu(&menu, &key, &state, &emit).await;
                // Re-fetch layout when it changes (compare revisions: != not >, they can wrap).
                let (m, k, st, em) = (menu.clone(), key.clone(), state.clone(), emit.clone());
                glib::spawn_future_local(async move {
                    if let Ok(mut updates) = m.receive_layout_updated().await {
                        while let Some(sig) = updates.next().await {
                            let rev = sig.args().map(|a| a.revision).unwrap_or(0);
                            let cached = st.borrow().entries.get(&k).map(|e| e.menu_revision).unwrap_or(0);
                            if rev != cached {
                                refresh_menu(&m, &k, &st, &em).await;
                            }
                        }
                    }
                });
            }
        }
    }

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

/// `Properties.GetAll(org.kde.StatusNotifierItem)` → update the SNI-derived fields (preserving the
/// separately-fetched menu), publish on change. Returns the item's `Menu` object path if any.
async fn refresh(
    proxy: &Proxy<'static>,
    key: &str,
    state: &Rc<RefCell<State>>,
    emit: &Rc<dyn Fn(Vec<TrayItem>)>,
) -> Option<String> {
    let props: HashMap<String, OwnedValue> = match get_all(proxy).await {
        Ok(p) => p,
        Err(e) => {
            warn!(key, error = %e, "tray: GetAll failed (transient)"); // keep last state
            return None;
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
    // Menu is an object path ('o'); "/" is the no-menu sentinel.
    let menu_path = props
        .get("Menu")
        .and_then(|v| zbus::zvariant::OwnedObjectPath::try_from(v.clone()).ok())
        .map(|p| p.to_string())
        .filter(|p| p != "/" && !p.is_empty());

    let mut changed = false;
    {
        let mut s = state.borrow_mut();
        let Some(entry) = s.entries.get_mut(key) else {
            return None; // removed while we were fetching
        };
        // On NeedsAttention, prefer the attention icon (falling back to the normal one).
        let attn = status == TrayStatus::NeedsAttention;
        let icon_name = attn
            .then(|| get_str("AttentionIconName"))
            .flatten()
            .or_else(|| get_str("IconName"));
        let icon_pixmap = attn
            .then(|| props.get("AttentionIconPixmap").and_then(build_pixmap))
            .flatten()
            .or_else(|| props.get("IconPixmap").and_then(build_pixmap));
        let item = TrayItem {
            key: key.to_string(),
            id: get_str("Id").unwrap_or_default(),
            title: get_str("Title").unwrap_or_default(),
            icon_name,
            icon_pixmap,
            icon_theme_path: get_str("IconThemePath"),
            status,
            menu: entry.item.menu.clone(), // preserve the DBusMenu-fetched menu
        };
        if entry.item != item {
            entry.item = item.clone();
            upsert(&mut s.items, item);
            changed = true;
        }
    }
    if changed {
        publish(state, emit);
    }
    menu_path
}

/// `GetLayout(0, 1, …)` → translate the top-level layout into the item's menu; publish on change.
async fn refresh_menu(
    menu: &DbusMenuProxy<'static>,
    key: &str,
    state: &Rc<RefCell<State>>,
    emit: &Rc<dyn Fn(Vec<TrayItem>)>,
) {
    let (revision, layout) = match menu.get_layout(0, 1, MENU_PROPS).await {
        Ok(r) => r,
        Err(e) => {
            warn!(key, error = %e, "tray: GetLayout failed (item exposes Menu but no usable service)");
            return;
        }
    };

    let mut changed = false;
    {
        let mut s = state.borrow_mut();
        let Some(entry) = s.entries.get_mut(key) else {
            return;
        };
        let mut saw_submenu = false;
        let mut saw_toggle = false;
        let items = parse_menu(&layout, key, &mut saw_submenu, &mut saw_toggle);
        if (saw_submenu || saw_toggle) && !entry.menu_degraded_warned {
            entry.menu_degraded_warned = true;
            warn!(key, saw_submenu, saw_toggle, "tray: flattening DBusMenu (submenu/toggle not rendered in v1)");
        }
        entry.menu_revision = revision;
        if entry.item.menu != items {
            entry.item.menu = items;
            let updated = entry.item.clone();
            upsert(&mut s.items, updated);
            changed = true;
        }
    }
    if changed {
        publish(state, emit);
    }
}

/// Upsert by key into the ordered publish list.
fn upsert(items: &mut Vec<TrayItem>, item: TrayItem) {
    if let Some(existing) = items.iter_mut().find(|i| i.key == item.key) {
        *existing = item;
    } else {
        items.push(item);
    }
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
    fn pixmap_size_selection() {
        let px = |w: i32| (w, w, vec![0u8; (w * w * 4) as usize]);
        let set = [px(16), px(22), px(64)];
        // Largest that fits 24 → 22.
        assert_eq!(select_pixmap(&set, 24).unwrap().0, 22);
        // None ≤ 12 → smallest available → 16.
        assert_eq!(select_pixmap(&set, 12).unwrap().0, 16);
        // Exact fit.
        assert_eq!(select_pixmap(&set, 64).unwrap().0, 64);
    }

    #[test]
    fn pixmap_rejects_malformed_entries() {
        // Bytes don't match w*h*4, zero dims → skipped; the one valid entry is chosen.
        let set = [
            (2, 2, vec![0u8; 7]),         // truncated
            (0, 5, vec![0u8; 0]),         // zero width
            (2, 2, vec![1u8; 16]),        // valid (2*2*4)
        ];
        let chosen = select_pixmap(&set, 16).unwrap();
        assert_eq!((chosen.0, chosen.1), (2, 2));
        assert!(select_pixmap(&[(2, 2, vec![0u8; 7])], 16).is_none(), "all-malformed → None");
    }

    #[test]
    fn argb_to_rgba_swaps_byte_order() {
        // One pixel ARGB = [0xAA, 0x11, 0x22, 0x33] → RGBA [0x11,0x22,0x33,0xAA].
        assert_eq!(argb_to_rgba(&[0xAA, 0x11, 0x22, 0x33]), vec![0x11, 0x22, 0x33, 0xAA]);
        // Two pixels, independent rotation.
        assert_eq!(
            argb_to_rgba(&[1, 2, 3, 4, 5, 6, 7, 8]),
            vec![2, 3, 4, 1, 6, 7, 8, 5]
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
