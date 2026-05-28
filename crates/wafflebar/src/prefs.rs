//! Preferences UI (F3) + Add Items / launcher items editor (F4). A host-rendered settings window:
//! a master list of the bar plus each placed module on the left, and on the right a form built from
//! the selected target's `config_schema()`.
//!
//! The plugins never own dialogs (the GTK-free invariant) — the host renders every form from the
//! declarative schema. Editing a field **writes the value back to `config.toml`** (via `toml_edit`,
//! preserving comments/order) and stops there: the existing file-watch reload (F2/F2b) picks the
//! change up and applies it through `configure()` or a structural rebuild. The window is just
//! another writer of the TOML, on the *same* code path as a hand edit — no separate apply pipeline.
//!
//! F4 adds the Add Items dialog (append a `[[modules]]` entry from the plugin catalog) and the
//! launcher items editor (the `DesktopList` field — add/remove/reorder `.desktop` references with an
//! application picker). Adding/removing a *module* reopens the window (the left list changes);
//! editing a module's *options* rebuilds only the form pane.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    gdk, Align, Button, CheckButton, DragSource, DropDown, DropTarget, Entry, EventControllerFocus,
    Image, Label, ListBox, ListBoxRow, Orientation, ScrolledWindow, SearchEntry, SpinButton, Switch,
    Window,
};
use tracing::warn;

use crate::dropdown;
use wafflebar_core::{Config, ConfigField, FieldKind, Position};

use crate::plugins;

/// What a form is editing: the `[bar]` table, or the i-th `[[modules]]` entry.
#[derive(Clone, Copy)]
enum Target {
    Bar,
    Module(usize),
}

/// Shared handles for the open Settings panel. `window`/`form` are `Weak` so handlers owned by those
/// widgets don't form a reference cycle — same discipline as `Weak<Host>`.
#[derive(Clone)]
struct Ctx {
    path: Rc<PathBuf>,
    window: glib::WeakRef<Window>,
    form: glib::WeakRef<gtk4::Box>,
    /// The bar edge, so nested pickers anchor to the same side as the Settings panel.
    position: Position,
    /// `[bar] lock` — when set, structural edits (reorder/add/remove) are disabled in the UI.
    locked: bool,
}

impl Ctx {
    /// Rebuild just the form pane for `target` (after an in-place option/items edit).
    fn rebuild_form(&self, target: Target) {
        if let Some(form) = self.form.upgrade() {
            build_form(&form, target, self);
        }
    }

    /// Reopen Settings (after adding/removing a module — the left list changed): close the current
    /// panel and build a fresh one. Simpler and leak-free versus an in-place list refresh.
    fn reopen(&self) {
        self.reopen_to(None);
    }

    /// Like [`reopen`](Self::reopen), but select module `select` afterward (the edit-current-item
    /// deep link — e.g. jump straight to a freshly-added module's form). `None` selects the Bar.
    fn reopen_to(&self, select: Option<usize>) {
        if let Some(win) = self.window.upgrade() {
            let path = (*self.path).clone();
            win.destroy();
            open_at(&path, select);
        }
    }
}

/// Open the preferences panel — a floating layer-shell dropdown under the bar (see
/// [`crate::dropdown`]): it floats (not tiled), accepts keyboard for text entry on dwl (which a
/// GtkPopover can't), and closes via the ✕ button or Escape.
pub fn open(config_path: &Path) {
    open_at(config_path, None);
}

