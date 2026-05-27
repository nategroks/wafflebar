//! Host-rendered applications menu (E2). The `appmenu` plugin emits a `View::AppMenu` marker; the
//! renderer calls [`build_appmenu`] to construct the actual two-pane search/category/app widget.
//!
//! This is host infrastructure, not a reducer: the menu's interactive state (search text, which
//! category/app is selected) lives in these GTK widgets, never in the plugin. The reducer-owned
//! config (favorites, recents settings) arrives via the marker; the app list + recents come from
//! the host-owned [`MenuState`] (refreshed by a directory watch in `app.rs`); launches go straight
//! to the host executor. See `View::AppMenu` and docs/UPSTREAM.md.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{
    gdk, glib, Box as GtkBox, EventControllerFocus, EventControllerKey, Image, Label, ListBox,
    ListBoxRow, Orientation, Popover, PropagationPhase, ScrolledWindow, SearchEntry,
};
use tracing::warn;
use wafflebar_core::{categorized, DesktopApp, Recents};

use crate::render::Host;

/// Host-owned application data backing the menu: the parsed/enumerated apps (refreshed on install/
/// uninstall by the directory watch) and the frecency recents. Built once, mutated by the watch.
#[derive(Default)]
pub struct MenuState {
    pub apps: Vec<DesktopApp>,
    pub recents: Recents,
}

/// Fixed popover size — sized for typical desktop displays; adapt to display dimensions in v2.
const MENU_W: i32 = 420;
const MENU_H: i32 = 520;

// --- Keyboard navigation (E3) ---
//
// The nav logic is a pure function so it's testable without GTK (the sandbox can't inject input,
// and GTK widget tests can't run — one-init-per-process). The widget below is a thin translator:
// it normalizes a GTK key event to `NavKey`, calls `handle_key`, and applies the `KeyAction`.
// (Third instance of the "extract the decision from the I/O" testability pattern, after FakeWm and
// the recents internals — see docs/UPSTREAM.md.)

/// Which pane the keyboard is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pane {
    Search,
    Categories,
    Apps,
}

/// A normalized key (GTK-free). The translator emits `Char` only for an *unmodified* printable key,
/// so compositor bindings (Super+key) and stray Ctrl/Alt combos never reach `handle_key`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavKey {
    Up,
    Down,
    Left,
    Right,
    Tab,
    ShiftTab,
    Enter,
    Escape,
    CtrlF,
    Char(char),
}

/// What the widget should do. `Move` is a relative intent on the focused pane, **clamped by the
/// caller** (`handle_key` doesn't know list lengths). `NoOp` means "let GTK handle it natively"
/// (text-cursor motion, native insert) — *not* "ignore".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyAction {
    Focus(Pane),
    Move(i32),
    Launch,
    Close,
    TypeIntoSearch(char),
    NoOp,
}

/// Pure keyboard-nav decision. State is just the focused pane and whether the search box is empty;
/// selection indices live in the `ListBox`es (GTK owns them), so there's no state to keep in sync.
pub fn handle_key(pane: Pane, search_empty: bool, key: NavKey) -> KeyAction {
    use KeyAction as A;
    use NavKey as K;
    use Pane::{Apps, Categories, Search};
    match key {
        K::Escape => A::Close,
        K::CtrlF => A::Focus(Search),
        // Tab cycles Search → Categories → Apps → Search; Shift+Tab reverses.
        K::Tab => A::Focus(match pane {
            Search => Categories,
            Categories => Apps,
            Apps => Search,
        }),
        K::ShiftTab => A::Focus(match pane {
            Search => Apps,
            Categories => Search,
            Apps => Categories,
        }),
        // Unmodified printable: in a pane, divert to search; in search, GTK inserts natively.
        K::Char(c) => match pane {
            Search => A::NoOp,
            Categories | Apps => A::TypeIntoSearch(c),
        },
        K::Up => match pane {
            Categories | Apps => A::Move(-1),
            Search => A::NoOp,
        },
        K::Down => match pane {
            Categories | Apps => A::Move(1),
            Search => A::Focus(Apps), // dive into results
        },
        K::Left => match pane {
            Apps => A::Focus(Categories),
            _ => A::NoOp,
        },
        K::Right => match pane {
            Categories => A::Focus(Apps),
            _ => A::NoOp,
        },
        K::Enter => match pane {
            Apps => A::Launch,
            Categories => A::Focus(Apps),
            // Don't launch a default selection the user never navigated to.
            Search => {
                if search_empty {
                    A::NoOp
                } else {
                    A::Launch
                }
            }
        },
    }
}

