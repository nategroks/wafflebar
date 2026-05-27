//! StatusNotifierItem (SNI) types crossing the plugin boundary. GTK-free and serializable.
//!
//! The host owns the SNI backend (the Watcher + Host + per-item D-Bus state — see the binary's
//! `plugins/statustray/backend.rs`); the plugin sees only this derived list and emits typed
//! commands, exactly like the audio/network split. v1 (D2a) carries what an icon button needs;
//! menus (DBusMenu), pixmap icons, scroll, and attention/overlay icons land in D2b/D2c.

use serde::{Deserialize, Serialize};

/// SNI `Status`. `Passive` items are hidden (the spec's "available but not shown"); `Active` and
/// `NeedsAttention` are shown. (NeedsAttention also swaps to the attention icon — that's D2c.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrayStatus {
    Passive,
    Active,
    NeedsAttention,
}

impl TrayStatus {
    /// Whether the item is shown on the bar.
    pub fn visible(self) -> bool {
        !matches!(self, TrayStatus::Passive)
    }
}

/// One tray item, as the plugin renders it. `key` is the stable identity (`bus_name + object_path`,
/// the Watcher registration key) used for keyed reconciliation; `id` is the app's own `Id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayItem {
    pub key: String,
    pub id: String,
    pub title: String,
    /// Themed icon name (`IconName`). `None` → the renderer shows a placeholder (pixmap icons are
    /// D2c). Resolved through the GTK icon theme, honoring the item's `IconThemePath` if any.
    pub icon_name: Option<String>,
    pub status: TrayStatus,
}

/// A command the tray plugin issues; the host's SNI backend performs it on the item's D-Bus proxy.
/// v1 = left-click Activate only; SecondaryActivate / Scroll / menu events are D2c/D2b.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrayCommand {
    /// `org.kde.StatusNotifierItem.Activate(x, y)` on the item with this `key`.
    Activate { key: String },
}
