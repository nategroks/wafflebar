//! Popups (calendar/volume/network flyouts). STUB — design in docs/ARCHITECTURE.md.
//!
//! Strategy: GTK `Popover` anchored to a bar widget; gtk4-layer-shell drives the `xdg_popup`
//! on the layer surface. The `View::Popover` node already exists in the contract, so modules
//! add content here, not plumbing.
// TODO(M3/M4): render `View::Popover` content into a gtk::Popover on the trigger widget.

/// A popover the host can present from a module's trigger widget.
pub struct Popup;
