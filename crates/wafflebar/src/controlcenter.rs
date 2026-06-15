//! Host-rendered **control center** panel (the quick-settings flyout): a header (clock/date,
//! battery, live power/temp/brightness stats), a stack of sliders (volume, mic, brightness, CPU
//! power cap, battery charge limit), expandable Bluetooth / Wi-Fi cards, two timers (a stopwatch
//! and a countdown), and a row of session actions.
//!
//! Like the apps menu and the volume mixer, this is **host infrastructure, not a reducer**: its
//! interactive state (slider drags, which card is expanded, the running timers) lives in these GTK
//! widgets, never in a plugin. The thin `controlcenter` plugin only contributes the bar button; the
//! host [`attach`]es this popover to that button's container — the same pattern as
//! [`Host::attach_volume_mixers`](crate::render::Host). Live values come from [`crate::sysinfo`]
//! (sysfs / `wpctl`) and the host's mirrored audio/Bluetooth/network state; commands go straight to
//! the host's sinks or are spawned.
//!
//! Controls whose backing mechanism is absent (no backlight, no `intel-rapl`, no `wpctl`, no charge
//! threshold) hide themselves rather than render dead — see `sysinfo`'s `Option` readers.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use chrono::Local;
use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, Button, GestureClick, Image, Label, Orientation, Popover, PositionType,
    Revealer, Scale,
};
use wafflebar_core::{BluetoothCommand, NetworkState, Position};

use crate::render::Host;
use crate::sysinfo;

/// Fixed panel width — sized to the mock; the height grows with its content.
const PANEL_W: i32 = 340;

/// A bundle of "re-read live state into the widgets" closures, run on open and on the periodic
/// refresh tick. Each control that mirrors system state contributes one.
type Refreshers = Rc<RefCell<Vec<Box<dyn Fn()>>>>;

