//! Notification server (G1a): owns `org.freedesktop.Notifications` and turns `Notify` calls into
//! [`Notification`]s delivered to the host (popups in G1b).
//!
//! This is the **SNI Watcher pattern** (D2) reused — own the bus name, expose a minimal `Send+Sync`
//! `#[interface]`, deliver to the main thread. The one difference: the spec has no "notification
//! posted" signal to ride (SNI rode `ItemRegistered`), so the iface→main bridge is an async channel.
//! The only signals we *emit* are `NotificationClosed` and `ActionInvoked` (the latter wired to popup
//! actions in G1b).
//!
//! In-process, not a separate daemon: notifications are a surface the bar process owns, like the tray
//! and menu. One daemon per session — if another already owns the name we don't fight it (unless
//! `--replace-notifications`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use gtk4::glib;
use tracing::{debug, warn};
use wafflebar_core::{CloseReason, Notification, Timeout, Urgency};
use zbus::fdo::RequestNameFlags;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;
use zbus::Connection;

const SPEC_VERSION: &str = "1.2";
const PATH: &str = "/org/freedesktop/Notifications";
const NAME: &str = "org.freedesktop.Notifications";

/// What the server hands to the host UI (popups, G1b; logged in G1a).
pub enum ServerOp {
    Post(Notification),
    Close(u32),
}

/// The `#[interface]` state — shared with zbus's executor, so everything here is `Send + Sync`
/// (`AtomicU32` + an async-channel `Sender`). No GTK, no `Rc`.
struct NotificationsIface {
    next_id: Arc<AtomicU32>,
    tx: async_channel::Sender<ServerOp>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl NotificationsIface {
    #[allow(clippy::too_many_arguments)] // the spec's Notify signature
    async fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        // `replaces_id != 0` updates that notification in place; otherwise mint a fresh non-zero id.
        let id = if replaces_id != 0 {
            replaces_id
        } else {
            self.next_id.fetch_add(1, Ordering::Relaxed)
        };
        let urgency = hints
            .get("urgency")
            .and_then(|v| u8::try_from(v).ok())
            .map(Urgency::from_hint)
            .unwrap_or_default();
        // actions arrive flat: [key, label, key, label, …]. Odd trailing entries are dropped.
        let actions = actions.chunks_exact(2).map(|c| (c[0].clone(), c[1].clone())).collect();
        let notification = Notification {
            id,
            app_name,
            app_icon,
            summary,
            body,
            actions,
            urgency,
            timeout: Timeout::from_spec(expire_timeout),
        };
        let _ = self.tx.send(ServerOp::Post(notification)).await;
        id
    }

    async fn close_notification(
        &self,
        id: u32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        let _ = self.tx.send(ServerOp::Close(id)).await;
        let _ = NotificationsIface::notification_closed(&emitter, id, CloseReason::Closed.code()).await;
    }

    /// Advertise only what we render — apps read this to degrade gracefully. (No `persistence`,
    /// `sound`, `actions-icons`, or `body-images` in v1.)
    async fn get_capabilities(&self) -> Vec<String> {
        ["body", "actions", "icon-static", "body-markup"].iter().map(|s| s.to_string()).collect()
    }

    async fn get_server_information(&self) -> (String, String, String, String) {
        ("wafflebar".into(), "wafflebar".into(), env!("CARGO_PKG_VERSION").into(), SPEC_VERSION.into())
    }

    #[zbus(signal)]
    async fn notification_closed(emitter: &SignalEmitter<'_>, id: u32, reason: u32) -> zbus::Result<()>;

    /// Emitted when a popup action is clicked (wired in G1b).
    #[zbus(signal)]
    async fn action_invoked(emitter: &SignalEmitter<'_>, id: u32, action_key: String) -> zbus::Result<()>;
}

/// Start the notification server on the GLib main context. `replace` takes the name from an existing
/// daemon (`--replace-notifications`); otherwise we don't fight one. `deliver` receives each op on
/// the main thread.
pub fn start(replace: bool, deliver: impl Fn(ServerOp) + 'static) {
    glib::spawn_future_local(run(replace, deliver));
}

async fn run(replace: bool, deliver: impl Fn(ServerOp) + 'static) {
    let conn = match Connection::session().await {
        Ok(c) => c,
        Err(e) => return warn!(error = %e, "notifications: no session bus; disabled"),
    };

    // The iface keeps the `Sender`; the channel is the only path off zbus's executor thread.
    let (tx, rx) = async_channel::unbounded::<ServerOp>();
    let iface = NotificationsIface { next_id: Arc::new(AtomicU32::new(1)), tx };
    if let Err(e) = conn.object_server().at(PATH, iface).await {
        return warn!(error = %e, "notifications: could not export interface; disabled");
    }

    // Don't-queue so we learn immediately if another daemon owns the name; replace takes it over.
    let flags = if replace {
        RequestNameFlags::ReplaceExisting | RequestNameFlags::AllowReplacement
    } else {
        RequestNameFlags::DoNotQueue.into()
    };
    match conn.request_name_with_flags(NAME, flags).await {
        Ok(_) => debug!("notifications: acquired {NAME}"),
        Err(e) => {
            warn!(error = %e, "notifications: another daemon owns {NAME}; wafflebar notifications disabled (stop it, or pass --replace-notifications)");
            return;
        }
    }

    // Drain ops to the host on the main thread for the process lifetime.
    while let Ok(op) = rx.recv().await {
        deliver(op);
    }
}