/// Build the applications-menu widget (the popover content). Reads the current app cache + recents
/// from the host; rebuilt when the watch refreshes the cache (via a re-render), so opening just
/// shows pre-built content — the cold-open path does no parsing.
pub fn build_appmenu(favorites: &[String], show_recents: bool, max_recents: u32, host: &Rc<Host>) -> gtk4::Widget {
    let state = host.menu();
    let state = state.borrow();

    // Sections (category-pane rows): the virtual Favorites/Recent first, then the real categories.
    let resolve = |ids: &[String]| -> Vec<DesktopApp> {
        ids.iter().filter_map(|id| find_app(&state.apps, id)).collect()
    };
    let mut sections: Vec<(String, Vec<DesktopApp>)> = Vec::new();
    let favs = resolve(favorites);
    if !favs.is_empty() {
        sections.push(("Favorites".into(), favs));
    }
    if show_recents && max_recents > 0 {
        let recent = resolve(&state.recents.ranked(max_recents as usize));
        if !recent.is_empty() {
            sections.push(("Recent".into(), recent));
        }
    }
    for (name, apps) in categorized(state.apps.clone()) {
        sections.push((name.to_string(), apps));
    }
    drop(state);

    let root = GtkBox::new(Orientation::Vertical, 6);
    root.add_css_class("appmenu-popover");
    root.set_size_request(MENU_W, MENU_H);
    root.set_margin_top(6);
    root.set_margin_bottom(6);
    root.set_margin_start(6);
    root.set_margin_end(6);

    if sections.is_empty() {
        root.append(&dim("No applications found."));
        return root.upcast();
    }

    let search = SearchEntry::new();
    let cat_list = ListBox::new();
    cat_list.set_width_request(150);
    let app_list = ListBox::new();
    app_list.set_hexpand(true);

    // The apps currently shown in the app pane, so row-activation can map an index back to an app.
    let shown: Rc<RefCell<Vec<DesktopApp>>> = Rc::new(RefCell::new(Vec::new()));
    let sections = Rc::new(sections);
    // Keyboard focus is the one bit of nav state we own (not cleanly queryable from GTK at key
    // time): a `Cell` because we read/write it from event closures that borrow their environment
    // immutably. The key controller and the mouse handlers both update it.
    let pane = Rc::new(Cell::new(Pane::Search));

    for (name, _) in sections.iter() {
        cat_list.append(&text_row(name));
    }

    // Category selection (only meaningful when not searching): show that category's apps.
    cat_list.connect_row_selected({
        let (sections, shown, app_list, host, pane) =
            (sections.clone(), shown.clone(), app_list.clone(), host.clone(), pane.clone());
        move |_, row| {
            if let Some(idx) = row.map(|r| r.index() as usize) {
                pane.set(Pane::Categories);
                if let Some((_, apps)) = sections.get(idx) {
                    populate_apps(&app_list, &shown, apps, &host);
                }
            }
        }
    });

    // Search: empty → the selected category's apps + categories enabled; non-empty → all matches
    // across categories + categories greyed (they're navigation hints, not filters, while searching).
    search.connect_search_changed({
        let (sections, shown, app_list, cat_list, host) =
            (sections.clone(), shown.clone(), app_list.clone(), cat_list.clone(), host.clone());
        move |entry| {
            let query = entry.text().to_lowercase();
            set_categories_enabled(&cat_list, query.is_empty());
            if query.is_empty() {
                let idx = cat_list.selected_row().map(|r| r.index() as usize).unwrap_or(0);
                let apps = sections.get(idx).map(|(_, a)| a.clone()).unwrap_or_default();
                populate_apps(&app_list, &shown, &apps, &host);
            } else {
                populate_apps(&app_list, &shown, &search_matches(&sections, &query), &host);
            }
        }
    });

    // Launch on click: build the intent, record the launch, close the menu.
    app_list.connect_row_activated({
        let (shown, host) = (shown.clone(), host.clone());
        move |list, row| {
            let app = shown.borrow().get(row.index() as usize).cloned();
            if let Some(app) = app {
                launch(&app, &host);
                if let Some(pop) = list.ancestor(Popover::static_type()).and_downcast::<Popover>() {
                    pop.popdown();
                }
            }
        }
    });

    // Track focus when the user clicks into search; focus search when the menu opens (so the user
    // can type immediately).
    let focus_search = EventControllerFocus::new();
    focus_search.connect_enter({
        let pane = pane.clone();
        move |_| pane.set(Pane::Search)
    });
    search.add_controller(focus_search);
    search.connect_map(|s| {
        s.grab_focus();
    });

    // Keyboard navigation (E3): a capture-phase controller decides Up/Down/Tab/etc. *before* the
    // ListBoxes' built-in nav, routing through the pure `handle_key`. `NoOp` → propagate so GTK
    // handles it natively (text-cursor motion, native search insert).
    let keys = EventControllerKey::new();
    keys.set_propagation_phase(PropagationPhase::Capture);
    keys.connect_key_pressed({
        let (pane, search, cat_list, app_list, shown, host) = (
            pane.clone(),
            search.clone(),
            cat_list.clone(),
            app_list.clone(),
            shown.clone(),
            host.clone(),
        );
        move |_, keyval, _, state| {
            let Some(key) = translate(keyval, state) else {
                return glib::Propagation::Proceed;
            };
            match handle_key(pane.get(), search.text().is_empty(), key) {
                KeyAction::NoOp => return glib::Propagation::Proceed,
                KeyAction::Close => popdown(&search),
                KeyAction::Focus(p) => focus_pane(p, &pane, &search, &cat_list, &app_list),
                KeyAction::Move(delta) => {
                    let list = if pane.get() == Pane::Categories { &cat_list } else { &app_list };
                    move_selection(list, delta);
                }
                KeyAction::Launch => {
                    launch_selected(&app_list, &shown, &host);
                    popdown(&search);
                }
                KeyAction::TypeIntoSearch(c) => {
                    focus_pane(Pane::Search, &pane, &search, &cat_list, &app_list);
                    let mut text = search.text().to_string();
                    text.push(c);
                    search.set_text(&text);
                    search.set_position(-1);
                }
            }
            glib::Propagation::Stop
        }
    });
    root.add_controller(keys);

    // Open on the first category so the app pane is never blank.
    cat_list.select_row(cat_list.row_at_index(0).as_ref());

    let panes = GtkBox::new(Orientation::Horizontal, 6);
    panes.set_vexpand(true);
    panes.append(&scroll(&cat_list));
    panes.append(&scroll(&app_list));
    root.append(&search);
    root.append(&panes);
    root.upcast()
}

