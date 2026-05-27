//! Notifications. STUB — design in docs/ARCHITECTURE.md "Notifications".
//!
//! Spec: `org.freedesktop.Notifications`
//! <https://specifications.freedesktop.org/notification-spec/latest/>.
//! wafflebar becomes the notification daemon (only one may own the name — same not-fighting
//! discipline as the tray Watcher) + a notification-center popover.
// TODO(M4): implement the daemon over zbus.

/// A live notification.
pub struct Notification;

/// The notification subsystem (daemon + center).
pub struct Notifications;
