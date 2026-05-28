//! Show-desktop plugin: one toggle button that asks the WM to show the desktop (minimize/restore
//! all windows). Mirrors xfce4-panel's `plugins/showdesktop/` — a single toggle button whose state
//! tracks the WM's show-desktop flag.
//!
//! The wrinkle: "show desktop" has no portable meaning on tiling WMs. dwl has no concept of it at
//! all; sway can script it; Hyprland has a special workspace. So the plugin keys off the backend's
//! [`WindowManager::supports_show_desktop`](wafflebar_core::WindowManager::supports_show_desktop):
//! when unsupported it renders [`View::Empty`] (the dwl user simply doesn't see a dead button), and
//! when a sway/Hyprland backend lands it lights up for free.

use wafflebar_core::{ActionId, Event, Plugin, Reaction, Topic, View, WmCommand};

use crate::plugins::Caps;

const ACTION_TOGGLE: &str = "toggle";

pub struct ShowDesktop {
    supported: bool,
    active: bool,
}

impl ShowDesktop {
    pub fn new(caps: &Caps) -> Self {
        Self {
            supported: caps.show_desktop,
            active: false,
        }
    }
}

impl Plugin for ShowDesktop {
    fn id(&self) -> &str {
        "showdesktop"
    }
    fn subscribe(&self) -> Vec<Topic> {
        Vec::new()
    }
    fn view(&self) -> View {
        if !self.supported {
            return View::Empty; // WM has no show-desktop (e.g. dwl) — show nothing, not a dead button
        }
        View::icon("wb-showdesktop-symbolic", 18)
            .button(ActionId::new(ACTION_TOGGLE))
            .with_class("wb-showdesktop")
            .with_class(if self.active { "active" } else { "inactive" })
    }
    fn on_event(&mut self, _ev: &Event) -> Reaction {
        Reaction::none()
    }
    fn on_action(&mut self, action: &ActionId) -> Reaction {
        // Guard on support too: when unsupported there's no button, so this is belt-and-suspenders.
        if !self.supported || action.0 != ACTION_TOGGLE {
            return Reaction::none();
        }
        self.active = !self.active; // optimistic; a real backend would confirm via an event
        Reaction {
            dirty: true,
            commands: vec![WmCommand::ToggleShowDesktop],
            ..Reaction::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_nothing_and_no_ops_when_unsupported() {
        // Backend reporting supports_show_desktop()=false (e.g. dwl).
        let mut plugin = ShowDesktop::new(&Caps { show_desktop: false });
        assert_eq!(plugin.view(), View::Empty);
        // No button is ever wired, but even a stray action must not emit a command.
        let r = plugin.on_action(&ActionId::new(ACTION_TOGGLE));
        assert_eq!(r, Reaction::none());
    }

    #[test]
    fn toggles_and_emits_command_when_supported() {
        let mut plugin = ShowDesktop::new(&Caps { show_desktop: true });
        assert!(matches!(plugin.view(), View::Button { .. }));
        let r = plugin.on_action(&ActionId::new(ACTION_TOGGLE));
        assert!(r.dirty);
        assert_eq!(r.commands, vec![WmCommand::ToggleShowDesktop]);
        assert!(plugin.active);
    }
}