/// Attach the control-center popover to a `controlcenter` bar-button container. Clicking the button
/// pops it up; GTK autohide closes it on click-out / Escape. A 2 s timer refreshes the live readouts
/// while it's visible. Mirrors `Host::attach_volume_mixer`.
pub fn attach(host: &Rc<Host>, container: &GtkBox) {
    let (body, refresh) = build(host);

    let popover = Popover::new();
    popover.set_child(Some(&body));
    popover.set_autohide(true);
    popover.add_css_class("controlcenter");
    popover.set_position(match host.position() {
        Position::Top => PositionType::Bottom,
        Position::Bottom => PositionType::Top,
    });
    popover.set_parent(container);

    {
        let refresh = refresh.clone();
        popover.connect_show(move |_| refresh());
    }
    // Keep the header (clock, battery, power, temp) live while open. The source self-cancels once
    // the panel widget is gone (structural reload drops the container → the popover → the body).
    {
        let popover_w = popover.downgrade();
        let refresh = refresh.clone();
        glib::timeout_add_seconds_local(2, move || match popover_w.upgrade() {
            Some(p) => {
                if p.is_visible() {
                    refresh();
                }
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
    }

    let gesture = GestureClick::new();
    gesture.set_button(gdk::BUTTON_PRIMARY);
    let pop = popover.clone();
    // Plain popup (not toggle): autohide already closes it on click-out, and a toggle would
    // race autohide (the button click counts as "outside") and immediately reopen. Same as the mixer.
    gesture.connect_released(move |_, _, _, _| pop.popup());
    container.add_controller(gesture);
}

/// Build the panel body and the master refresh closure. The body is a single vertical box; sections
/// append into it and register their own refreshers.
fn build(host: &Rc<Host>) -> (GtkBox, Rc<dyn Fn()>) {
    let body = GtkBox::new(Orientation::Vertical, 10);
    body.add_css_class("controlcenter");
    body.set_size_request(PANEL_W, -1);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);

    let refreshers: Refreshers = Rc::new(RefCell::new(Vec::new()));

    build_header(&body, &refreshers);
    build_sliders(&body, host, &refreshers);
    build_cards(&body, host, &refreshers);
    build_timers(&body);
    build_actions(&body);

    let refresh: Rc<dyn Fn()> = {
        let refreshers = refreshers.clone();
        Rc::new(move || {
            for r in refreshers.borrow().iter() {
                r();
            }
        })
    };
    (body, refresh)
}

// ─────────────────────────────────────────────────────────────────────────────
// Header: clock + date, battery pill, and a power/temp/brightness stat strip.
// ─────────────────────────────────────────────────────────────────────────────

fn build_header(body: &GtkBox, refreshers: &Refreshers) {
    let header = GtkBox::new(Orientation::Vertical, 4);
    header.add_css_class("cc-header");

    let top = GtkBox::new(Orientation::Horizontal, 8);
    let clock_col = GtkBox::new(Orientation::Vertical, 0);
    let time = Label::new(None);
    time.set_xalign(0.0);
    time.add_css_class("cc-time");
    let date = Label::new(None);
    date.set_xalign(0.0);
    date.add_css_class("cc-date");
    clock_col.append(&time);
    clock_col.append(&date);

    let battery = Label::new(None);
    battery.add_css_class("cc-battery");
    battery.set_valign(Align::Start);

    top.append(&clock_col);
    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    top.append(&spacer);
    top.append(&battery);
    header.append(&top);

    // Stat strip: ⚡ power · 🌡 temp · 🔆 brightness. Each hides when its source is absent.
    let stats = GtkBox::new(Orientation::Horizontal, 14);
    stats.add_css_class("cc-stats");
    let power = stat_label(&stats);
    let temp = stat_label(&stats);
    let light = stat_label(&stats);
    header.append(&stats);

    body.append(&header);

    refreshers.borrow_mut().push(Box::new(move || {
        let now = Local::now();
        time.set_text(&now.format("%H:%M").to_string());
        date.set_text(&now.format("%A, %B %-d").to_string());

        match sysinfo::read_battery() {
            Some(b) => {
                let bolt = if b.state.is_charging() { "\u{26a1} " } else { "" };
                battery.set_text(&format!("{bolt}{}%", b.percent));
                battery.set_visible(true);
                if b.power_w > 0.05 {
                    power.set_text(&format!("\u{26a1} {:.1}W", b.power_w));
                    power.set_visible(true);
                } else {
                    power.set_visible(false);
                }
            }
            None => {
                battery.set_visible(false);
                power.set_visible(false);
            }
        }
        match sysinfo::read_cpu_temp() {
            Some(t) => {
                temp.set_text(&format!("\u{1f321} {:.0}\u{b0}", t));
                temp.set_visible(true);
            }
            None => temp.set_visible(false),
        }
        match sysinfo::read_brightness() {
            Some((pct, _, _)) => {
                light.set_text(&format!("\u{1f506} {pct}%"));
                light.set_visible(true);
            }
            None => light.set_visible(false),
        }
    }));
}

fn stat_label(parent: &GtkBox) -> Label {
    let l = Label::new(None);
    l.add_css_class("cc-stat");
    parent.append(&l);
    l
}

// ─────────────────────────────────────────────────────────────────────────────
// Sliders: volume, mic, brightness, CPU power cap, battery charge limit.
// ─────────────────────────────────────────────────────────────────────────────

fn build_sliders(body: &GtkBox, host: &Rc<Host>, refreshers: &Refreshers) {
    // Volume → the host's audio sink (always shown; the panel's plugin subscribes to audio so the
    // backend is running). Seeds from the mirrored sink level.
    {
        let host = host.clone();
        add_pct_slider(
            body,
            refreshers,
            "audio-volume-high-symbolic",
            None,
            {
                let host = host.clone();
                Rc::new(move || host.volume_state().map(|(p, _)| p).or(Some(0)))
            },
            Rc::new(move |p| host.set_volume(p)),
        );
    }
    // Microphone (default source) via wpctl — hidden when wpctl is absent.
    add_pct_slider(
        body,
        refreshers,
        "audio-input-microphone-symbolic",
        None,
        Rc::new(|| sysinfo::read_mic().map(|(p, _)| p)),
        Rc::new(sysinfo::set_mic),
    );
    // Backlight brightness via brightnessctl/sysfs — hidden on machines with no backlight.
    add_pct_slider(
        body,
        refreshers,
        "display-brightness-symbolic",
        None,
        Rc::new(|| sysinfo::read_brightness().map(|(p, _, _)| p)),
        Rc::new(sysinfo::set_brightness),
    );
    // CPU package power cap (intel-rapl) — value shown in watts; hidden when RAPL is absent.
    add_power_slider(body, refreshers);
    // Battery charge limit (charge_control_end_threshold) — value shown in percent; hidden when the
    // firmware doesn't expose it.
    add_pct_slider(
        body,
        refreshers,
        "battery-level-100-charged-symbolic",
        Some(Unit::Percent),
        Rc::new(|| sysinfo::read_charge_limit().map(|(p, _)| p)),
        Rc::new(sysinfo::set_charge_limit),
    );
}

/// What, if anything, to print to the right of a slider.
#[derive(Clone, Copy)]
enum Unit {
    Percent,
}

/// A 0..=100 percent slider row: `[icon] [────●────] [value?]`. `get` returns the current percent or
/// `None` (row hidden); `set` applies a new percent. `unit` optionally prints a trailing value.
fn add_pct_slider(
    body: &GtkBox,
    refreshers: &Refreshers,
    icon: &str,
    unit: Option<Unit>,
    get: Rc<dyn Fn() -> Option<u8>>,
    set: Rc<dyn Fn(u8)>,
) {
    let (row, scale, value, updating) = slider_row(icon, unit.is_some());
    body.append(&row);

    scale.set_range(0.0, 100.0);
    scale.set_increments(5.0, 10.0);
    scale.connect_value_changed({
        let (updating, set) = (updating.clone(), set.clone());
        move |s| {
            if !updating.get() {
                set(s.value().round() as u8);
            }
        }
    });

    let refresh = move || match get() {
        Some(p) => {
            row.set_visible(true);
            updating.set(true);
            scale.set_value(p as f64);
            updating.set(false);
            if let Some(Unit::Percent) = unit {
                value.set_text(&format!("{p}%"));
            }
        }
        None => row.set_visible(false),
    };
    refresh();
    refreshers.borrow_mut().push(Box::new(refresh));
}

/// The CPU power-cap slider: range is 0..=max watts (read from RAPL), value printed as `NNW`.
fn add_power_slider(body: &GtkBox, refreshers: &Refreshers) {
    let (row, scale, value, updating) = slider_row("power-profile-performance-symbolic", true);
    body.append(&row);
    scale.set_increments(1.0, 5.0);

    scale.connect_value_changed({
        let updating = updating.clone();
        move |s| {
            if !updating.get() {
                sysinfo::set_power_limit(s.value().round());
            }
        }
    });

    let refresh = move || match sysinfo::read_power_limit() {
        Some((cur, max, _)) => {
            row.set_visible(true);
            updating.set(true);
            scale.set_range(0.0, max.max(cur).max(1.0));
            scale.set_value(cur);
            updating.set(false);
            value.set_text(&format!("{:.0}W", cur));
        }
        None => row.set_visible(false),
    };
    refresh();
    refreshers.borrow_mut().push(Box::new(refresh));
}

/// Build a slider row widget, returning the row plus its `Scale`, trailing value `Label`, and the
/// `updating` guard cell (set while seeding so a programmatic `set_value` doesn't echo to the
/// backend — the volume-mixer discipline).
fn slider_row(icon: &str, with_value: bool) -> (GtkBox, Scale, Label, Rc<Cell<bool>>) {
    let row = GtkBox::new(Orientation::Horizontal, 10);
    row.add_css_class("cc-slider");
    let img = Image::from_icon_name(icon);
    img.add_css_class("cc-slider-icon");
    img.set_pixel_size(18);
    let scale = Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 1.0);
    scale.set_hexpand(true);
    scale.set_draw_value(false);
    let value = Label::new(None);
    value.add_css_class("cc-value");
    row.append(&img);
    row.append(&scale);
    if with_value {
        row.append(&value);
    }
    (row, scale, value, Rc::new(Cell::new(false)))
}

// ─────────────────────────────────────────────────────────────────────────────
// Cards: Bluetooth + Wi-Fi, side by side, each with an expandable detail revealer.
// ─────────────────────────────────────────────────────────────────────────────

fn build_cards(body: &GtkBox, host: &Rc<Host>, refreshers: &Refreshers) {
    let grid = GtkBox::new(Orientation::Horizontal, 8);
    grid.set_homogeneous(true);
    grid.add_css_class("cc-pill-row");

    let (bt_pill, bt_label, bt_icon) = pill("bluetooth-symbolic", "Bluetooth");
    let (wifi_pill, wifi_label, wifi_icon) = pill("network-wireless-symbolic", "Wi-Fi");
    grid.append(&bt_pill);
    grid.append(&wifi_pill);
    body.append(&grid);

    // One full-width detail revealer per pill, stacked under the row.
    let bt_detail = GtkBox::new(Orientation::Vertical, 4);
    bt_detail.add_css_class("cc-detail");
    let bt_rev = revealer(&bt_detail);
    body.append(&bt_rev);

    let wifi_detail = GtkBox::new(Orientation::Vertical, 4);
    wifi_detail.add_css_class("cc-detail");
    let wifi_rev = revealer(&wifi_detail);
    body.append(&wifi_rev);

    // Bluetooth pill click → toggle its detail, populating from the mirrored state.
    on_click(&bt_pill, {
        let (host, bt_rev, bt_detail) = (host.clone(), bt_rev.clone(), bt_detail.clone());
        move || {
            let open = !bt_rev.reveals_child();
            if open {
                populate_bt(&bt_detail, &host);
            }
            bt_rev.set_reveal_child(open);
        }
    });
    on_click(&wifi_pill, {
        let (host, wifi_rev, wifi_detail) = (host.clone(), wifi_rev.clone(), wifi_detail.clone());
        move || {
            let open = !wifi_rev.reveals_child();
            if open {
                populate_wifi(&wifi_detail, &host);
            }
            wifi_rev.set_reveal_child(open);
        }
    });

    // Refresh: update the pill summaries, and re-populate an open detail so live changes show.
    let host_r = host.clone();
    refreshers.borrow_mut().push(Box::new(move || {
        let bt = host_r.bluetooth_state();
        if !bt.present {
            bt_pill.set_visible(false);
        } else {
            bt_pill.set_visible(true);
            let summary = if !bt.powered {
                "Off".to_string()
            } else if let Some(d) = bt.devices.iter().find(|d| d.connected) {
                d.name.clone()
            } else {
                "On".to_string()
            };
            bt_label.set_text(&summary);
            bt_icon.set_icon_name(Some(if bt.any_connected() {
                "bluetooth-active-symbolic"
            } else {
                "bluetooth-symbolic"
            }));
            if bt_rev.reveals_child() {
                populate_bt(&bt_detail, &host_r);
            }
        }

        match host_r.network_state() {
            Some(NetworkState::Wireless { strength, .. }) => {
                wifi_label.set_text(&format!("{strength}%"));
                wifi_icon.set_icon_name(Some("network-wireless-symbolic"));
            }
            Some(NetworkState::Wired { .. }) => {
                wifi_label.set_text("Wired");
                wifi_icon.set_icon_name(Some("network-wired-symbolic"));
            }
            Some(NetworkState::Disconnected) => {
                wifi_label.set_text("Off");
                wifi_icon.set_icon_name(Some("network-wireless-offline-symbolic"));
            }
            None => wifi_label.set_text("Wi-Fi"),
        }
        if wifi_rev.reveals_child() {
            populate_wifi(&wifi_detail, &host_r);
        }
    }));
}

/// Fill the Bluetooth detail: a power toggle then one button per device (connect/disconnect/pair).
fn populate_bt(detail: &GtkBox, host: &Rc<Host>) {
    clear(detail);
    let bt = host.bluetooth_state();
    let power = Button::with_label(if bt.powered { "Bluetooth: On" } else { "Bluetooth: Off" });
    power.add_css_class("cc-detail-item");
    power.connect_clicked({
        let host = host.clone();
        let powered = bt.powered;
        move |_| host.send_bluetooth(BluetoothCommand::SetPowered(!powered))
    });
    detail.append(&power);

    if bt.powered {
        if bt.devices.is_empty() {
            let l = Label::new(Some("No paired devices"));
            l.add_css_class("dim-label");
            l.set_xalign(0.0);
            detail.append(&l);
        }
        for d in &bt.devices {
            let mark = if d.connected { "\u{25cf} " } else { "\u{25cb} " };
            let batt = d.battery.map(|b| format!("  {b}%")).unwrap_or_default();
            let btn = Button::with_label(&format!("{mark}{}{batt}", d.name));
            btn.add_css_class("cc-detail-item");
            let (host, path, connected) = (host.clone(), d.path.clone(), d.connected);
            let paired = d.paired;
            btn.connect_clicked(move |_| {
                let cmd = if !paired {
                    BluetoothCommand::Pair(path.clone())
                } else if connected {
                    BluetoothCommand::Disconnect(path.clone())
                } else {
                    BluetoothCommand::Connect(path.clone())
                };
                host.send_bluetooth(cmd);
            });
            detail.append(&btn);
        }
    }
}

/// Fill the Wi-Fi detail: the current connection readout + a "Network settings…" launcher.
fn populate_wifi(detail: &GtkBox, host: &Rc<Host>) {
    clear(detail);
    let text = match host.network_state() {
        Some(NetworkState::Wireless { interface, strength, rx_bps, tx_bps }) => {
            format!("{interface} · {strength}%   \u{2193}{:.1} \u{2191}{:.1} Mbps", rx_bps as f64 / 1e6, tx_bps as f64 / 1e6)
        }
        Some(NetworkState::Wired { interface, rx_bps, tx_bps }) => {
            format!("{interface} (wired)   \u{2193}{:.1} \u{2191}{:.1} Mbps", rx_bps as f64 / 1e6, tx_bps as f64 / 1e6)
        }
        Some(NetworkState::Disconnected) => "Disconnected".to_string(),
        None => "NetworkManager unavailable".to_string(),
    };
    let l = Label::new(Some(&text));
    l.add_css_class("dim-label");
    l.set_xalign(0.0);
    l.set_wrap(true);
    detail.append(&l);

    let settings = Button::with_label("Network settings\u{2026}");
    settings.add_css_class("cc-detail-item");
    settings.connect_clicked(|_| spawn(&["nm-connection-editor"]));
    detail.append(&settings);
}

/// Build a pill (a card button): `[icon] [label] [›]`, returning the button plus its mutable label
/// and icon so the refresher can update the summary.
fn pill(icon: &str, label: &str) -> (Button, Label, Image) {
    let inner = GtkBox::new(Orientation::Horizontal, 8);
    let img = Image::from_icon_name(icon);
    img.add_css_class("cc-pill-icon");
    img.set_pixel_size(16);
    let lab = Label::new(Some(label));
    lab.add_css_class("cc-pill-label");
    lab.set_xalign(0.0);
    lab.set_hexpand(true);
    lab.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    let chevron = Label::new(Some("\u{203a}"));
    chevron.add_css_class("cc-chevron");
    inner.append(&img);
    inner.append(&lab);
    inner.append(&chevron);
    let btn = Button::new();
    btn.add_css_class("cc-pill");
    btn.set_child(Some(&inner));
    (btn, lab, img)
}

// ─────────────────────────────────────────────────────────────────────────────
// Timers: a count-up stopwatch and a count-down (Pomodoro-style) timer.
// ─────────────────────────────────────────────────────────────────────────────

/// One timer's mutable state, shared between its label, its controls, and the 1 Hz tick.
struct Timer {
    /// Seconds remaining (countdown) or elapsed (stopwatch).
    secs: Cell<u64>,
    running: Cell<bool>,
    /// `true` → counts down toward 0 (then stops); `false` → counts up.
    countdown: bool,
    /// The reset/initial value (countdown start, or 0 for the stopwatch).
    initial: Cell<u64>,
}

fn build_timers(body: &GtkBox) {
    let grid = GtkBox::new(Orientation::Horizontal, 8);
    grid.set_homogeneous(true);
    grid.add_css_class("cc-pill-row");

    let stopwatch = Rc::new(Timer {
        secs: Cell::new(0),
        running: Cell::new(false),
        countdown: false,
        initial: Cell::new(0),
    });
    let pomodoro = Rc::new(Timer {
        secs: Cell::new(25 * 60),
        running: Cell::new(false),
        countdown: true,
        initial: Cell::new(25 * 60),
    });

    let (sw_pill, sw_label, _) = pill("chronometer-symbolic", "");
    let (pm_pill, pm_label, _) = pill("alarm-symbolic", "");
    sw_label.set_text(&sysinfo::format_mmss(stopwatch.secs.get()));
    pm_label.set_text(&sysinfo::format_mmss(pomodoro.secs.get()));
    grid.append(&sw_pill);
    grid.append(&pm_pill);
    body.append(&grid);

    let sw_detail = GtkBox::new(Orientation::Horizontal, 6);
    sw_detail.add_css_class("cc-detail");
    let sw_rev = revealer(&sw_detail);
    body.append(&sw_rev);
    timer_controls(&sw_detail, &stopwatch, &sw_label, false);

    let pm_detail = GtkBox::new(Orientation::Horizontal, 6);
    pm_detail.add_css_class("cc-detail");
    let pm_rev = revealer(&pm_detail);
    body.append(&pm_rev);
    timer_controls(&pm_detail, &pomodoro, &pm_label, true);

    on_click(&sw_pill, {
        let sw_rev = sw_rev.clone();
        move || sw_rev.set_reveal_child(!sw_rev.reveals_child())
    });
    on_click(&pm_pill, {
        let pm_rev = pm_rev.clone();
        move || pm_rev.set_reveal_child(!pm_rev.reveals_child())
    });

    // A single 1 Hz tick advances both running timers and repaints their labels. Self-cancels when
    // the panel is gone (the labels' shared widget is dropped) — keyed off the stopwatch label's root.
    // Liveness is keyed on `body` (a weak ref), never on the labels: the closure holds strong label
    // clones, so a label-keyed guard would never fail (the timer + labels would leak across a
    // structural reload). `body` is held only by the popover/container, so its weak upgrade fails the
    // moment the panel is torn down, the source Breaks, and the closure's strong refs are released.
    let body_w = body.downgrade();
    let timers = [(stopwatch.clone(), sw_label.clone()), (pomodoro.clone(), pm_label.clone())];
    glib::timeout_add_seconds_local(1, move || {
        if body_w.upgrade().is_none() {
            return glib::ControlFlow::Break; // panel torn down (structural reload) → stop ticking
        }
        for (t, lbl) in &timers {
            if !t.running.get() {
                continue;
            }
            let next = if t.countdown {
                let left = t.secs.get().saturating_sub(1);
                if left == 0 {
                    t.running.set(false); // countdown reached zero → auto-pause
                }
                left
            } else {
                t.secs.get() + 1
            };
            t.secs.set(next);
            lbl.set_text(&sysinfo::format_mmss(next));
        }
        glib::ControlFlow::Continue
    });
}

/// Start/Pause + Reset (and ±1 min for the countdown) controls for a timer.
fn timer_controls(detail: &GtkBox, timer: &Rc<Timer>, label: &Label, adjustable: bool) {
    let toggle = Button::with_label("Start");
    toggle.add_css_class("cc-detail-item");
    toggle.connect_clicked({
        let (timer, toggle) = (timer.clone(), toggle.clone());
        move |_| {
            let running = !timer.running.get();
            timer.running.set(running);
            toggle.set_label(if running { "Pause" } else { "Start" });
        }
    });
    detail.append(&toggle);

    if adjustable {
        for (lbl, delta) in [("\u{2212}1m", -60i64), ("+1m", 60)] {
            let b = Button::with_label(lbl);
            b.add_css_class("cc-detail-item");
            let (timer, label) = (timer.clone(), label.clone());
            b.connect_clicked(move |_| {
                let next = (timer.secs.get() as i64 + delta).max(0) as u64;
                timer.secs.set(next);
                timer.initial.set(next);
                label.set_text(&sysinfo::format_mmss(next));
            });
            detail.append(&b);
        }
    }

    let reset = Button::with_label("Reset");
    reset.add_css_class("cc-detail-item");
    reset.connect_clicked({
        let (timer, label, toggle) = (timer.clone(), label.clone(), toggle.clone());
        move |_| {
            timer.running.set(false);
            timer.secs.set(timer.initial.get());
            label.set_text(&sysinfo::format_mmss(timer.initial.get()));
            toggle.set_label("Start");
        }
    });
    detail.append(&reset);
}

// ─────────────────────────────────────────────────────────────────────────────
// Session actions: lock / suspend / reboot / power off.
// ─────────────────────────────────────────────────────────────────────────────

fn build_actions(body: &GtkBox) {
    let row = GtkBox::new(Orientation::Horizontal, 8);
    row.set_homogeneous(true);
    row.add_css_class("cc-actions");
    // (icon, tooltip, argv). loginctl/systemctl cover the common logind session actions.
    let actions: &[(&str, &str, &[&str])] = &[
        ("system-lock-screen-symbolic", "Lock", &["loginctl", "lock-session"]),
        ("weather-clear-night-symbolic", "Suspend", &["systemctl", "suspend"]),
        ("view-refresh-symbolic", "Reboot", &["systemctl", "reboot"]),
        ("system-shutdown-symbolic", "Power off", &["systemctl", "poweroff"]),
    ];
    for (icon, tip, argv) in actions {
        let btn = Button::new();
        btn.add_css_class("cc-action");
        btn.set_tooltip_text(Some(tip));
        let img = Image::from_icon_name(icon);
        img.set_pixel_size(18);
        btn.set_child(Some(&img));
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        btn.connect_clicked(move |_| spawn(&argv.iter().map(String::as_str).collect::<Vec<_>>()));
        row.append(&btn);
    }
    body.append(&row);
}

// ─────────────────────────────────────────────────────────────────────────────
// Small shared helpers.
// ─────────────────────────────────────────────────────────────────────────────

/// A non-animated-collapse revealer (slide-down) wrapping `child`, initially hidden.
fn revealer(child: &impl IsA<gtk4::Widget>) -> Revealer {
    let r = Revealer::new();
    r.set_child(Some(child));
    r.set_transition_type(gtk4::RevealerTransitionType::SlideDown);
    r.set_reveal_child(false);
    r
}

/// Wire a primary-click handler onto a button (the pills/cards toggle their detail this way; a plain
/// `connect_clicked` would also work, but the cards aren't all `Button`s in future variants).
fn on_click<F: Fn() + 'static>(btn: &Button, f: F) {
    btn.connect_clicked(move |_| f());
}

/// Remove every child of a box (used to rebuild a detail revealer from fresh state).
fn clear(b: &GtkBox) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

/// Spawn a detached process (session actions, the network editor). Failures are logged, not fatal.
fn spawn(argv: &[&str]) {
    if let Some((cmd, args)) = argv.split_first() {
        if let Err(e) = std::process::Command::new(cmd).args(args).spawn() {
            tracing::debug!(?argv, error = %e, "controlcenter: spawn failed");
        }
    }
}
