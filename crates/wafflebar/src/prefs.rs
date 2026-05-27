//! Preferences UI (F3). A host-rendered settings window: a master list of the bar plus each placed
//! module on the left, and on the right a form built from the selected target's `config_schema()`.
//!
//! The plugins never own dialogs (the GTK-free invariant) — the host renders every form from the
//! declarative schema. Editing a field **writes the value back to `config.toml`** (via `toml_edit`,
//! preserving comments/order) and stops there: the existing file-watch reload (F2/F2b) picks the
//! change up and applies it through `configure()` or a structural rebuild. The window is just
//! another writer of the TOML, on the *same* code path as a hand edit — no separate apply pipeline.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, DropDown, Entry, EventControllerFocus, Label, ListBox,
    ListBoxRow, Orientation, ScrolledWindow, SpinButton, Switch,
};
use tracing::warn;
use wafflebar_core::{Config, ConfigField, FieldKind, Position};

use crate::plugins;

/// What a form is editing: the `[bar]` table, or the i-th `[[modules]]` entry.
#[derive(Clone, Copy)]
enum Target {
    Bar,
    Module(usize),
}

/// Open the preferences window over the running bar. Re-reads the config from disk so the form
/// reflects the current file (including any hand edits since launch).
pub fn open(app: &Application, config_path: &Path) {
    let config = match Config::load(config_path) {
        Ok(c) => Rc::new(c),
        Err(e) => {
            warn!(error = %e, "preferences: config no longer loads; not opening");
            return;
        }
    };
    let path = Rc::new(config_path.to_path_buf());

    let window = ApplicationWindow::builder()
        .application(app)
        .title("wafflebar settings")
        .default_width(560)
        .default_height(420)
        .build();

    // Left: the target list (Bar + each module). Right: the form for the selected target.
    let split = gtk4::Box::new(Orientation::Horizontal, 0);
    let list = ListBox::new();
    list.set_width_request(180);
    let form = gtk4::Box::new(Orientation::Vertical, 8);
    form.set_margin_top(12);
    form.set_margin_bottom(12);
    form.set_margin_start(12);
    form.set_margin_end(12);

    list.append(&list_row("Bar  ·  position, size, theme"));
    for m in &config.modules {
        list.append(&list_row(&format!("{}  ·  cell ({}, {})", m.kind, m.cell.row, m.cell.col)));
    }

    list.connect_row_selected({
        let (form, config, path) = (form.clone(), config.clone(), path.clone());
        move |_, row| {
            let target = match row.map(|r| r.index()) {
                Some(0) => Target::Bar,
                Some(i) => Target::Module((i - 1) as usize),
                None => return,
            };
            build_form(&form, target, &config, &path);
        }
    });

    let list_scroll = ScrolledWindow::builder().child(&list).build();
    let form_scroll = ScrolledWindow::builder().child(&form).hexpand(true).build();
    split.append(&list_scroll);
    split.append(&gtk4::Separator::new(Orientation::Vertical));
    split.append(&form_scroll);
    window.set_child(Some(&split));

    // Select the Bar row so the window never opens to an empty form.
    list.select_row(list.row_at_index(0).as_ref());
    window.present();
}

fn list_row(text: &str) -> ListBoxRow {
    let row = ListBoxRow::new();
    let label = Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_margin_top(8);
    label.set_margin_bottom(8);
    label.set_margin_start(10);
    label.set_margin_end(10);
    row.set_child(Some(&label));
    row
}

/// Rebuild the form pane for `target` from its schema, populating each field's current value.
fn build_form(form: &gtk4::Box, target: Target, config: &Rc<Config>, path: &Rc<PathBuf>) {
    while let Some(child) = form.first_child() {
        form.remove(&child);
    }

    let fields = match target {
        Target::Bar => bar_config_schema(),
        Target::Module(i) => {
            config.modules.get(i).map(|m| plugins::config_schema(&m.kind)).unwrap_or_default()
        }
    };

    if fields.is_empty() {
        let none = Label::new(Some("This module has no configurable options."));
        none.set_xalign(0.0);
        none.add_css_class("dim-label");
        form.append(&none);
        return;
    }

    for field in &fields {
        form.append(&field_row(field, target, config, path));
    }

    // Every `[bar]` property is fixed at window creation (anchors, exclusive zone, monitor, CSS), and
    // the F2b reload rebuilds only the grid — so bar edits persist but apply on restart. Surface that
    // honestly rather than hiding a path that only half-works.
    if matches!(target, Target::Bar) {
        let note = Label::new(Some("Bar settings apply after restarting wafflebar."));
        note.set_xalign(0.0);
        note.set_margin_top(8);
        note.add_css_class("dim-label");
        form.append(&note);
    }
}