/// Resolve a favorites/recents id to a cached app. Accepts ids with or without the `.desktop` suffix.
fn find_app(apps: &[DesktopApp], id: &str) -> Option<DesktopApp> {
    let want = id.strip_suffix(".desktop").unwrap_or(id);
    apps.iter().find(|a| a.file_id == want).cloned()
}

/// Normalize a GTK key event to a `NavKey`, or `None` to let GTK handle it. Emits `Char` only for an
/// *unmodified* printable key, so compositor (Super) and stray Alt/Ctrl combos pass through.
fn translate(keyval: gdk::Key, state: gdk::ModifierType) -> Option<NavKey> {
    use gdk::Key;
    let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
    let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
    let other = state.intersects(gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK);
    match keyval {
        Key::Up => Some(NavKey::Up),
        Key::Down => Some(NavKey::Down),
        Key::Left => Some(NavKey::Left),
        Key::Right => Some(NavKey::Right),
        Key::Tab | Key::ISO_Left_Tab => Some(if shift { NavKey::ShiftTab } else { NavKey::Tab }),
        Key::Return | Key::KP_Enter => Some(NavKey::Enter),
        Key::Escape => Some(NavKey::Escape),
        _ if ctrl && keyval == Key::f => Some(NavKey::CtrlF),
        _ if !ctrl && !other => keyval.to_unicode().filter(|c| !c.is_control()).map(NavKey::Char),
        _ => None,
    }
}

