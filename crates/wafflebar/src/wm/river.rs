//! river backend (STUB). IPC: the `river-status-unstable-v1` (tags/layout/focus state) and `river-control-unstable-v1` (commands) Wayland protocols — not a socket.
#![allow(dead_code)] // stub: validated for trait shape, not constructed until its milestone.

use wafflebar_core::{WindowManager, WmCommand, WmEvent};

pub struct RiverBackend;

impl WindowManager for RiverBackend {
    fn snapshot(&self) -> Vec<WmEvent> {
        unimplemented!("river backend not implemented")
    }
    fn execute(&mut self, _cmd: &WmCommand) {
        unimplemented!("river backend not implemented")
    }
}
