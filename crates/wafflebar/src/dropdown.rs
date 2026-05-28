//! A floating layer-shell "dropdown" panel anchored under the bar, for UI that needs the keyboard
//! (the Settings panel, the applications menu's search).
//!
//! On dwl a `GtkPopover` is an xdg-popup of the bar's layer surface, and the compositor won't give
//! a layer-surface's popup keyboard focus — so popover text entry never works. A *standalone* layer
//! surface with `Exclusive` keyboard *does* get keys (the wofi/fuzzel pattern). This makes such a
//! window look like a dropdown from the bar (anchored to the top-left, below the bar's exclusive
//! zone) while actually receiving keyboard. Escape or clicking the trigger again closes it.
//!
//! **Singleton, by necessity.** `Exclusive` keyboard is a hard grab on dwl: if two such surfaces are
//! ever mapped at once they stack grabs and the only recovery is logging out. The bar's triggers are
//! pointer events (the keyboard grab doesn't stop them), so a user clicking a launcher twice would
//! otherwise pile up grabbing windows. We therefore allow exactly one dropdown at a time: opening
//! one closes any other, and triggers toggle (see [`is_open`] / [`close_open`]).

use std::cell::RefCell;

use gtk4::glib::WeakRef;
use gtk4::prelude::*;
use gtk4::{EventControllerKey, PropagationPhase, Window};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

thread_local! {
    /// The currently-open dropdown, if any. `Weak` so a window closed by other means doesn't linger
    /// here. GTK is single-threaded, so a `thread_local` is the natural home.
    static OPEN: RefCell<Option<WeakRef<Window>>> = const { RefCell::new(None) };
}

/// Is a dropdown currently open? (Triggers use this to toggle.)
pub fn is_open() -> bool {
    OPEN.with(|o| o.borrow().as_ref().and_then(WeakRef::upgrade).is_some())
}

/// Close the open dropdown, if any. Idempotent; safe to call when nothing is open.
pub fn close_open() {
    // Take the registry slot and *drop the borrow* before destroying: `destroy()` synchronously
    // fires the window's `connect_destroy`, which re-borrows OPEN — holding the borrow across it
    // would panic with "already borrowed".
    let win = OPEN.with(|o| o.borrow_mut().take());
    if let Some(win) = win.and_then(|w| w.upgrade()) {
        win.destroy();
    }
}

/// Turn `win` into a floating dropdown panel: an `Overlay` layer surface anchored at the top-left
/// under the bar, with `Exclusive` keyboard (so entries can type on dwl) and Escape-to-close. Call
/// before `present`. Any previously-open dropdown is closed first — only one may exist at a time.
pub fn panel(win: &Window) {
    close_open(); // never let two Exclusive-keyboard surfaces coexist (see module docs)

    win.init_layer_shell();
    win.set_layer(Layer::Overlay);
    win.set_anchor(Edge::Top, true); // below the bar (its exclusive zone reserves the top strip)
    win.set_anchor(Edge::Left, true); // pin to the left under the launchers, not centered
    win.set_margin(Edge::Top, 4);
    win.set_margin(Edge::Left, 4);
    win.set_keyboard_mode(KeyboardMode::Exclusive);

    // Capture-phase so Escape always closes the dropdown, even when a focused child (e.g. a
    // SearchEntry, which would otherwise consume Escape to clear itself) has the keyboard.
    let key = EventControllerKey::new();
    key.set_propagation_phase(PropagationPhase::Capture);
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

    // Keep the registry honest no matter how the window dies (Escape, toggle, app launch, ✕).
    win.connect_destroy(|w| {
        OPEN.with(|o| {
            let mut slot = o.borrow_mut();
            if slot.as_ref().and_then(WeakRef::upgrade).as_ref() == Some(w) {
                *slot = None;
            }
        });
    });
    OPEN.with(|o| *o.borrow_mut() = Some(win.downgrade()));
}
