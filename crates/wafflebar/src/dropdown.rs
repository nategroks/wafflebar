//! A floating layer-shell "dropdown" panel anchored under the bar, for UI that needs the keyboard
//! (the Settings panel, the applications menu's search).
//!
//! On dwl a `GtkPopover` is an xdg-popup of the bar's layer surface, and the compositor won't give
//! a layer-surface's popup keyboard focus — so popover text entry never works. A *standalone* layer
//! surface with `Exclusive` keyboard *does* get keys (the wofi/fuzzel pattern). This makes such a
//! window look like a dropdown from the bar (anchored to the top edge, below the bar's exclusive
//! zone) while actually receiving keyboard. Escape closes it.

use gtk4::prelude::*;
use gtk4::{EventControllerKey, Window};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

/// Turn `win` into a floating dropdown panel: an `Overlay` layer surface anchored under the bar,
/// with `Exclusive` keyboard (so entries can type on dwl) and Escape-to-close. Call before present.
pub fn panel(win: &Window) {
    win.init_layer_shell();
    win.set_layer(Layer::Overlay);
    win.set_anchor(Edge::Top, true); // below the bar (its exclusive zone reserves the top strip)
    win.set_margin(Edge::Top, 4);
    win.set_keyboard_mode(KeyboardMode::Exclusive);

    let key = EventControllerKey::new();
    let w = win.downgrade();
    key.connect_key_pressed(move |_, keyval, _, _| {
        if keyval == gtk4::gdk::Key::Escape {
            if let Some(w) = w.upgrade() {
                w.destroy();
            }
            gtk4::glib::Propagation::Stop
        } else {
            gtk4::glib::Propagation::Proceed
        }
    });
    win.add_controller(key);
}