/// One label-plus-control row, with the control populated from the current TOML value and its change
/// handler wired to write the value back.
fn field_row(field: &ConfigField, target: Target, config: &Rc<Config>, path: &Rc<PathBuf>) -> gtk4::Box {
    let row = gtk4::Box::new(Orientation::Horizontal, 12);
    let label = Label::new(Some(&field.label));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    row.append(&label);

    match &field.kind {
        FieldKind::Text { default } => {
            let entry = Entry::new();
            entry.set_text(&current_str(config, target, &field.key).unwrap_or_else(|| default.clone()));
            // Commit on Enter and on focus-leave (not per keystroke — that would reload on every key).
            let commit = {
                let (path, key, entry) = (path.clone(), field.key.clone(), entry.clone());
                move || write_value(&path, target, &key, entry.text().as_str().into())
            };
            entry.connect_activate({
                let commit = commit.clone();
                move |_| commit()
            });
            let focus = EventControllerFocus::new();
            focus.connect_leave(move |_| commit());
            entry.add_controller(focus);
            row.append(&entry);
        }
        FieldKind::Int { min, max, default } => {
            let cur = current_int(config, target, &field.key).unwrap_or(*default);
            let spin = SpinButton::with_range(*min as f64, *max as f64, 1.0);
            spin.set_value(cur as f64); // before connecting, so the programmatic set doesn't write
            let (path, key) = (path.clone(), field.key.clone());
            spin.connect_value_changed(move |s| {
                write_value(&path, target, &key, (s.value() as i64).into());
            });
            row.append(&spin);
        }
        FieldKind::Bool { default } => {
            let cur = current_bool(config, target, &field.key).unwrap_or(*default);
            let sw = Switch::new();
            sw.set_valign(Align::Center);
            sw.set_active(cur); // before connecting
            let (path, key) = (path.clone(), field.key.clone());
            sw.connect_active_notify(move |s| {
                write_value(&path, target, &key, s.is_active().into());
            });
            row.append(&sw);
        }
        FieldKind::Choice { options, default } => {
            let labels: Vec<&str> = options.iter().map(|(_, l)| l.as_str()).collect();
            let dd = DropDown::from_strings(&labels);
            let cur = current_str(config, target, &field.key).unwrap_or_else(|| default.clone());
            let idx = options.iter().position(|(v, _)| *v == cur).unwrap_or(0);
            dd.set_selected(idx as u32); // before connecting
            let (path, key, options) = (path.clone(), field.key.clone(), options.clone());
            dd.connect_selected_notify(move |d| {
                if let Some((value, _)) = options.get(d.selected() as usize) {
                    write_value(&path, target, &key, value.as_str().into());
                }
            });
            row.append(&dd);
        }
        FieldKind::Note(text) => {
            let note = Label::new(Some(text));
            note.set_xalign(1.0);
            note.set_wrap(true);
            note.add_css_class("dim-label");
            row.append(&note);
        }
    }
    row
}

/// The bar's own settings, described against the `[bar]` table. (Not from a plugin — the host owns
/// the bar config.)
fn bar_config_schema() -> Vec<ConfigField> {
    vec![
        ConfigField::choice("position", "Position", &[("top", "Top"), ("bottom", "Bottom")], "top"),
        ConfigField::int("height", "Height (px)", 16, 64, 28),
        // TODO(prefs): populate monitor options from the live output list (a dynamic Choice via
        // `&self`), instead of the static common selectors.
        ConfigField::choice(
            "monitor",
            "Monitor",
            &[
                ("primary", "Primary"),
                ("left", "Left"),
                ("center", "Center"),
                ("right", "Right"),
                ("all", "All"),
            ],
            "primary",
        ),
        ConfigField::text("theme", "Theme CSS path (blank = built-in Nord)", ""),
    ]
}

// --- current-value readers: typed, via ModuleConfig accessors for modules and the typed BarConfig
// fields for the bar — so no `toml` type crosses into the binary. ---

