//! Application launcher. STUB — design in docs/ARCHITECTURE.md "Launcher".
//!
//! Spec: freedesktop Desktop Entry Spec
//! <https://specifications.freedesktop.org/desktop-entry-spec/latest/> + Icon Theme Spec.
// TODO(M4): parse .desktop entries (categories, NoDisplay, TryExec); resolve icons via GTK
// IconTheme; track recently-used.

/// A resolved application entry.
pub struct AppEntry;

/// The launcher subsystem.
pub struct Launcher;