/// Open Settings, optionally selecting module `select` (a deep link; `None` → the Bar row).
fn open_at(config_path: &Path, select: Option<usize>) {
    let config = match Config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "preferences: config no longer loads; not opening");
            return;
        }
    };
    let path = Rc::new(config_path.to_path_buf());

    let window = Window::builder().default_width(620).default_height(460).build();
    dropdown::panel(&window, config.bar.position); // floating dropdown hugging the bar's edge

    let form = gtk4::Box::new(Orientation::Vertical, 8);
    form.set_margin_top(12);
    form.set_margin_bottom(12);
    form.set_margin_start(12);
    form.set_margin_end(12);

    let locked = config.bar.lock;
    let ctx = Ctx {
        path,
        window: window.downgrade(),
        form: form.downgrade(),
        position: config.bar.position,
        locked,
    };

    // Left: target list (Bar + each module) over Add / Remove-selected buttons.
    let list = ListBox::new();
    list.set_width_request(190);
    list.append(&list_row("Bar  ·  position, size, theme"));
    // Per-module checkboxes for multi-select removal — independent of the single-select form
    // navigation (clicking a row's label still drives the form; the checkbox only marks for delete).
    let checks: Rc<RefCell<Vec<CheckButton>>> = Rc::new(RefCell::new(Vec::new()));
    for (mi, m) in config.modules.iter().enumerate() {
        // Locked: no grip glyph, no checkbox, no drag — the row is just a navigable label.
        let prefix = if locked { "" } else { "⠿  " };
        let label = Label::new(Some(&format!("{prefix}{}  ·  cell ({}, {})", m.kind, m.cell.row, m.cell.col)));
        label.set_xalign(0.0);
        let hbox = gtk4::Box::new(Orientation::Horizontal, 6);
        hbox.set_margin_top(6);
        hbox.set_margin_bottom(6);
        hbox.set_margin_start(8);
        hbox.set_margin_end(8);
        if !locked {
            let check = CheckButton::new();
            check.set_valign(Align::Center);
            hbox.append(&check);
            checks.borrow_mut().push(check);
        }
        hbox.append(&label);
        let row = ListBoxRow::new();
        row.set_child(Some(&hbox));
        if !locked {
            // Drag-to-reorder (P5): the row carries its module index; dropping it on another module
            // row rewrites the `[[modules]]` order in the TOML, which the watcher reloads. The Bar
            // row above has no source/target, so it's neither draggable nor a drop site.
            let src = DragSource::new();
            src.set_actions(gdk::DragAction::MOVE);
            src.connect_prepare(move |_, _, _| {
                Some(gdk::ContentProvider::for_value(&(mi as i32).to_value()))
            });
            row.add_controller(src);
            let tgt = DropTarget::new(i32::static_type(), gdk::DragAction::MOVE);
            tgt.connect_drop({
                let ctx = ctx.clone();
                move |_, val, _, _| {
                    let Ok(from) = val.get::<i32>() else { return false };
                    let from = from as usize;
                    if from != mi {
                        move_module(&ctx.path, from, mi);
                        ctx.reopen(); // rebuild the window so the list reflects the new order
                    }
                    true
                }
            });
            row.add_controller(tgt);
        }
        list.append(&row);
    }
    list.connect_row_selected({
        let ctx = ctx.clone();
        move |_, row| {
            let target = match row.map(|r| r.index()) {
                Some(0) => Target::Bar,
                Some(i) => Target::Module((i - 1) as usize),
                None => return,
            };
            ctx.rebuild_form(target);
        }
    });

    let add_btn = Button::with_label("➕  Add Item…");
    add_btn.set_margin_top(6);
    add_btn.connect_clicked({
        let ctx = ctx.clone();
        move |_| open_add_items(&ctx)
    });

    let remove_btn = Button::with_label("🗑  Remove selected");
    remove_btn.add_css_class("destructive-action");
    remove_btn.connect_clicked({
        let (ctx, checks) = (ctx.clone(), checks.clone());
        move |_| {
            let idxs: Vec<usize> = checks
                .borrow()
                .iter()
                .enumerate()
                .filter(|(_, c)| c.is_active())
                .map(|(i, _)| i)
                .collect();
            if idxs.is_empty() {
                return;
            }
            remove_modules(&ctx.path, &idxs);
            ctx.reopen(); // the module list changed — rebuild the window
        }
    });

    // Reserve the list's width on the ScrolledWindow itself (a scroll wrapper doesn't propagate its
    // child's width request) + drop horizontal scroll, so the hexpanding form pane can't overlap it.
    let list_scroll = ScrolledWindow::builder().child(&list).vexpand(true).build();
    list_scroll.set_width_request(190);
    list_scroll.set_hexpand(false);
    list_scroll.set_hscrollbar_policy(gtk4::PolicyType::Never);
    let left = gtk4::Box::new(Orientation::Vertical, 0);
    left.append(&list_scroll);
    if locked {
        // Layout locked: no add/remove, and a hint why (unlock via the Bar form's "Lock layout").
        let note = list_row("🔒  Layout locked");
        note.set_selectable(false);
        left.append(&note);
    } else {
        left.append(&add_btn);
        left.append(&remove_btn);
    }

    let split = gtk4::Box::new(Orientation::Horizontal, 0);
    split.set_vexpand(true);
    split.append(&left);
    split.append(&gtk4::Separator::new(Orientation::Vertical));
    split.append(&ScrolledWindow::builder().child(&form).hexpand(true).build());

    // Header with a close button (a layer-shell panel has no WM titlebar). Escape also closes.
    let header = gtk4::Box::new(Orientation::Horizontal, 8);
    header.set_margin_top(8);
    header.set_margin_start(12);
    header.set_margin_end(8);
    let title = Label::new(Some("wafflebar settings"));
    title.set_xalign(0.0);
    title.set_hexpand(true);
    let close = Button::with_label("✕");
    {
        let w = window.downgrade();
        close.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.destroy();
            }
        });
    }
    header.append(&title);
    header.append(&close);

    let root = gtk4::Box::new(Orientation::Vertical, 0);
    root.add_css_class("settings"); // themed (nord/dawn) to match the apps menu, not native GTK
    root.append(&header);
    root.append(&split);
    window.set_child(Some(&root));

    // Select the deep-link target (module row = index+1; row 0 is the Bar), else the Bar. Falls back
    // to the Bar if the index is out of range.
    let row_index = match select {
        Some(i) if (i as i32) < config.modules.len() as i32 => i as i32 + 1,
        _ => 0,
    };
    list.select_row(list.row_at_index(row_index).as_ref());
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

