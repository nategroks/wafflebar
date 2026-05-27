//! i3 backend (STUB). IPC: i3-ipc over `$I3SOCK` (`i3 --get-socketpath`).
#![allow(dead_code)] // stub: validated for trait shape, not constructed until its milestone.

use wafflebar_core::{WindowManager, WmCommand, WmEvent};

pub struct I3Backend;

impl WindowManager for I3Backend {
    fn snapshot(&self) -> Vec<WmEvent> {
        unimplemented!("i3 backend not implemented")
    }
    fn execute(&mut self, _cmd: &WmCommand) {
        unimplemented!("i3 backend not implemented")
    }
}
