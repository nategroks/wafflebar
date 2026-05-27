//! hyprland backend (STUB). IPC: UNIX sockets at `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock` (event stream) and `.socket.sock` (commands).
#![allow(dead_code)] // stub: validated for trait shape, not constructed until its milestone.

use wafflebar_core::{WindowManager, WmCommand, WmEvent};

pub struct HyprlandBackend;

impl WindowManager for HyprlandBackend {
    fn snapshot(&self) -> Vec<WmEvent> {
        unimplemented!("hyprland backend not implemented")
    }
    fn execute(&mut self, _cmd: &WmCommand) {
        unimplemented!("hyprland backend not implemented")
    }
}
