//! Open-Meteo polling backend. The host starts this once when any weather plugin subscribes; the
//! backend reads the first `[[modules]] type = "weather"` entry from the user's config.toml at
//! start to pick lat/lon/units. The plugin reducer stays pure (no IO); this backend handles both
//! the HTTP fetch and the config lookup.
//!
//! Synchronous HTTPS via `ureq` on a 10-min `glib::timeout_add_seconds`; deserialised with
//! `serde_json`. Response is ~1 KB and the request usually completes in <300 ms; if that ever
//! becomes a problem we'd move it to a worker thread + channel back to GLib.

use std::path::PathBuf;
use std::time::Duration;

use gtk4::glib;
use tracing::{debug, warn};
use wafflebar_core::{WeatherReading, WeatherState};

const POLL_SECS: u64 = 600;

pub trait WeatherBackend {
    fn start(self, emit: Box<dyn Fn(WeatherState)>) -> glib::SourceId;
}

pub struct OpenMeteoBackend;

impl WeatherBackend for OpenMeteoBackend {
    fn start(self, emit: Box<dyn Fn(WeatherState)>) -> glib::SourceId {
        let (zip, mut lat, mut lon, units) = read_config_or_default();
        // ZIP wins if present: a one-shot lookup at start time. We don't re-resolve on every poll
        // because ZIP geometry doesn't move.
        if !zip.is_empty() {
            match resolve_zip(&zip) {
                Some((zlat, zlon)) => {
                    debug!(zip, zlat, zlon, "weather: resolved ZIP");
                    lat = zlat;
                    lon = zlon;
                }
                None => warn!(zip, "weather: ZIP lookup failed; falling back to lat/lon fields"),
            }
        }
        debug!(lat, lon, units, "weather: starting backend");
        emit(fetch(lat, lon, &units));
        glib::timeout_add_seconds_local(POLL_SECS as u32, move || {
            emit(fetch(lat, lon, &units));
            glib::ControlFlow::Continue
        })
    }
}

/// US ZIP → (lat, lon) via api.zippopotam.us (free, key-less, ~1 KB JSON). One-shot at start.
fn resolve_zip(zip: &str) -> Option<(f64, f64)> {
    let url = format!("https://api.zippopotam.us/us/{zip}");
    let resp = ureq::get(&url).timeout(Duration::from_secs(8)).call().ok()?;
    let body: serde_json::Value = resp.into_json().ok()?;
    let place = body.get("places").and_then(|p| p.get(0))?;
    let lat = place.get("latitude").and_then(|v| v.as_str()).and_then(|s| s.parse().ok())?;
    let lon = place.get("longitude").and_then(|v| v.as_str()).and_then(|s| s.parse().ok())?;
    Some((lat, lon))
}

/// Locate the user's wafflebar config and pull the first weather module's zip/lat/lon/units.
/// Empty ZIP means "no ZIP override — use the lat/lon fields"; falls back across the board if the
/// file is missing, malformed, or has no weather entry.
fn read_config_or_default() -> (String, f64, f64, String) {
    let path = config_path();
    let body = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, path = %path.display(), "weather: cannot read config; using defaults");
            return (String::new(), 0.0, 0.0, "fahrenheit".into());
        }
    };
    let toml: toml::Value = match body.parse() {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "weather: bad config TOML; using defaults");
            return (String::new(), 0.0, 0.0, "fahrenheit".into());
        }
    };
    let modules = match toml.get("modules").and_then(|m| m.as_array()) {
        Some(a) => a,
        None => return (String::new(), 0.0, 0.0, "fahrenheit".into()),
    };
    for m in modules {
        let kind = m.get("type").and_then(|v| v.as_str());
        if kind != Some("weather") {
            continue;
        }
        // ZIP can be a string ("76065") or an integer (76065) in TOML — accept both.
        let zip = m.get("zip")
            .and_then(|v| v.as_str().map(|s| s.to_string())
                .or_else(|| v.as_integer().map(|i| i.to_string())))
            .unwrap_or_default();
        let lat = m.get("latitude")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .unwrap_or(0.0);
        let lon = m.get("longitude")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .unwrap_or(0.0);
        let units = m.get("units").and_then(|v| v.as_str()).unwrap_or("fahrenheit").to_string();
        return (zip, lat, lon, units);
    }
    (String::new(), 0.0, 0.0, "fahrenheit".into())
}

fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(p).join("wafflebar/config.toml");
    }
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h).join(".config/wafflebar/config.toml");
    }
    PathBuf::from("wafflebar/config.toml")
}

fn fetch(lat: f64, lon: f64, units: &str) -> WeatherState {
    let unit_param = match units {
        "celsius" => "celsius",
        _ => "fahrenheit",
    };
    let unit_suffix = if unit_param == "celsius" { "°C" } else { "°F" };
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}\
        &current=temperature_2m,weather_code&temperature_unit={unit_param}"
    );
    debug!(url, "weather: fetching");
    let resp = match ureq::get(&url).timeout(Duration::from_secs(8)).call() {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "weather: fetch failed");
            return WeatherState { reading: None };
        }
    };
    let body: serde_json::Value = match resp.into_json() {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "weather: bad JSON");
            return WeatherState { reading: None };
        }
    };
    let cur = &body["current"];
    let temp = cur["temperature_2m"].as_f64();
    let code = cur["weather_code"].as_u64();
    let (Some(temperature), Some(code)) = (temp, code) else {
        warn!(?cur, "weather: missing fields");
        return WeatherState { reading: None };
    };
    let (condition, icon) = code_to_label(code as u32);
    WeatherState {
        reading: Some(WeatherReading {
            temperature,
            unit_suffix: unit_suffix.to_string(),
            condition: condition.to_string(),
            icon: icon.to_string(),
        }),
    }
}

fn code_to_label(code: u32) -> (&'static str, &'static str) {
    match code {
        0          => ("clear",         "weather-clear-symbolic"),
        1 | 2      => ("partly cloudy", "weather-few-clouds-symbolic"),
        3          => ("overcast",      "weather-overcast-symbolic"),
        45 | 48    => ("fog",           "weather-fog-symbolic"),
        51..=55    => ("drizzle",       "weather-showers-scattered-symbolic"),
        56 | 57    => ("freezing rain", "weather-snow-symbolic"),
        61..=65    => ("rain",          "weather-showers-symbolic"),
        66 | 67    => ("freezing rain", "weather-snow-symbolic"),
        71..=75    => ("snow",          "weather-snow-symbolic"),
        77         => ("snow grains",   "weather-snow-symbolic"),
        80..=82    => ("rain showers",  "weather-showers-symbolic"),
        85 | 86    => ("snow showers",  "weather-snow-symbolic"),
        95         => ("thunderstorm",  "weather-storm-symbolic"),
        96 | 99    => ("hailstorm",     "weather-storm-symbolic"),
        _          => ("unknown",       "weather-clear-symbolic"),
    }
}
