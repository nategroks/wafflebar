//! The `Plugin` contract.
//!
//! A module is a **pure reducer**: it owns some state, updates it from [`Event`]s it subscribed
//! to, renders that state to a [`View`], and turns user actions into a [`Reaction`] (declarative
//! commands the host applies). It never holds a GTK widget or a live WM handle — see the
//! v1→v2 isolation invariant in `docs/ARCHITECTURE.md`. This keeps the whole boundary
//! serializable and makes the eventual external-process step a transport wrapper, not a rewrite.

use crate::view::{ActionId, View};
use crate::wm::WmCommand;
use serde::{Deserialize, Serialize};

/// What a module wants to be woken by. The host wires each topic to a real source (the WM
/// backend's fd, a GLib timer, …) and never busy-polls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Topic {
    /// Window-manager state changes ([`WmEvent`](crate::wm::WmEvent)).
    Wm,
    /// A periodic tick every `secs` seconds.
    Timer { secs: u32 },
}

/// An event delivered to a subscribed module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    /// A window-manager state change.
    Wm(crate::wm::WmEvent),
    /// A timer tick for the given interval.
    Tick { secs: u32 },
}

/// A module's response to an event or action: whether its view changed, plus any side effects
/// the host should perform. Declarative so the boundary stays serializable.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reaction {
    /// The view changed; the host should re-render this module's subtree.
    pub dirty: bool,
    /// WM commands to execute (e.g. focus a tag, activate a window).
    pub commands: Vec<WmCommand>,
    /// Processes to spawn (argv), e.g. a launcher button. Host owns process spawning.
    pub spawn: Vec<Vec<String>>,
}

impl Reaction {
    /// Nothing happened.
    pub fn none() -> Self {
        Self::default()
    }
    /// The view changed; re-render.
    pub fn dirty() -> Self {
        Self {
            dirty: true,
            ..Self::default()
        }
    }
    /// Execute one WM command (does not by itself imply a re-render).
    pub fn command(cmd: WmCommand) -> Self {
        Self {
            commands: vec![cmd],
            ..Self::default()
        }
    }
    /// Spawn a process.
    pub fn spawn(argv: Vec<String>) -> Self {
        Self {
            spawn: vec![argv],
            ..Self::default()
        }
    }
}

/// A panel module. Object-safe so the host can hold `Box<dyn Plugin>`. Construction is done by
/// the host's module registry (not a trait method), so this trait stays object-safe and free of
/// constructor generics.
pub trait Plugin {
    /// Stable identifier (also used as a CSS class / log tag), e.g. `"clock"`.
    fn id(&self) -> &str;
    /// Which event topics should wake this module.
    fn subscribe(&self) -> Vec<Topic>;
    /// Render current state to a GTK-free [`View`].
    fn view(&self) -> View;
    /// React to a subscribed event (update internal state).
    fn on_event(&mut self, ev: &Event) -> Reaction;
    /// React to a user action routed back by [`ActionId`].
    fn on_action(&mut self, action: &ActionId) -> Reaction;
    /// Release any resources. Default: nothing (pure modules hold none).
    fn teardown(&mut self) {}
}