/// Apply a `Focus` action: track it, grab GTK focus, and select the pane's first row if none yet.
fn focus_pane(p: Pane, pane: &Rc<Cell<Pane>>, search: &SearchEntry, cat: &ListBox, app: &ListBox) {
    pane.set(p);
    match p {
        Pane::Search => {
            search.grab_focus();
        }
        Pane::Categories => {
            if cat.selected_row().is_none() {
                cat.select_row(cat.row_at_index(0).as_ref());
            }
            cat.grab_focus();
        }
        Pane::Apps => {
            if app.selected_row().is_none() {
                app.select_row(app.row_at_index(0).as_ref());
            }
            app.grab_focus();
        }
    }
}

/// Move the selection in `list` by `delta`, clamped to the row range (no wrap).
fn move_selection(list: &ListBox, delta: i32) {
    let n = row_count(list);
    if n == 0 {
        return;
    }
    let cur = list.selected_row().map(|r| r.index()).unwrap_or(0);
    let next = (cur + delta).clamp(0, n - 1);
    list.select_row(list.row_at_index(next).as_ref());
}

fn row_count(list: &ListBox) -> i32 {
    let mut n = 0;
    let mut child = list.first_child();
    while let Some(w) = child {
        n += 1;
        child = w.next_sibling();
    }
    n
}

fn launch_selected(app_list: &ListBox, shown: &Rc<RefCell<Vec<DesktopApp>>>, host: &Rc<Host>) {
    let idx = app_list.selected_row().map(|r| r.index() as usize).unwrap_or(0);
    if let Some(app) = shown.borrow().get(idx).cloned() {
        launch(&app, host);
    }
}

fn popdown(widget: &impl IsA<gtk4::Widget>) {
    if let Some(pop) = widget.ancestor(Popover::static_type()).and_downcast::<Popover>() {
        pop.popdown();
    }
}

/// Apps across all sections matching the query, de-duplicated by id, name-sorted.
fn search_matches(sections: &[(String, Vec<DesktopApp>)], query_lower: &str) -> Vec<DesktopApp> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<DesktopApp> = sections
        .iter()
        .flat_map(|(_, apps)| apps.iter())
        .filter(|a| a.matches(query_lower) && seen.insert(a.file_id.clone()))
        .cloned()
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn populate_apps(app_list: &ListBox, shown: &Rc<RefCell<Vec<DesktopApp>>>, apps: &[DesktopApp], host: &Rc<Host>) {
    while let Some(child) = app_list.first_child() {
        app_list.remove(&child);
    }
    if apps.is_empty() {
        app_list.append(&dim_row("No results."));
    }
    for app in apps {
        app_list.append(&app_row(app, host));
    }
    *shown.borrow_mut() = apps.to_vec();
}

fn app_row(app: &DesktopApp, _host: &Rc<Host>) -> ListBoxRow {
    let line = GtkBox::new(Orientation::Horizontal, 8);
    line.set_margin_top(3);
    line.set_margin_bottom(3);
    line.set_margin_start(6);
    if let Some(icon) = &app.icon {
        let img = Image::from_icon_name(icon);
        img.set_pixel_size(24);
        line.append(&img);
    }
    let text = GtkBox::new(Orientation::Vertical, 0);
    let name = Label::new(Some(&app.name));
    name.set_xalign(0.0);
    text.append(&name);
    if let Some(gn) = &app.generic_name {
        text.append(&dim(gn));
    }
    line.append(&text);
    let row = ListBoxRow::new();
    row.set_child(Some(&line));
    row
}

fn launch(app: &DesktopApp, host: &Rc<Host>) {
    match app.launch(None, &[]) {
        Ok(intent) => {
            host.run_launch(&intent);
            // Record optimistically: the click was honoured; the executor logs any runtime failure.
            if let Err(e) = host.menu().borrow_mut().recents.record_launch(&app.file_id) {
                warn!(error = %e, "appmenu: could not persist recents");
            }
        }
        Err(e) => warn!(app = app.file_id, error = %e, "appmenu: launch produced no intent"),
    }
}

