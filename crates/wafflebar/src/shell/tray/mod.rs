//! System tray (StatusNotifierItem). STUB — full design in docs/ARCHITECTURE.md "Tray".
//!
//! Spec: <https://www.freedesktop.org/wiki/Specifications/StatusNotifierItem/>
//! wafflebar is both Watcher (`org.kde.StatusNotifierWatcher`) and Host. Subtleties already
//! settled on paper: Watcher name-ownership race, Host registration, item lifetime on app
//! crash, IconName-vs-IconPixmap resolution, KDE-vs-Ayatana drift, com.canonical.dbusmenu.
// TODO(M4): implement over zbus.

/// A tray item to render (icon + tooltip + menu handle). Populated from SNI in M4.
pub struct TrayItem;

/// The tray subsystem: owns the Watcher/Host D-Bus objects and the live item set.
pub struct Tray;