/// Rebuild the form pane for `target`. Loads the config fresh from disk each time, so the form
/// always reflects the file (including hand edits) and there's no shared mutable config state.
fn build_form(form: &gtk4::Box, target: Target, ctx: &Ctx) {
    while let Some(child) = form.first_child() {
        form.remove(&child);
    }
    let config = match Config::load(&*ctx.path) {
        Ok(c) => c,
        Err(_) => {
            form.append(&dim_label("Config file no longer loads."));
            return;
        }
    };

    // Per-plugin "About" header: the module's display name + what it is, from the catalog.
    if let Target::Module(i) = target {
        if let Some(m) = config.modules.get(i) {
            if let Some(info) = plugins::catalog().into_iter().find(|p| p.kind == m.kind) {
                let name = Label::new(None);
                name.set_xalign(0.0);
                name.set_markup(&format!("<b>{}</b>", info.name)); // names are static, no markup chars
                form.append(&name);
                let about = dim_label(info.description);
                about.set_margin_bottom(8);
                form.append(&about);
            }
        }
    }

    let fields = match target {
        Target::Bar => bar_config_schema(),
        Target::Module(i) => {
            config.modules.get(i).map(|m| plugins::config_schema(&m.kind)).unwrap_or_default()
        }
    };

    if fields.is_empty() && matches!(target, Target::Module(_)) {
        form.append(&dim_label("This module has no configurable options."));
    }
    for field in &fields {
        form.append(&field_row(field, target, &config, ctx));
    }

    match target {
        Target::Bar => {
            // Every `[bar]` property is fixed at window creation (anchors, exclusive zone, monitor,
            // CSS) and the F2b reload rebuilds only the grid — so bar edits persist but apply on
            // restart. Surface that honestly rather than hiding a path that only half-works.
            let note = dim_label("Bar settings apply after restarting wafflebar.");
            note.set_margin_top(8);
            form.append(&note);
        }
        Target::Module(i) if !ctx.locked => {
            let remove = Button::with_label("Remove this item");
            remove.add_css_class("destructive-action");
            remove.set_margin_top(12);
            remove.set_halign(Align::Start);
            let ctx = ctx.clone();
            remove.connect_clicked(move |_| {
                remove_module(&ctx.path, i);
                ctx.reopen();
            });
            form.append(&remove);
        }
        Target::Module(_) => {} // locked: field edits stay, but no remove
    }
}

fn dim_label(text: &str) -> Label {
    let l = Label::new(Some(text));
    l.set_xalign(0.0);
    l.set_wrap(true);
    l.add_css_class("dim-label");
    l
}

