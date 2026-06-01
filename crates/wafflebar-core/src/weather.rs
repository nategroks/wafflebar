//! Weather state. GTK-free, serializable.

use serde::{Deserialize, Serialize};

/// One reading from the weather backend. Temperatures already converted to the user's units;
/// the icon name is derived from the upstream weather code so the plugin reducer doesn't have to
/// reimplement that mapping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeatherReading {
    /// Display temperature (already in the configured unit — `°F` or `°C`).
    pub temperature: f64,
    /// Symbol the plugin renders to a label suffix (e.g. `"°F"`).
    pub unit_suffix: String,
    /// Short human label for the current condition (`"clear"`, `"rain"`, `"snow"`, etc.).
    pub condition: String,
    /// Freedesktop symbolic icon name corresponding to the condition.
    pub icon: String,
}

/// Snapshot from the backend — `None` while the first fetch hasn't returned. The plugin renders
/// `View::Empty` until a successful fetch lands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WeatherState {
    pub reading: Option<WeatherReading>,
}
