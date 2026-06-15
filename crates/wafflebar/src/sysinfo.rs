//! Host-side system readouts for the control center (C-center): battery, brightness, CPU
//! temperature, audio-source (microphone) level, charge limit, and the CPU power cap.
//!
//! This is **host I/O**, not part of the serializable plugin boundary — so it lives in the binary
//! (like `menu.rs`), not in GTK-free `wafflebar-core`. The interesting *decisions* (parsing sysfs
//! strings, the percent/raw conversions, the `wpctl` output grammar, the `MM:SS` formatter) are
//! pulled out as pure functions and unit-tested in-crate; the readers/writers are a thin shell of
//! filesystem + `Command` I/O around them.
//!
//! **Degrade, never dummy.** Every reader returns `Option`/`None` when the backing file or tool is
//! absent (no battery, no backlight, no `intel-rapl`, no `wpctl`). The panel hides the corresponding
//! control rather than showing a dead one — the same "render nothing, not a dead button" discipline
//! the volume/bluetooth plugins use.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

// ─────────────────────────────────────────────────────────────────────────────
// Battery
// ─────────────────────────────────────────────────────────────────────────────

/// Charge state, normalized from the kernel's `status` string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChargeState {
    Charging,
    Discharging,
    Full,
    Unknown,
}

impl ChargeState {
    /// A freedesktop-ish glyph hint used by the header pill.
    pub fn is_charging(self) -> bool {
        matches!(self, ChargeState::Charging)
    }
}

/// A battery readout: charge percent, charge/discharge state, and instantaneous power in watts
/// (0.0 when the gauge doesn't report current/power).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Battery {
    pub percent: u8,
    pub state: ChargeState,
    pub power_w: f64,
}

/// Normalize the sysfs `status` string (`Charging`/`Discharging`/`Full`/`Not charging`/…).
pub fn parse_charge_state(s: &str) -> ChargeState {
    match s.trim().to_ascii_lowercase().as_str() {
        "charging" => ChargeState::Charging,
        "discharging" => ChargeState::Discharging,
        "full" => ChargeState::Full,
        _ => ChargeState::Unknown,
    }
}

/// Instantaneous power draw in watts from the sysfs gauges. Prefer `power_now` (µW); otherwise
/// derive it from `current_now` (µA) × `voltage_now` (µV) ÷ 1e12. `None` when neither is available
/// or parseable.
pub fn power_watts(
    power_now: Option<&str>,
    current_now: Option<&str>,
    voltage_now: Option<&str>,
) -> Option<f64> {
    let num = |o: Option<&str>| o.and_then(|s| s.trim().parse::<f64>().ok());
    if let Some(p_uw) = num(power_now) {
        return Some((p_uw / 1e6).abs());
    }
    match (num(current_now), num(voltage_now)) {
        (Some(i_ua), Some(v_uv)) => Some((i_ua * v_uv / 1e12).abs()),
        _ => None,
    }
}

/// Read the first `/sys/class/power_supply/BAT*` (or any supply of type `Battery`). `None` on a
/// desktop with no battery.
pub fn read_battery() -> Option<Battery> {
    let dir = battery_dir()?;
    let read = |f: &str| fs::read_to_string(dir.join(f)).ok();
    let percent = read("capacity")?.trim().parse::<f64>().ok()?.clamp(0.0, 100.0) as u8;
    let state = parse_charge_state(&read("status").unwrap_or_default());
    let power_w = power_watts(
        read("power_now").as_deref(),
        read("current_now").as_deref(),
        read("voltage_now").as_deref(),
    )
    .unwrap_or(0.0);
    Some(Battery { percent, state, power_w })
}

