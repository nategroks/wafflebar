//! sway backend (STUB). IPC: sway-ipc(7) over `$SWAYSOCK` (wire-compatible with i3-ipc).
#![allow(dead_code)] // stub: validated for trait shape, not constructed until its milestone.

use wafflebar_core::{WindowManager, WmCommand, WmEvent};

pub struct SwayBackend;

impl WindowManager for SwayBackend {
    fn snapshot(&self) -> Vec<WmEvent> {
        unimplemented!("sway backend not implemented")
    }
    fn execute(&mut self, _cmd: &WmCommand) {
        unimplemented!("sway backend not implemented")
    }
}
