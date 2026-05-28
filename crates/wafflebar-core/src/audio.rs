//! Audio types crossing the plugin boundary: the typed events a volume source emits and the
//! commands a volume plugin issues. GTK-free and serializable like the rest of the boundary.
//!
//! The host owns the actual audio backend (libpulse, on the GLib loop — see the binary's
//! `plugins/volume/backend.rs`); plugins only ever see these typed values. This mirrors the
//! `WmEvent`/`WmCommand` split exactly.

use serde::{Deserialize, Serialize};

/// wafflebar's canonical "100%" volume reference. Matches PulseAudio's `PA_VOLUME_NORM` (0x10000),
/// so the libpulse backend emits raw `pa_volume_t` values unchanged; a non-PA backend would scale
/// into this reference. The reducer derives the display percent from it.
pub const VOLUME_NORM: u32 = 0x10000;

/// A typed event from the audio backend. The backend emits raw magnitudes; the *reducer* derives
/// the displayed percent and compares that, so two events that differ only below display resolution
/// collapse to no re-render (see the volume plugin's reducer discipline).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VolumeEvent {
    /// The default sink's volume/mute changed (also emitted once on connect and on sink switch).
    /// `volume` is in [`VOLUME_NORM`] units (0 = muted scale, `VOLUME_NORM` = 100%).
    SinkVolumeChanged {
        sink_index: u32,
        volume: u32,
        muted: bool,
    },
    /// The full list of usable output sinks (re-emitted whenever the set or default changes), so the
    /// host can render an output picker. Separate from `SinkVolumeChanged` so plugins that only care
    /// about the level can keep ignoring it.
    Sinks(Vec<SinkInfo>),
    /// The backend is not usable (no server, connection failed/dropped). The plugin renders nothing.
    Unavailable,
}

/// A sink (audio output) for the picker UI. GTK-free, like the rest of the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SinkInfo {
    /// PulseAudio sink name (e.g. `alsa_output.pci-0000_00_1f.3.analog-stereo`) — used to address
    /// the sink when setting it as default. Stable across volume changes.
    pub name: String,
    /// Human-readable description (e.g. "Built-in Audio Analog Stereo").
    pub description: String,
    /// `true` if this is currently the default sink.
    pub is_default: bool,
}

/// A command a volume plugin issues; the host's audio backend performs it on the default sink.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VolumeCommand {
    /// Toggle mute on the default sink.
    ToggleMute,
    /// Change the default sink's volume by `delta` percentage points (clamped to 0..=100).
    Adjust { delta: i32 },
    /// Set the default sink's volume to an absolute `percent` (0..=100) — the mixer slider.
    SetVolume { percent: u8 },
    /// Switch the default sink to the one with this PulseAudio sink name — the output picker.
    SetDefaultSink(String),
}
