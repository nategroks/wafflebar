//! Core library for **wafflebar** — a desktop panel for tiling window managers.
//!
//! This crate is deliberately **GTK-free** (a hard invariant — see `docs/ARCHITECTURE.md`) so
//! the whole module boundary is serializable and the eventual external-process isolation step is
//! a transport wrapper, not a rewrite. It owns:
//!
//! - [`config`] — the on-disk TOML schema (versioned) and its loader.
//! - [`grid`] — the grid layout engine (validates module placements on the `rows × columns` track).
//! - [`view`] — [`View`], the GTK-free UI *description* a module returns.
//! - [`wm`] — the [`WindowManager`] abstraction (Cairo-Dock GLDI model): events, commands, types.
//! - [`plugin`] — the [`Plugin`] reducer contract (events in → `View` + `Reaction` out).
//! - [`fake`] — a scriptable [`WindowManager`] for tests.
//!
//! The GTK4 front-end (`wafflebar` binary) is the *host*: it renders `View`s to widgets, owns the
//! widget tree, runs the GLib main loop, and provides the concrete compositor backends.

pub mod audio;
pub mod config;
pub mod cpu;
pub mod fake;
pub mod freedesktop;
pub mod grid;
pub mod memory;
pub mod net;
pub mod plugin;
pub mod reconcile;
pub mod schema;
pub mod tray;
pub mod view;
pub mod wm;

pub use config::{
    Align, BarConfig, Cell, Config, ConfigError, GridConfig, ModuleConfig, Position,
    SCHEMA_VERSION,
};
pub use audio::{VolumeCommand, VolumeEvent, VOLUME_NORM};
pub use cpu::CpuState;
pub use freedesktop::{application_dirs, list_applications, DesktopAction, DesktopApp, Launch};
pub use grid::{GridEngine, GridError, Placement};
pub use memory::MemoryState;
pub use net::NetworkState;
pub use plugin::{Event, Plugin, Reaction, Topic};
pub use reconcile::{diff_children, ChildPatch, ListPatch};
pub use schema::{ConfigField, FieldKind};
pub use tray::{TrayCommand, TrayItem, TrayStatus};
pub use view::{ActionId, MenuItem, Pixmap, SeparatorStyle, View};
pub use wm::{Output, Tag, TagState, Window, WindowId, WindowManager, WmCommand, WmEvent};