fn current_str(config: &Config, target: Target, key: &str) -> Option<String> {
    match target {
        Target::Module(i) => config.modules.get(i)?.opt_str(key).map(String::from),
        Target::Bar => match key {
            "position" => Some(position_str(config.bar.position).to_string()),
            "monitor" => Some(config.bar.monitor.clone()),
            "theme" => config.bar.theme.clone(),
            _ => None,
        },
    }
}

fn current_int(config: &Config, target: Target, key: &str) -> Option<i64> {
    match target {
        Target::Module(i) => config.modules.get(i)?.opt_i64(key),
        Target::Bar => (key == "height").then_some(config.bar.height as i64),
    }
}

fn current_bool(config: &Config, target: Target, key: &str) -> Option<bool> {
    match target {
        Target::Module(i) => config.modules.get(i)?.opt_bool(key),
        Target::Bar => None, // no boolean bar fields yet
    }
}

fn position_str(p: Position) -> &'static str {
    match p {
        Position::Top => "top",
        Position::Bottom => "bottom",
    }
}

/// Write one value back to the config file. The UI does **not** apply edits directly — it writes the
/// TOML and lets the file watcher drive the reload. That avoids double-apply and keeps the UI and
/// hand-edits on a single code path.
fn write_value(path: &Path, target: Target, key: &str, value: toml_edit::Value) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            warn!(error = %e, "preferences: cannot read config to write; edit dropped");
            return;
        }
    };
    let mut doc = match text.parse::<toml_edit::DocumentMut>() {
        Ok(d) => d,
        Err(e) => {
            warn!(error = %e, "preferences: config no longer parses; edit dropped");
            return;
        }
    };

    match target {
        Target::Bar => doc["bar"][key] = toml_edit::Item::Value(value),
        Target::Module(i) => {
            // Defensive: the entry can vanish if the file was edited (module removed) while the
            // window is open. Don't write to the wrong index — drop the edit and warn.
            let entry = doc
                .get_mut("modules")
                .and_then(|m| m.as_array_of_tables_mut())
                .and_then(|a| a.get_mut(i));
            match entry {
                Some(table) => table[key] = toml_edit::Item::Value(value),
                None => {
                    warn!(index = i, "preferences: module no longer in config; edit dropped");
                    return;
                }
            }
        }
    }

    if let Err(e) = std::fs::write(path, doc.to_string()) {
        warn!(error = %e, "preferences: failed to write config; edit dropped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: &str = "schema = 1\n\
        # keep me\n\
        [bar]\nposition = \"top\"\nheight = 28\n\n\
        [[modules]]\ntype = \"clock\"\ncell = { row = 0, col = 0 }\nformat = \"%H:%M\"\n";

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir()
            .join(format!("wfb-prefs-{}-{}.toml", std::process::id(), tag));
        std::fs::write(&p, CFG).unwrap();
        p
    }

    #[test]
    fn write_module_option_preserves_comments_and_changes_value() {
        let p = tmp("mod");
        write_value(&p, Target::Module(0), "format", "%H:%M:%S".into());
        let out = std::fs::read_to_string(&p).unwrap();
        assert!(out.contains("# keep me"), "comment preserved across write");
        let cfg = Config::parse(&out).unwrap();
        assert_eq!(cfg.modules[0].opt_str("format"), Some("%H:%M:%S"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn write_bar_value_round_trips() {
        let p = tmp("bar");
        write_value(&p, Target::Bar, "height", 40i64.into());
        let cfg = Config::parse(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(cfg.bar.height, 40);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn write_to_missing_module_index_is_dropped() {
        let p = tmp("oob");
        write_value(&p, Target::Module(99), "format", "x".into());
        // Out-of-bounds index leaves the file byte-for-byte unchanged.
        assert_eq!(std::fs::read_to_string(&p).unwrap(), CFG);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn current_readers_pull_typed_values() {
        let cfg = Config::parse(CFG).unwrap();
        assert_eq!(current_str(&cfg, Target::Module(0), "format"), Some("%H:%M".into()));
        assert_eq!(current_str(&cfg, Target::Bar, "position"), Some("top".into()));
        assert_eq!(current_int(&cfg, Target::Bar, "height"), Some(28));
        assert_eq!(current_bool(&cfg, Target::Module(0), "expand"), None);
    }
}
