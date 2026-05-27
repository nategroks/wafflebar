//! First-class shell subsystems (NOT modules — see `docs/ARCHITECTURE.md`).
//!
//! Stubbed in M2: each submodule carries its public type signatures, a spec link, and a
//! `TODO(M4)`. The full designs live in `docs/ARCHITECTURE.md` (the tray especially).
#![allow(dead_code)]

pub mod launcher;
pub mod notifications;
pub mod popups;
pub mod tray;
