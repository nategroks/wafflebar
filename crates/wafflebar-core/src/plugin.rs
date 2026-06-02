//! The `Plugin` contract.
//!
//! A module is a **pure reducer**: it owns some state, updates it from [`Event`]s it subscribed
//! to, renders that state to a [`View`], and turns user actions into a [`Reaction`] (declarative
//! commands the host applies). It never holds a GTK widget or a live WM handle — see the
//! v1→v2 isolation invariant in `docs/ARCHITECTURE.md`. This keeps the whole boundary
//! serializable and makes the eventual external-process step a transport wrapper, not a rewrite.

use crate::audio::{VolumeCommand, VolumeEvent};
use crate::bluetooth::{BluetoothCommand, BluetoothState};
use crate::config::ModuleConfig;
use crate::cpu::CpuState;
use crate::feed::FeedEvent;
use crate::freedesktop::Launch;
use crate::memory::MemoryState;
use crate::net::NetworkState;
use crate::schema::ConfigField;
use crate::tray::{TrayCommand, TrayItem};
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
    /// Audio (default-sink volume/mute) changes. The host starts the audio backend only if some
    /// plugin subscribes to this.
    Audio,
    /// Network (primary-connection) changes. The host starts the network backend only if some
    /// plugin subscribes to this.
    Network,
    /// Memory (RAM/swap) snapshots. The host starts the memory polling backend only if some plugin
    /// subscribes to this.
    Memory,
    /// CPU load deltas. The host starts the CPU polling backend only if some plugin subscribes.
    Cpu,
    /// StatusNotifier (system tray) item changes. The host starts the SNI backend only if some
    /// plugin subscribes to this.
    Tray,
    /// Bluetooth (adapter power + paired-device connection) changes. The host starts the BlueZ
    /// backend only if some plugin subscribes to this.
    Bluetooth,
    /// External block feed ([`FeedEvent`](crate::feed::FeedEvent)). The host binds the
    /// someblocks-style intake only if some plugin subscribes — natewm-mode wires this; vanilla
    /// configs leave it inert.
    Feed,
}

/// An event delivered to a subscribed module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    /// A window-manager state change.
    Wm(crate::wm::WmEvent),
    /// A timer tick for the given interval.
    Tick { secs: u32 },
    /// An audio state change from the host's audio backend.
    Volume(VolumeEvent),
    /// A network state change from the host's network backend.
    Network(NetworkState),
    /// A memory snapshot from the host's polling backend.
    Memory(MemoryState),
    /// A CPU load delta from the host's polling backend.
    Cpu(CpuState),
    /// The current tray item list from the host's SNI backend (full snapshot on any change).
    Tray(Vec<TrayItem>),
    /// The current Bluetooth state from the host's BlueZ backend (full snapshot on any change).
    Bluetooth(BluetoothState),
    /// A snapshot from the external block feed (someblocks-shaped). Always replaces — the producer
    /// owns ordering and timing, the bar renders the latest frame.
    Feed(FeedEvent),
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
    /// Desktop-app launch intents for the host executor to perform (DBus activation or Exec).
    /// Same discipline as `commands`: the plugin emits pure-data intents, the executor performs
    /// them — `zbus`/`Command` never appear in the plugin layer.
    pub launch: Vec<Launch>,
    /// Audio commands for the host's audio backend (e.g. toggle mute, adjust volume).
    pub volume: Vec<VolumeCommand>,
    /// Tray commands for the host's SNI backend (e.g. activate an item).
    pub tray: Vec<TrayCommand>,
    /// Bluetooth commands for the host's BlueZ backend (power toggle, connect/disconnect).
    pub bluetooth: Vec<BluetoothCommand>,
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
    /// Perform one desktop-app launch intent.
    pub fn launch(intent: Launch) -> Self {
        Self {
            launch: vec![intent],
            ..Self::default()
        }
    }
    /// Issue one audio command.
    pub fn volume(cmd: VolumeCommand) -> Self {
        Self {
            volume: vec![cmd],
            ..Self::default()
        }
    }
    /// Issue one tray command.
    pub fn tray(cmd: TrayCommand) -> Self {
        Self {
            tray: vec![cmd],
            ..Self::default()
        }
    }
    /// Issue one Bluetooth command.
    pub fn bluetooth(cmd: BluetoothCommand) -> Self {
        Self {
            bluetooth: vec![cmd],
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
    /// Re-read this module's config after a live edit (the host calls this only when the module's
    /// own config section changed). Re-derive internal state and return a [`Reaction`] (`dirty` to
    /// re-render). Default: no-op — for modules with no configurable options. Placement params
    /// (the bar output, capabilities) are construction-only and never arrive here.
    fn configure(&mut self, _cfg: &ModuleConfig) -> Reaction {
        Reaction::none()
    }
    /// Release any resources. Default: nothing (pure modules hold none).
    fn teardown(&mut self) {}
    /// Describe this module's configurable options for the preferences UI (F3). Returns field
    /// *descriptions* (key/label/kind/default), never current values — the host reads and writes
    /// values through the TOML, the single source of truth. Default: no configurable options.
    fn config_schema(&self) -> Vec<ConfigField> {
        Vec::new()
    }
}
