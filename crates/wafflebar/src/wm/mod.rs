//! Window-manager backends behind the [`WindowManager`](wafflebar_core::WindowManager) seam.
//!
//! `dwl` is implemented (M2); `sway` is next (i3-ipc). The four other backends (river, Hyprland, i3,
//! bspwm) had stub files since M2 that named each one's IPC, to *validate the trait shape* against
//! more than one mental model. That validation is complete — the data model (`Tag`/`Window`/
//! `WmCommand`) held against dwl's bitmask tags and (in the sway work) named workspaces — so the
//! stubs were deleted: they were scaffolding, removed *because* the trait validated, not because the
//! backends are abandoned. river/Hyprland/bspwm land as focused PRs against the now-validated trait;
//! i3 rides on the sway backend (shared i3-ipc protocol).

use std::cell::RefCell;
use std::os::fd::RawFd;
use std::rc::Rc;

use tracing::{info, warn};
use wafflebar_core::{WindowManager, WmEvent};

pub mod dwl;
pub mod sway;

/// A live compositor connection: a [`WindowManager`] plus the event-loop plumbing the host drives
/// (an fd to watch, and a `dispatch` to drain on wake-up). This lives in the binary, not the core
/// trait, because it's a host I/O concern — `core`'s `WindowManager` is the GTK-free *data*
/// abstraction (which the test `FakeWm` implements without a socket).
pub trait WmConnection: WindowManager {
    /// The readable fd the host watches (the backend's compositor socket).
    fn fd(&self) -> RawFd;
    /// Drain the connection on an fd wake-up, returning accumulated events.
    fn dispatch(&mut self) -> Vec<WmEvent>;
}

/// Connect to the running compositor. Tries dwl today; the sway branch lands in PR-B. `None` means
/// no supported compositor — the bar still runs, WM-driven modules just stay empty.
pub fn connect_backend() -> Option<Rc<RefCell<dyn WmConnection>>> {
    match dwl::DwlBackend::connect() {
        Ok(b) => {
            info!("dwl backend connected");
            Some(Rc::new(RefCell::new(b)) as Rc<RefCell<dyn WmConnection>>)
        }
        Err(e) => {
            warn!(error = %e, "dwl backend unavailable; tags/window will be empty");
            None
        }
    }
}