/// One label-plus-control row, control populated from the current TOML value, change handler wired
/// to write the value back.
fn field_row(field: &ConfigField, target: Target, config: &Config, ctx: &Ctx) -> gtk4::Box {
    let row = gtk4::Box::new(Orientation::Horizontal, 12);
    let label = Label::new(Some(&field.label));
    label.set_xalign(0.0);
    label.set_valign(Align::Start);
    label.set_hexpand(true);
    row.append(&label);
    let path = &ctx.path;

    match &field.kind {
        FieldKind::Text { default } => {
            let entry = Entry::new();
            entry.set_text(&current_str(config, target, &field.key).unwrap_or_else(|| default.clone()));
            // Commit on Enter and on focus-leave (not per keystroke — that would reload every key).
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
        FieldKind::File { default } => {
            // Entry (accepts a theme name or a path) + a Browse… file chooser that fills the path.
            let entry = Entry::new();
            entry.set_hexpand(true);
            entry.set_text(&current_str(config, target, &field.key).unwrap_or_else(|| default.clone()));
            let commit = {
                let (path, key, entry) = (path.clone(), field.key.clone(), entry.clone());
                move || write_value(&path, target, &key, entry.text().as_str().into())
            };
            entry.connect_activate({
                let commit = commit.clone();
                move |_| commit()
            });
            let focus = EventControllerFocus::new();
            focus.connect_leave({
                let commit = commit.clone();
                move |_| commit()
            });
            entry.add_controller(focus);

            let browse = Button::with_label("Browse…");
            browse.connect_clicked({
                let (entry, commit) = (entry.clone(), commit.clone());
                move |btn| {
                    let dialog = gtk4::FileDialog::builder().title("Choose an icon image").build();
                    let win = btn.root().and_downcast::<gtk4::Window>();
                    let (entry, commit) = (entry.clone(), commit.clone());
                    dialog.open(win.as_ref(), gtk4::gio::Cancellable::NONE, move |res| {
                        if let Some(p) = res.ok().and_then(|f| f.path()) {
                            entry.set_text(&p.to_string_lossy());
                            commit();
                        }
                    });
                }
            });

            let hbox = gtk4::Box::new(Orientation::Horizontal, 6);
            hbox.append(&entry);
            hbox.append(&browse);
            row.append(&hbox);
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
            let note = dim_label(text);
            note.set_xalign(1.0);
            row.append(&note);
        }
        FieldKind::DesktopList => {
            row.append(&items_editor(target, &field.key, config, ctx));
        }
    }
    row
}

/// The launcher items editor (F4): the current `.desktop` references with remove + up/down reorder,
/// over an "Add Application" button that opens the picker. Each action rewrites the whole `items`
/// array to TOML, then rebuilds the form pane.
fn items_editor(target: Target, key: &str, config: &Config, ctx: &Ctx) -> gtk4::Box {
    let col = gtk4::Box::new(Orientation::Vertical, 4);
    let items: Vec<String> = match target {
        Target::Module(i) => config.modules.get(i).map(|m| m.opt_str_list(key)).unwrap_or_default(),
        Target::Bar => Vec::new(),
    };

    for (j, item) in items.iter().enumerate() {
        let line = gtk4::Box::new(Orientation::Horizontal, 4);
        let name = Label::new(Some(item));
        name.set_xalign(0.0);
        name.set_hexpand(true);
        line.append(&name);
        // Up/down reorder (DnD deferred to v2); disabled at the ends.
        let up = Button::from_icon_name("go-up-symbolic");
        up.set_sensitive(j > 0);
        wire_item_edit(&up, target, key, ctx, items.clone(), move |v| v.swap(j, j - 1));
        let down = Button::from_icon_name("go-down-symbolic");
        down.set_sensitive(j + 1 < items.len());
        wire_item_edit(&down, target, key, ctx, items.clone(), move |v| v.swap(j, j + 1));
        let remove = Button::from_icon_name("list-remove-symbolic");
        wire_item_edit(&remove, target, key, ctx, items.clone(), move |v| {
            v.remove(j);
        });
        line.append(&up);
        line.append(&down);
        line.append(&remove);
        col.append(&line);
    }

    let add = Button::with_label("Add Application…");
    add.set_halign(Align::Start);
    add.connect_clicked({
        let ctx = ctx.clone();
        let key = key.to_string();
        let existing = items.clone();
        move |_| open_app_picker(&ctx, target, &key, existing.clone())
    });
    col.append(&add);
    col
}

/// Wire a button to apply `edit` to the current items vector, write it, and rebuild the form.
fn wire_item_edit(
    button: &Button,
    target: Target,
    key: &str,
    ctx: &Ctx,
    items: Vec<String>,
    edit: impl Fn(&mut Vec<String>) + 'static,
) {
    let (ctx, key) = (ctx.clone(), key.to_string());
    button.connect_clicked(move |_| {
        let mut items = items.clone();
        edit(&mut items);
        if let Target::Module(i) = target {
            write_items(&ctx.path, i, &key, &items);
        }
        ctx.rebuild_form(target);
    });
}

/// Modal application picker (F4): a searchable list of installed `.desktop` apps; picking one
/// appends its id to the launcher's items.
fn open_app_picker(ctx: &Ctx, target: Target, key: &str, existing: Vec<String>) {
    let rows = wafflebar_core::list_applications()
        .into_iter()
        .map(|app| PickerRow {
            icon: app.icon,
            title: app.name,
            subtitle: app.file_id.clone(),
            sensitive: true,
            payload: app.file_id,
        })
        .collect();
    let (ctx, key) = (ctx.clone(), key.to_string());
    let position = ctx.position;
    open_picker("Add Application", position, rows, move |file_id| {
        let mut items = existing.clone();
        items.push(file_id.to_string());
        if let Target::Module(i) = target {
            write_items(&ctx.path, i, &key, &items);
        }
        ctx.rebuild_form(target);
    });
}

/// Modal Add Items dialog (F4): the plugin catalog as a searchable list; picking a kind appends a
/// `[[modules]]` entry and reopens the window.
fn open_add_items(ctx: &Ctx) {
    let config = Config::load(&*ctx.path).unwrap_or_default();
    let present: std::collections::HashSet<&str> =
        config.modules.iter().map(|m| m.kind.as_str()).collect();
    let next_col = config.modules.iter().map(|m| m.cell.col + m.cell.colspan).max().unwrap_or(0);

    let rows = plugins::catalog()
        .into_iter()
        .map(|info| {
            let taken = info.unique && present.contains(info.kind);
            PickerRow {
                icon: Some(info.icon.to_string()),
                title: info.name.to_string(),
                subtitle: if taken {
                    format!("{} (already added)", info.description)
                } else {
                    info.description.to_string()
                },
                sensitive: !taken,
                payload: info.kind.to_string(),
            }
        })
        .collect();

    // The appended module lands at the end, so its index is the current module count.
    let new_index = config.modules.len();
    let ctx = ctx.clone();
    open_picker("Add Item", ctx.position, rows, move |kind| {
        append_module(&ctx.path, kind, next_col);
        ctx.reopen_to(Some(new_index)); // deep-link straight to the just-added module's form
    });
}

/// A row in a [`open_picker`] list: an icon, a title, a dim subtitle, and the payload handed to the
/// pick callback. Insensitive rows can't be activated.
struct PickerRow {
    icon: Option<String>,
    title: String,
    subtitle: String,
    sensitive: bool,
    payload: String,
}

/// A searchable single-select list, calling `on_pick(payload)` on activation. Shared by Add Items
/// and the application picker. A standalone layer-shell dropdown (Exclusive keyboard, Escape to
/// close), so its search box can actually receive keyboard on dwl.
fn open_picker(title: &str, position: Position, rows: Vec<PickerRow>, on_pick: impl Fn(&str) + 'static) {
    let win = Window::builder().default_width(440).default_height(480).build();
    dropdown::panel(&win, position);

    let vbox = gtk4::Box::new(Orientation::Vertical, 6);
    vbox.set_margin_top(8);
    vbox.set_margin_bottom(8);
    vbox.set_margin_start(8);
    vbox.set_margin_end(8);
    let header = Label::new(Some(title));
    header.set_xalign(0.0);
    header.set_margin_bottom(4);
    vbox.append(&header);

    if rows.is_empty() {
        vbox.append(&dim_label("No applications found."));
        win.set_child(Some(&vbox));
        win.present();
        return;
    }

    let listbox = ListBox::new();
    for r in &rows {
        let line = gtk4::Box::new(Orientation::Horizontal, 8);
        line.set_margin_top(4);
        line.set_margin_bottom(4);
        line.set_margin_start(6);
        line.set_margin_end(6);
        if let Some(icon) = &r.icon {
            line.append(&Image::from_icon_name(icon));
        }
        let text = gtk4::Box::new(Orientation::Vertical, 0);
        let t = Label::new(Some(&r.title));
        t.set_xalign(0.0);
        text.append(&t);
        let s = dim_label(&r.subtitle);
        text.append(&s);
        line.append(&text);
        let lr = ListBoxRow::new();
        lr.set_child(Some(&line));
        lr.set_sensitive(r.sensitive);
        listbox.append(&lr);
    }

    let search = SearchEntry::new();
    let rows = Rc::new(rows);
    // Filter hides non-matching rows (it doesn't remove them), so `row.index()` stays aligned with
    // `rows` for the activation lookup.
    listbox.set_filter_func({
        let (rows, search) = (rows.clone(), search.clone());
        move |row| {
            let q = search.text().to_lowercase();
            q.is_empty()
                || rows
                    .get(row.index() as usize)
                    .map(|r| format!("{} {}", r.title, r.subtitle).to_lowercase().contains(&q))
                    .unwrap_or(true)
        }
    });
    search.connect_search_changed({
        let listbox = listbox.clone();
        move |_| listbox.invalidate_filter()
    });
    listbox.connect_row_activated({
        let (rows, w) = (rows.clone(), win.downgrade());
        move |_, row| {
            if let Some(r) = rows.get(row.index() as usize) {
                if r.sensitive {
                    on_pick(&r.payload);
                    if let Some(w) = w.upgrade() {
                        w.destroy();
                    }
                }
            }
        }
    });

    vbox.append(&search);
    vbox.append(&ScrolledWindow::builder().child(&listbox).vexpand(true).build());
    win.set_child(Some(&vbox));
    win.present();
}

/// The bar's own settings, described against the `[bar]` table (host-owned, not from a plugin).
fn bar_config_schema() -> Vec<ConfigField> {
    vec![
        ConfigField::choice("position", "Position", &[("top", "Top"), ("bottom", "Bottom")], "top"),
        ConfigField::int("height", "Height (px)", 16, 64, 28),
        ConfigField::int("icon_size", "Icon size (px, 0 = auto from height)", 0, 64, 0),
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
        ConfigField::int("length_percent", "Length (% of monitor width)", 1, 100, 100),
        ConfigField::choice(
            "alignment",
            "Alignment (when shorter than full)",
            &[("start", "Start"), ("center", "Center"), ("end", "End")],
            "center",
        ),
        ConfigField::bool("reserve_space", "Reserve screen space (strut)", true),
        ConfigField::bool("keep_below", "Keep below windows", false),
        ConfigField::bool("lock", "Lock layout (no reorder/add/remove)", false),
        ConfigField::text("theme", "Theme CSS path (blank = built-in Nord)", ""),
    ]
}

// --- current-value readers: typed (ModuleConfig accessors for modules, typed BarConfig fields for
// the bar), so no `toml` type crosses into the binary. ---

fn current_str(config: &Config, target: Target, key: &str) -> Option<String> {
    match target {
        Target::Module(i) => config.modules.get(i)?.opt_str(key).map(String::from),
        Target::Bar => match key {
            "position" => Some(position_str(config.bar.position).to_string()),
            "monitor" => Some(config.bar.monitor.clone()),
            "alignment" => Some(alignment_str(config.bar.alignment).to_string()),
            "theme" => config.bar.theme.clone(),
            _ => None,
        },
    }
}

fn current_int(config: &Config, target: Target, key: &str) -> Option<i64> {
    match target {
        Target::Module(i) => config.modules.get(i)?.opt_i64(key),
        Target::Bar => match key {
            "height" => Some(config.bar.height as i64),
            "icon_size" => Some(config.bar.icon_size as i64),
            "length_percent" => Some(config.bar.length_percent as i64),
            _ => None,
        },
    }
}

fn current_bool(config: &Config, target: Target, key: &str) -> Option<bool> {
    match target {
        Target::Module(i) => config.modules.get(i)?.opt_bool(key),
        Target::Bar => match key {
            "reserve_space" => Some(config.bar.reserve_space),
            "keep_below" => Some(config.bar.keep_below),
            "lock" => Some(config.bar.lock),
            _ => None,
        },
    }
}

fn alignment_str(a: wafflebar_core::Alignment) -> &'static str {
    use wafflebar_core::Alignment;
    match a {
        Alignment::Start => "start",
        Alignment::Center => "center",
        Alignment::End => "end",
    }
}

