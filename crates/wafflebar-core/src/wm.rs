//! The `WindowManager` abstraction — wafflebar's universality seam.
//!
//! Modules never talk to dwl (or river/sway/…) directly; they consume the GTK-free types here.
//! A backend (e.g. `src/wm/dwl.rs` in the binary) implements [`WindowManager`], translating its
//! compositor's native IPC into [`WmEvent`]s and executing [`WmCommand`]s. This is the
//! Cairo-Dock GLDI model (see `docs/PRIOR_ART.md`): one core, swappable backends.
//!
//! Everything here is serializable so it can cross the module boundary (and, later, a process
//! boundary — see `docs/ARCHITECTURE.md`).

use serde::{Deserialize, Serialize};

/// Opaque, stable-per-session window identifier (e.g. a foreign-toplevel handle's id).
pub type WindowId = u64;

/// A connected output (monitor).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    /// Connector name as the compositor reports it (e.g. `DP-1`).
    pub name: String,
    /// Layout x-position (used for left/center/right selection; survives connector renaming).
    pub x: i32,
}

/// Per-tag state, mirroring dwl's `tag_state` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TagState {
    /// No clients, not selected.
    None,
    /// Selected / viewed.
    Active,
    /// Contains an urgent client.
    Urgent,
}

/// A workspace/tag on a given output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    /// Zero-based tag index.
    pub index: u32,
    /// Display name (dwl uses "1".."9").
    pub name: String,
    pub state: TagState,
    /// This tag holds the focused client.
    pub focused: bool,
    /// This tag has at least one client.
    pub occupied: bool,
}

/// A top-level window (from `wlr-foreign-toplevel-management`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub id: WindowId,
    pub title: String,
    pub app_id: String,
    pub focused: bool,
    pub minimized: bool,
}

/// State changes a backend publishes onto the event bus. A backend emits a full snapshot of the
/// relevant slice (e.g. *all* tags for an output) so modules are pure reducers over these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WmEvent {
    /// The full tag list for an output changed.
    Tags { output: String, tags: Vec<Tag> },
    /// The tiling layout symbol for an output changed (e.g. `[]=`).
    Layout { output: String, symbol: String },
    /// The focused window's title/app_id on an output changed.
    ActiveWindow {
        output: String,
        title: String,
        app_id: String,
    },
    /// The global window list changed (foreign-toplevel; used by the taskbar in M2 PR2).
    Windows { windows: Vec<Window> },
}

/// A command a module asks the WM to perform (the result of a click/scroll). Declarative +
/// serializable: the host applies it to the active backend. Modules hold no live WM handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WmCommand {
    /// View `tag` on `output` (dwl-ipc `set_tags`).
    FocusTag { output: String, tag: u32 },
    /// Select tiling layout `index` on `output` (dwl-ipc `set_layout`).
    SetLayout { output: String, index: u32 },
    /// Focus/raise a window (foreign-toplevel `activate`).
    ActivateWindow(WindowId),
    /// Close a window (foreign-toplevel `close`).
    CloseWindow(WindowId),
    /// Minimize/unminimize a window (foreign-toplevel `set_minimized`).
    SetMinimized(WindowId, bool),
}

/// Implemented by each compositor backend. The host drives event delivery via the backend's fd
/// (see `docs/ARCHITECTURE.md`); this trait covers the initial snapshot and command execution.
pub trait WindowManager {
    /// Current state expressed as the events a freshly-subscribed module would need to render.
    fn snapshot(&self) -> Vec<WmEvent>;
    /// Perform a command (the declarative result of a module action).
    fn execute(&mut self, cmd: &WmCommand);
}