fn set_categories_enabled(cat_list: &ListBox, enabled: bool) {
    let mut child = cat_list.first_child();
    while let Some(row) = child {
        row.set_sensitive(enabled);
        child = row.next_sibling();
    }
}

fn scroll(child: &impl IsA<gtk4::Widget>) -> ScrolledWindow {
    ScrolledWindow::builder().child(child).vexpand(true).build()
}

fn text_row(text: &str) -> ListBoxRow {
    let label = Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_margin_top(6);
    label.set_margin_bottom(6);
    label.set_margin_start(8);
    label.set_margin_end(8);
    let row = ListBoxRow::new();
    row.set_child(Some(&label));
    row
}

fn dim(text: &str) -> Label {
    let l = Label::new(Some(text));
    l.set_xalign(0.0);
    l.add_css_class("dim-label");
    l
}

fn dim_row(text: &str) -> ListBoxRow {
    let row = ListBoxRow::new();
    row.set_child(Some(&dim(text)));
    row.set_selectable(false);
    row
}

#[cfg(test)]
mod tests {
    use super::{handle_key, KeyAction as A, NavKey as K, Pane};

    #[test]
    fn keyboard_nav_transitions() {
        // Escape / Ctrl+F from any pane.
        assert_eq!(handle_key(Pane::Apps, false, K::Escape), A::Close);
        assert_eq!(handle_key(Pane::Categories, true, K::CtrlF), A::Focus(Pane::Search));

        // Search: Down dives into results; Up/native-insert are NoOp; Enter launches only with text.
        assert_eq!(handle_key(Pane::Search, false, K::Down), A::Focus(Pane::Apps));
        assert_eq!(handle_key(Pane::Search, false, K::Up), A::NoOp);
        assert_eq!(handle_key(Pane::Search, false, K::Char('a')), A::NoOp);
        assert_eq!(handle_key(Pane::Search, true, K::Enter), A::NoOp);
        assert_eq!(handle_key(Pane::Search, false, K::Enter), A::Launch);

        // Type-to-search from the panes.
        assert_eq!(handle_key(Pane::Categories, true, K::Char('x')), A::TypeIntoSearch('x'));
        assert_eq!(handle_key(Pane::Apps, true, K::Char('z')), A::TypeIntoSearch('z'));

        // Categories: Up/Down move; Right/Enter dive to apps; Left does nothing.
        assert_eq!(handle_key(Pane::Categories, true, K::Up), A::Move(-1));
        assert_eq!(handle_key(Pane::Categories, true, K::Down), A::Move(1));
        assert_eq!(handle_key(Pane::Categories, true, K::Right), A::Focus(Pane::Apps));
        assert_eq!(handle_key(Pane::Categories, true, K::Enter), A::Focus(Pane::Apps));
        assert_eq!(handle_key(Pane::Categories, true, K::Left), A::NoOp);

        // Apps: Up/Down move; Left back to categories; Right nothing; Enter launches (default sel).
        assert_eq!(handle_key(Pane::Apps, true, K::Up), A::Move(-1));
        assert_eq!(handle_key(Pane::Apps, true, K::Down), A::Move(1));
        assert_eq!(handle_key(Pane::Apps, true, K::Left), A::Focus(Pane::Categories));
        assert_eq!(handle_key(Pane::Apps, true, K::Right), A::NoOp);
        assert_eq!(handle_key(Pane::Apps, true, K::Enter), A::Launch);

        // Tab cycles all three regions; Shift+Tab reverses.
        assert_eq!(handle_key(Pane::Search, true, K::Tab), A::Focus(Pane::Categories));
        assert_eq!(handle_key(Pane::Categories, true, K::Tab), A::Focus(Pane::Apps));
        assert_eq!(handle_key(Pane::Apps, true, K::Tab), A::Focus(Pane::Search));
        assert_eq!(handle_key(Pane::Apps, true, K::ShiftTab), A::Focus(Pane::Categories));
        assert_eq!(handle_key(Pane::Search, true, K::ShiftTab), A::Focus(Pane::Apps));
    }
}
