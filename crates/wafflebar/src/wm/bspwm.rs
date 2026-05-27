//! bspwm backend (STUB). IPC: `bspc subscribe` for events and `bspc` for commands over `$BSPWM_SOCKET`.
#![allow(dead_code)] // stub: validated for trait shape, not constructed until its milestone.

use wafflebar_core::{WindowManager, WmCommand, WmEvent};

pub struct BspwmBackend;

impl WindowManager for BspwmBackend {
    fn snapshot(&self) -> Vec<WmEvent> {
        unimplemented!("bspwm backend not implemented")
    }
    fn execute(&mut self, _cmd: &WmCommand) {
        unimplemented!("bspwm backend not implemented")
    }
}
