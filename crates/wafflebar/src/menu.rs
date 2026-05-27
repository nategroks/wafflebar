//! Host-rendered applications menu (E2). The `appmenu` plugin emits a `View::AppMenu` marker; the
//! renderer calls [`build_appmenu`] to construct the actual two-pane search/category/app widget.
//!
//! This is host infrastructure, not a reducer: the menu's interactive state (search text, which
//! category/app is selected) lives in these GTK widgets, never in the plugin. The reducer-owned
//! config (favorites, recents settings) arrives via the marker; the app list + recents come from
//! the host-owned [`MenuState`] (refreshed by a directory watch in `app.rs`); launches go straight
//! to the host executor. See `View::AppMenu` and docs/UPSTREAM.md.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Image, Label, ListBox, ListBoxRow, Orientation, Popover, ScrolledWindow, SearchEntry};
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

    for (name, _) in sections.iter() {
        cat_list.append(&text_row(name));
    }

    // Category selection (only meaningful when not searching): show that category's apps.
    cat_list.connect_row_selected({
        let (sections, shown, app_list, host) = (sections.clone(), shown.clone(), app_list.clone(), host.clone());
        move |_, row| {
            if let Some(idx) = row.map(|r| r.index() as usize) {
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
