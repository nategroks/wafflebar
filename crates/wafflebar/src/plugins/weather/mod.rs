//! Weather plugin: temperature + condition from Open-Meteo. Reducer is tiny — the backend pre-cooks
//! the temperature into the configured unit and chooses the freedesktop icon, so the plugin just
//! renders. Config carries lat/lon + units (`celsius`|`fahrenheit`); the *first* placed weather
//! plugin's config drives the backend (multiple weather modules with different coords is rare and
//! intentionally not supported in v1).

pub mod backend;

use wafflebar_core::{
    ActionId, ConfigField, Event, FieldKind, ModuleConfig, Plugin, Reaction, Topic, View,
    WeatherReading, WeatherState,
};

pub struct Weather {
    display: Option<Display>,
}

#[derive(Clone, PartialEq)]
struct Display {
    icon: String,
    label: String,
}

impl Weather {
    pub fn new() -> Self { Self { display: None } }
}

impl Default for Weather {
    fn default() -> Self { Self::new() }
}

fn derive(r: &WeatherReading) -> Display {
    Display {
        icon: r.icon.clone(),
        label: format!("{:.0}{} {}", r.temperature, r.unit_suffix, r.condition),
    }
}

impl Plugin for Weather {
    fn id(&self) -> &str { "weather" }

    fn subscribe(&self) -> Vec<Topic> { vec![Topic::Weather] }

    fn view(&self) -> View {
        let Some(d) = &self.display else { return View::Empty; };
        View::row(
            vec![
                View::icon(d.icon.clone(), 16).with_class("weather-icon"),
                View::label(d.label.clone()).with_class("weather-label"),
            ],
            4,
        )
        .with_class("module")
        .with_class("weather")
    }

    fn on_event(&mut self, ev: &Event) -> Reaction {
        let Event::Weather(state) = ev else { return Reaction::none(); };
        let next = state.reading.as_ref().map(derive);
        if self.display == next { return Reaction::none(); }
        self.display = next;
        Reaction::dirty()
    }

    fn on_action(&mut self, _a: &ActionId) -> Reaction { Reaction::none() }

    fn config_schema(&self) -> Vec<ConfigField> {
        vec![
            // ZIP is the friendliest entry point: if set, the backend resolves it to lat/lon at
            // startup via api.zippopotam.us (US-only, no key). Leave empty to use the lat/lon fields.
            ConfigField {
                key: "zip".into(),
                label: "ZIP code (US) — overrides lat/lon when set".into(),
                kind: FieldKind::Text { default: "".into() },
            },
            ConfigField {
                key: "latitude".into(),
                label: "Latitude (decimal degrees) — used when ZIP is empty".into(),
                kind: FieldKind::Text { default: "0".into() },
            },
            ConfigField {
                key: "longitude".into(),
                label: "Longitude (decimal degrees) — used when ZIP is empty".into(),
                kind: FieldKind::Text { default: "0".into() },
            },
            ConfigField::choice(
                "units",
                "Temperature units",
                &[("fahrenheit", "Fahrenheit (°F)"), ("celsius", "Celsius (°C)")],
                "fahrenheit",
            ),
        ]
    }
}

/// Read the first weather plugin's lat/lon/units from a slot list — the host uses this at backend
/// start time. Public so `app.rs` can call it without depending on the reducer's internals.
pub(crate) fn read_config(cfg: &ModuleConfig) -> (f64, f64, String) {
    let lat = cfg.opt_str("latitude").and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let lon = cfg.opt_str("longitude").and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let units = cfg.opt_str("units").unwrap_or("fahrenheit").to_string();
    (lat, lon, units)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(t: f64, cond: &str) -> WeatherReading {
        WeatherReading {
            temperature: t,
            unit_suffix: "°F".into(),
            condition: cond.into(),
            icon: "weather-clear-symbolic".into(),
        }
    }

    #[test]
    fn empty_state_renders_nothing() {
        let mut w = Weather::new();
        assert!(!w.on_event(&Event::Weather(WeatherState::default())).dirty);
        assert_eq!(w.view(), View::Empty);
    }

    #[test]
    fn reading_renders_temperature_and_condition() {
        let mut w = Weather::new();
        let s = WeatherState { reading: Some(r(72.4, "clear")) };
        assert!(w.on_event(&Event::Weather(s)).dirty);
        match w.view() {
            View::Row { children, .. } => {
                assert!(matches!(&children[1], View::Label { text, .. } if text == "72°F clear"));
            }
            _ => panic!("expected row"),
        }
    }
}
