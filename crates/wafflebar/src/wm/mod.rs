//! Window-manager backends behind the [`WindowManager`](wafflebar_core::WindowManager) seam.
//!
//! `dwl` is implemented (M2). The rest are stub files that name the IPC each will speak, so the
//! trait shape is validated against more than one mental model (Cairo-Dock GLDI lesson). They
//! compile but `unimplemented!()` at runtime.

pub mod dwl;

pub mod bspwm;
pub mod hyprland;
pub mod i3;
pub mod river;
pub mod sway;

pub use dwl::DwlBackend;
