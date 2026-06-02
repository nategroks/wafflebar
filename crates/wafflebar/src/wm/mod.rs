//! Window-manager backends behind the [`WindowManager`](wafflebar_core::WindowManager) seam.
//!
//! `dwl` (M2) and `sway`/`i3` (i3-ipc) are implemented. The four other backends (river, Hyprland, i3,
//! bspwm) had stub files since M2 that named each one's IPC, to *validate the trait shape* against
//! more than one mental model. That validation is complete — the data model (`Tag`/`Window`/
//! `WmCommand`) held against dwl's bitmask tags and (in the sway work) named workspaces — so the
//! stubs were deleted: they were scaffolding, removed *because* the trait validated, not because the
//! backends are abandoned. river/Hyprland/bspwm land as focused PRs against the now-validated trait;
//! i3 rides on the sway backend (shared i3-ipc protocol).

use std::cell::RefCell;
use std::os::fd::RawFd;
use std::rc::Rc;

use tracing::{debug, info, warn};
use wafflebar_core::{WindowManager, WmEvent};

pub mod dwl;
pub mod dwl_stdin;
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

/// How the host selects a backend. Default = autodetect (sway → dwl-ipc → stdin probe);
/// `force_dwl_stdin` short-circuits to the vanilla-dwl `-s` stdin backend for the natewm contract.
#[derive(Debug, Clone, Copy, Default)]
pub struct BackendSelect {
    /// Force the `dwl_stdin` backend even when other transports could connect. Set by the
    /// `--dwl-status-stdin` CLI flag. The stdin TTY guard inside [`dwl_stdin`] still has to pass.
    pub force_dwl_stdin: bool,
}

/// Connect to the running compositor.
///
/// Probe order:
/// 1. `--dwl-status-stdin` → [`dwl_stdin::DwlStdinBackend`] (vanilla dwl `-s` mode; no IPC patch
///    required, no `WmCommand` execution).
/// 2. sway/i3 IPC (`$SWAYSOCK`/`$I3SOCK`).
/// 3. dwl-ipc-unstable-v2 + foreign-toplevel ([`dwl::DwlBackend`] — requires the IPC patch).
///
/// `None` means no supported backend — the bar still runs, WM-driven modules just stay empty.
pub fn connect_backend(select: BackendSelect) -> Option<Rc<RefCell<dyn WmConnection>>> {
    if select.force_dwl_stdin {
        match dwl_stdin::DwlStdinBackend::connect(true) {
            Ok(b) => {
                info!("dwl_stdin backend connected (--dwl-status-stdin)");
                return Some(Rc::new(RefCell::new(b)) as Rc<RefCell<dyn WmConnection>>);
            }
            Err(e) => warn!(error = %e, "--dwl-status-stdin requested but stdin backend refused; falling through"),
        }
    }
    match sway::SwayBackend::connect() {
        Ok(b) => {
            info!("sway/i3 backend connected");
            return Some(Rc::new(RefCell::new(b)) as Rc<RefCell<dyn WmConnection>>);
        }
        Err(e) => debug!(error = %e, "no sway/i3 session"), // expected off sway/i3
    }
    match dwl::DwlBackend::connect() {
        Ok(b) => {
            info!("dwl backend connected");
            Some(Rc::new(RefCell::new(b)) as Rc<RefCell<dyn WmConnection>>)
        }
        Err(e) => {
            warn!(error = %e, "no compositor backend (dwl_stdin/sway/i3/dwl); tags/window will be empty");
            None
        }
    }
}