/// The first power-supply directory that is a battery.
fn battery_dir() -> Option<PathBuf> {
    let base = Path::new("/sys/class/power_supply");
    let mut entries: Vec<PathBuf> = fs::read_dir(base).ok()?.flatten().map(|e| e.path()).collect();
    entries.sort();
    entries.into_iter().find(|p| {
        fs::read_to_string(p.join("type"))
            .map(|t| t.trim() == "Battery")
            .unwrap_or(false)
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Backlight brightness
// ─────────────────────────────────────────────────────────────────────────────

/// Brightness as a 0..=100 percent of `max` (rounded). `max == 0` → 0 to avoid a divide-by-zero.
pub fn brightness_percent(cur: u64, max: u64) -> u8 {
    if max == 0 {
        return 0;
    }
    ((cur as u128 * 100 + (max as u128) / 2) / max as u128).min(100) as u8
}

/// The raw brightness value for a target percent against `max` (the inverse of
/// [`brightness_percent`]); clamped to `1..=max` so a slider never drives the panel fully dark.
pub fn brightness_raw(percent: u8, max: u64) -> u64 {
    let p = percent.min(100) as u128;
    ((p * max as u128 + 50) / 100).clamp(1, max as u128) as u64
}

/// The backlight device: its sysfs dir, current percent, and `max_brightness`. `None` when no
/// backlight exists (desktops, some external-only setups).
pub fn read_brightness() -> Option<(u8, PathBuf, u64)> {
    let dir = backlight_dir()?;
    let max = fs::read_to_string(dir.join("max_brightness")).ok()?.trim().parse::<u64>().ok()?;
    let cur = fs::read_to_string(dir.join("brightness")).ok()?.trim().parse::<u64>().ok()?;
    Some((brightness_percent(cur, max), dir, max))
}

fn backlight_dir() -> Option<PathBuf> {
    let base = Path::new("/sys/class/backlight");
    let mut entries: Vec<PathBuf> = fs::read_dir(base).ok()?.flatten().map(|e| e.path()).collect();
    entries.sort();
    entries.into_iter().next()
}

/// Set the backlight to `percent`. Prefers `brightnessctl` (it ships a udev rule / setuid path so an
/// unprivileged session can write), falling back to a direct sysfs write when the device is writable.
pub fn set_brightness(percent: u8) {
    if on_path("brightnessctl") {
        // `brightnessctl set N%` — clamps internally; no device arg = the default backlight.
        let _ = Command::new("brightnessctl").args(["set", &format!("{percent}%")]).spawn();
        return;
    }
    if let Some((_, dir, max)) = read_brightness() {
        let _ = fs::write(dir.join("brightness"), brightness_raw(percent, max).to_string());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CPU temperature
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a sysfs millidegree-Celsius temperature into °C. `46000` → `46.0`.
pub fn parse_millideg(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().map(|m| m / 1000.0)
}

/// A representative CPU/package temperature in °C, read from the thermal zones. Picks the hottest
/// `x86_pkg_temp`/`cpu`/`acpitz` zone, else the hottest zone overall. `None` when none exist.
pub fn read_cpu_temp() -> Option<f64> {
    let base = Path::new("/sys/class/thermal");
    let mut best: Option<f64> = None;
    let mut best_pref: Option<f64> = None;
    for entry in fs::read_dir(base).ok()?.flatten() {
        let p = entry.path();
        if !p.file_name().and_then(|n| n.to_str()).map(|n| n.starts_with("thermal_zone")).unwrap_or(false) {
            continue;
        }
        let Some(temp) = fs::read_to_string(p.join("temp")).ok().and_then(|s| parse_millideg(&s)) else {
            continue;
        };
        let kind = fs::read_to_string(p.join("type")).unwrap_or_default().to_ascii_lowercase();
        if kind.contains("pkg") || kind.contains("cpu") || kind.contains("acpitz") || kind.contains("coretemp") {
            best_pref = Some(best_pref.map_or(temp, |b: f64| b.max(temp)));
        }
        best = Some(best.map_or(temp, |b: f64| b.max(temp)));
    }
    best_pref.or(best)
}

// ─────────────────────────────────────────────────────────────────────────────
// Microphone (default audio source) via wpctl / pactl
// ─────────────────────────────────────────────────────────────────────────────

/// Parse `wpctl get-volume` output (`Volume: 0.65` or `Volume: 0.65 [MUTED]`) into `(percent, muted)`.
pub fn parse_wpctl_volume(out: &str) -> Option<(u8, bool)> {
    let rest = out.trim().strip_prefix("Volume:")?.trim();
    let muted = rest.contains("[MUTED]");
    let num = rest.split_whitespace().next()?;
    let frac = num.parse::<f64>().ok()?;
    Some(((frac * 100.0).round().clamp(0.0, 150.0).min(100.0) as u8, muted))
}

/// Read the default audio source level via `wpctl`. `None` when WirePlumber's `wpctl` isn't present.
pub fn read_mic() -> Option<(u8, bool)> {
    if !on_path("wpctl") {
        return None;
    }
    let out = Command::new("wpctl").args(["get-volume", "@DEFAULT_AUDIO_SOURCE@"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_wpctl_volume(&String::from_utf8_lossy(out.stdout.as_slice()))
}

/// Set the default audio source to `percent` via `wpctl`.
pub fn set_mic(percent: u8) {
    if on_path("wpctl") {
        let _ = Command::new("wpctl")
            .args(["set-volume", "@DEFAULT_AUDIO_SOURCE@", &format!("{}", percent as f64 / 100.0)])
            .spawn();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Battery charge limit (charge_control_end_threshold)
// ─────────────────────────────────────────────────────────────────────────────

/// The battery charge-stop threshold (`charge_control_end_threshold`, 0..=100) and the file backing
/// it. `None` on hardware/firmware without the control (most desktops, many laptops).
pub fn read_charge_limit() -> Option<(u8, PathBuf)> {
    let dir = battery_dir()?;
    let f = dir.join("charge_control_end_threshold");
    let v = fs::read_to_string(&f).ok()?.trim().parse::<u8>().ok()?;
    Some((v.min(100), f))
}

/// Set the charge-stop threshold (no-op if the control is absent or not writable).
pub fn set_charge_limit(percent: u8) {
    if let Some((_, f)) = read_charge_limit() {
        let _ = fs::write(f, percent.clamp(20, 100).to_string());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CPU power cap (intel-rapl powercap)
// ─────────────────────────────────────────────────────────────────────────────

/// The package power cap: `(current_w, max_w, file)` from `intel-rapl:0`'s long-term constraint.
/// `None` when RAPL isn't exposed (AMD without rapl, VMs, locked firmware).
pub fn read_power_limit() -> Option<(f64, f64, PathBuf)> {
    let dir = Path::new("/sys/class/powercap/intel-rapl:0");
    let cur_f = dir.join("constraint_0_power_limit_uw");
    let cur = fs::read_to_string(&cur_f).ok()?.trim().parse::<f64>().ok()? / 1e6;
    let max = fs::read_to_string(dir.join("constraint_0_max_power_uw"))
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|uw| uw / 1e6)
        .filter(|w| *w > 0.0)
        .unwrap_or((cur * 2.0).max(cur + 1.0));
    Some((cur, max, cur_f))
}

/// Set the package long-term power cap in watts (no-op if RAPL is absent / not writable).
pub fn set_power_limit(watts: f64) {
    if let Some((_, _, f)) = read_power_limit() {
        let uw = (watts.max(1.0) * 1e6).round() as u64;
        let _ = fs::write(f, uw.to_string());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Formatting + shared helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Format a duration as `MM:SS` (minutes uncapped, e.g. `125:09` for >2h). Used by the panel timers.
pub fn format_mmss(total_secs: u64) -> String {
    format!("{:02}:{:02}", total_secs / 60, total_secs % 60)
}

/// Whether `bin` resolves on `$PATH` (so optional tools like `wpctl`/`brightnessctl` only drive a
/// control when actually installed).
fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| dir.join(bin).is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_state_parsing_is_case_insensitive() {
        assert_eq!(parse_charge_state("Charging"), ChargeState::Charging);
        assert_eq!(parse_charge_state(" discharging\n"), ChargeState::Discharging);
        assert_eq!(parse_charge_state("Full"), ChargeState::Full);
        assert_eq!(parse_charge_state("Not charging"), ChargeState::Unknown);
        assert_eq!(parse_charge_state(""), ChargeState::Unknown);
    }

    #[test]
    fn power_prefers_power_now_then_derives_from_current_voltage() {
        // power_now is µW → W; sign is dropped (discharge gauges report negative).
        assert_eq!(power_watts(Some("17700000"), None, None), Some(17.7));
        assert_eq!(power_watts(Some("-17700000"), None, None), Some(17.7));
        // No power_now → current(µA) × voltage(µV) / 1e12.
        let w = power_watts(None, Some("1500000"), Some("11800000")).unwrap();
        assert!((w - 17.7).abs() < 0.01, "got {w}");
        // Neither present → None.
        assert_eq!(power_watts(None, None, Some("11800000")), None);
        assert_eq!(power_watts(None, None, None), None);
    }

    #[test]
    fn brightness_round_trips_through_percent_and_raw() {
        assert_eq!(brightness_percent(0, 0), 0); // no divide-by-zero
        assert_eq!(brightness_percent(96000, 96000), 100);
        assert_eq!(brightness_percent(48000, 96000), 50);
        // raw clamps to at least 1 so a 0% slider never blacks the panel out.
        assert_eq!(brightness_raw(0, 96000), 1);
        assert_eq!(brightness_raw(50, 96000), 48000);
        assert_eq!(brightness_raw(100, 96000), 96000);
    }

    #[test]
    fn millideg_parsing() {
        assert_eq!(parse_millideg("46000"), Some(46.0));
        assert_eq!(parse_millideg(" 52123\n"), Some(52.123));
        assert_eq!(parse_millideg("x"), None);
    }

    #[test]
    fn wpctl_volume_grammar() {
        assert_eq!(parse_wpctl_volume("Volume: 0.65"), Some((65, false)));
        assert_eq!(parse_wpctl_volume("Volume: 0.65 [MUTED]"), Some((65, true)));
        assert_eq!(parse_wpctl_volume("Volume: 1.00"), Some((100, false)));
        // Over-amplified sources clamp to 100 for the slider.
        assert_eq!(parse_wpctl_volume("Volume: 1.40"), Some((100, false)));
        assert_eq!(parse_wpctl_volume("garbage"), None);
    }

    #[test]
    fn mmss_formatting() {
        assert_eq!(format_mmss(0), "00:00");
        assert_eq!(format_mmss(4), "00:04");
        assert_eq!(format_mmss(1500), "25:00");
        assert_eq!(format_mmss(7509), "125:09"); // minutes are uncapped
    }
}