fn position_str(p: Position) -> &'static str {
    match p {
        Position::Top => "top",
        Position::Bottom => "bottom",
    }
}

/// Write one scalar value back to the config file. The UI does **not** apply edits directly — it
/// writes the TOML and lets the file watcher drive the reload. That avoids double-apply and keeps
/// the UI and hand-edits on a single code path.
fn write_value(path: &Path, target: Target, key: &str, value: toml_edit::Value) {
    edit_doc(path, |doc| match target {
        Target::Bar => {
            doc["bar"][key] = toml_edit::Item::Value(value);
            true
        }
        Target::Module(i) => match module_table(doc, i) {
            Some(table) => {
                table[key] = toml_edit::Item::Value(value);
                true
            }
            None => {
                warn!(index = i, "preferences: module no longer in config; edit dropped");
                false
            }
        },
    });
}

/// Rewrite a module's `.desktop` items array (F4 launcher editor).
fn write_items(path: &Path, module: usize, key: &str, items: &[String]) {
    edit_doc(path, |doc| match module_table(doc, module) {
        Some(table) => {
            let mut arr = toml_edit::Array::new();
            for it in items {
                arr.push(it.as_str());
            }
            table[key] = toml_edit::value(arr);
            true
        }
        None => false,
    });
}

/// Append a new `[[modules]]` entry of `kind` at row 0, column `col`, seeding the schema's declared
/// defaults (the third use of `FieldKind::default`, after form population and add-time seeding).
fn append_module(path: &Path, kind: &str, col: u32) {
    edit_doc(path, |doc| {
        let mut table = toml_edit::Table::new();
        table["type"] = toml_edit::value(kind);
        let mut cell = toml_edit::InlineTable::new();
        cell.insert("row", 0i64.into());
        cell.insert("col", i64::from(col).into());
        table["cell"] = toml_edit::value(cell);
        for field in plugins::config_schema(kind) {
            match field.kind {
                FieldKind::Text { default } | FieldKind::File { default } if !default.is_empty() => {
                    table[&field.key] = toml_edit::value(default)
                }
                FieldKind::Int { default, .. } => table[&field.key] = toml_edit::value(default),
                FieldKind::Bool { default } => table[&field.key] = toml_edit::value(default),
                FieldKind::Choice { default, .. } => table[&field.key] = toml_edit::value(default),
                _ => {}
            }
        }
        if doc.get("modules").and_then(|m| m.as_array_of_tables()).is_none() {
            doc["modules"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
        }
        doc["modules"].as_array_of_tables_mut().unwrap().push(table);
        true
    });
}

/// Move the `from`-th `[[modules]]` entry so it lands at index `to` (drag-to-reorder, P5). Rebuilds
/// the array-of-tables (toml_edit has no element-move/insert); each module table is cloned intact, so
/// per-module keys/comments survive. No-op on out-of-range or `from == to`.
fn move_module(path: &Path, from: usize, to: usize) {
    edit_doc(path, |doc| {
        let Some(aot) = doc.get("modules").and_then(|m| m.as_array_of_tables()) else {
            return false;
        };
        let n = aot.len();
        if from >= n || to >= n || from == to {
            return false;
        }
        // Reuse the modules' existing document positions (ascending), reassigned in the new order —
        // toml_edit renders array-of-tables by each table's parsed `position`, so a bare reorder of
        // the Vec is ignored; setting fresh small positions would instead hoist them above `[bar]`.
        let mut slots: Vec<usize> = aot.iter().filter_map(|t| t.position()).collect();
        slots.sort_unstable();
        let mut tables: Vec<toml_edit::Table> = aot.iter().cloned().collect();
        let moved = tables.remove(from);
        tables.insert(to.min(tables.len()), moved);
        let mut rebuilt = toml_edit::ArrayOfTables::new();
        for (i, mut t) in tables.into_iter().enumerate() {
            if let Some(p) = slots.get(i) {
                t.set_position(*p);
            }
            rebuilt.push(t);
        }
        doc["modules"] = toml_edit::Item::ArrayOfTables(rebuilt);
        true
    });
}

/// Remove several `[[modules]]` entries in one write (multi-select removal). Removes in descending
/// index order so earlier indices stay valid; de-duped; out-of-range indices skipped.
fn remove_modules(path: &Path, indices: &[usize]) {
    let mut idxs: Vec<usize> = indices.to_vec();
    idxs.sort_unstable();
    idxs.dedup();
    edit_doc(path, |doc| {
        let Some(aot) = doc.get_mut("modules").and_then(|m| m.as_array_of_tables_mut()) else {
            return false;
        };
        let mut removed = false;
        for &i in idxs.iter().rev() {
            if i < aot.len() {
                aot.remove(i);
                removed = true;
            }
        }
        removed
    });
}

/// Remove the i-th `[[modules]]` entry.
fn remove_module(path: &Path, i: usize) {
    edit_doc(path, |doc| match doc.get_mut("modules").and_then(|m| m.as_array_of_tables_mut()) {
        Some(aot) if i < aot.len() => {
            aot.remove(i);
            true
        }
        _ => false,
    });
}

/// Defensive accessor: the i-th `[[modules]]` table, or `None` if the entry vanished (the file was
/// edited while the window was open) — so an edit is never written to the wrong index.
fn module_table(doc: &mut toml_edit::DocumentMut, i: usize) -> Option<&mut toml_edit::Table> {
    doc.get_mut("modules").and_then(|m| m.as_array_of_tables_mut()).and_then(|a| a.get_mut(i))
}

/// Load → mutate (via `edit`, which returns whether to write) → write the config file. Read/parse
/// failures warn and drop the edit rather than clobbering the file.
fn edit_doc(path: &Path, edit: impl FnOnce(&mut toml_edit::DocumentMut) -> bool) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => return warn!(error = %e, "preferences: cannot read config; edit dropped"),
    };
    let mut doc = match text.parse::<toml_edit::DocumentMut>() {
        Ok(d) => d,
        Err(e) => return warn!(error = %e, "preferences: config no longer parses; edit dropped"),
    };
    if edit(&mut doc) {
        if let Err(e) = std::fs::write(path, doc.to_string()) {
            warn!(error = %e, "preferences: failed to write config; edit dropped");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: &str = "schema = 1\n\
        # keep me\n\
        [bar]\nposition = \"top\"\nheight = 28\n\n\
        [[modules]]\ntype = \"clock\"\ncell = { row = 0, col = 0 }\nformat = \"%H:%M\"\n\n\
        [[modules]]\ntype = \"launcher\"\ncell = { row = 0, col = 1 }\nitems = [\"firefox.desktop\"]\n";

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("wfb-prefs-{}-{}.toml", std::process::id(), tag));
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
        assert_eq!(std::fs::read_to_string(&p).unwrap(), CFG, "out-of-bounds edit changed nothing");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn append_module_adds_entry_with_defaults_and_next_col() {
        let p = tmp("append");
        append_module(&p, "memory", 2);
        let cfg = Config::parse(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(cfg.modules.len(), 3);
        let m = &cfg.modules[2];
        assert_eq!(m.kind, "memory");
        assert_eq!(m.cell.col, 2);
        assert_eq!(m.opt_i64("interval"), Some(5)); // schema default seeded
        assert!(std::fs::read_to_string(&p).unwrap().contains("# keep me"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn remove_module_drops_entry() {
        let p = tmp("rm");
        remove_module(&p, 0);
        let cfg = Config::parse(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(cfg.modules.len(), 1);
        assert_eq!(cfg.modules[0].kind, "launcher");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn remove_modules_drops_several_at_once() {
        // CFG = [clock, launcher]. Removing both (any order) → empty; descending-order safe.
        let p = tmp("rmm");
        remove_modules(&p, &[1, 0]);
        assert_eq!(Config::parse(&std::fs::read_to_string(&p).unwrap()).unwrap().modules.len(), 0);
        std::fs::remove_file(&p).ok();
        // Removing just index 1 leaves clock; out-of-range/dup indices are ignored.
        let p = tmp("rmm2");
        remove_modules(&p, &[1, 1, 9]);
        let cfg = Config::parse(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(cfg.modules.len(), 1);
        assert_eq!(cfg.modules[0].kind, "clock");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn move_module_reorders_and_preserves_tables() {
        let p = tmp("move"); // CFG = [clock, launcher]
        move_module(&p, 0, 1); // clock → index 1
        let out = std::fs::read_to_string(&p).unwrap();
        let cfg = Config::parse(&out).unwrap();
        let kinds: Vec<&str> = cfg.modules.iter().map(|m| m.kind.as_str()).collect();
        assert_eq!(kinds, ["launcher", "clock"], "order swapped");
        // per-module keys survive the rebuild, and the top-of-file comment is untouched
        assert_eq!(cfg.modules[1].opt_str("format"), Some("%H:%M"), "moved module kept its keys");
        assert!(out.contains("# keep me"));
        // no-ops: out of range / same index leave the file byte-identical
        let before = std::fs::read_to_string(&p).unwrap();
        move_module(&p, 1, 1);
        move_module(&p, 9, 0);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before, "no-op moves change nothing");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn write_items_replaces_the_array() {
        let p = tmp("items");
        write_items(&p, 1, "items", &["a.desktop".into(), "b.desktop".into()]);
        let cfg = Config::parse(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(cfg.modules[1].opt_str_list("items"), vec!["a.desktop", "b.desktop"]);
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
