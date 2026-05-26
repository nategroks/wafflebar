//! Core library for **wafflebar** — a griddy Wayland status bar.
//!
//! This crate is deliberately **GTK-free** so it can be unit-tested in isolation and
//! reused by the future `wafflebar-config` GUI (Part B). It owns three things:
//!
//! - [`config`] — the on-disk TOML schema (versioned) and its loader.
//! - [`grid`] — the grid layout engine: validates module placements against the
//!   configured `rows × columns` track and detects out-of-bounds / overlapping cells.
//! - shared value types re-exported below.
//!
//! The GTK4 front-end (`wafflebar` binary) consumes [`grid::GridEngine`] to lay widgets
//! out on a `gtk::Grid`, and never re-implements placement logic itself.

pub mod config;
pub mod grid;

pub use config::{
    Align, BarConfig, Cell, Config, ConfigError, GridConfig, ModuleConfig, Position,
    SCHEMA_VERSION,
};
pub use grid::{GridEngine, GridError, Placement};
