//! The host side of the `Plugin` contract: render a [`View`] to GTK widgets, own the widget
//! tree, and route user actions back to modules.
//!
//! Nothing here leaks GTK back into the module layer — modules only ever produce `View`s and
//! receive `Event`s/`ActionId`s. See `docs/ARCHITECTURE.md` (v1→v2 isolation).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{gdk, GestureClick, Orientation, Popover, Separator};
use tracing::debug;
use wafflebar_core::reconcile::child_key;
use wafflebar_core::{
    diff_children, ActionId, ChildPatch, Event, Launch, ListPatch, MenuItem, Plugin, Position,
    Reaction, SeparatorStyle, Topic, TrayCommand, View, VolumeCommand, WmCommand,
};

/// One placed module: its kind (for logging), the boxed reducer, and the host-owned container
/// widget whose child is rebuilt on every dirty update.
pub struct PluginSlot {
    pub kind: String,
    pub module: Box<dyn Plugin>,
    pub container: gtk4::Box,
    /// The last `View` rendered into `container`, kept so the next update can be reconciled against
    /// it (keyed diff) instead of rebuilt wholesale. `None` before the first render.
    pub last_view: Option<View>,
}

/// Owns all module slots and the command sink, and drives rendering + action routing.
pub struct Host {
    slots: RefCell<Vec<PluginSlot>>,
    /// Where `WmCommand`s go (wired by the binary to the active backend's `execute`).
    command_sink: Box<dyn Fn(&WmCommand)>,
    /// Where `Launch` intents go (wired to the shell executor: DBus activation or spawn).
    launch_sink: Box<dyn Fn(&Launch)>,
    /// Where `VolumeCommand`s go (wired to the audio backend, or a no-op when none is running).
    volume_sink: Box<dyn Fn(&VolumeCommand)>,
    /// Where `TrayCommand`s go (wired to the SNI backend, or a no-op when none is running).
    tray_sink: Box<dyn Fn(&TrayCommand)>,
    /// The bar's edge. Popovers open away from it (read at render time so a Phase F edge change is
    /// picked up without a stale cached anchor). `Cell` so a live `[bar]` reposition (F2c) can update
    /// it without rebuilding the host.
    position: std::cell::Cell<Position>,
    /// Effective icon pixel size for bar glyphs (resolved from `[bar] icon_size`/`height` via
    /// `BarConfig::effective_icon_size`). Applied to every `View::Icon` at render time so icons track
    /// the bar's thickness. `Cell` so a live `[bar]` reload (F2c) updates it without rebuilding.
    icon_size: std::cell::Cell<u32>,
    /// Host-owned application data for the applications menu (E2): the app cache + recents. Empty
    /// unless an `appmenu` plugin is present and `app.rs` populates it. The menu widget reads it at
    /// render time; the directory watch refreshes it and re-renders.
    menu: Rc<RefCell<crate::menu::MenuState>>,
}

impl Host {
    pub fn new(
        slots: Vec<PluginSlot>,
        command_sink: Box<dyn Fn(&WmCommand)>,
        launch_sink: Box<dyn Fn(&Launch)>,
        volume_sink: Box<dyn Fn(&VolumeCommand)>,
        tray_sink: Box<dyn Fn(&TrayCommand)>,
        position: Position,
        icon_size: u32,
    ) -> Rc<Self> {
        Rc::new(Self {
            slots: RefCell::new(slots),
            command_sink,
            launch_sink,
            volume_sink,
            tray_sink,
            position: std::cell::Cell::new(position),
            icon_size: std::cell::Cell::new(icon_size),
            menu: Rc::new(RefCell::new(crate::menu::MenuState::default())),
        })
    }

    /// Update the bar edge after a live `[bar]` reposition (F2c), so popovers open the right way.
    pub fn set_position(&self, position: Position) {
        self.position.set(position);
    }

    /// The bar's current edge (used to anchor dropdowns above/below the bar).
    pub fn position(&self) -> Position {
        self.position.get()
    }

    /// The effective icon pixel size for bar glyphs (see the `icon_size` field).
    pub fn icon_size(&self) -> u32 {
        self.icon_size.get()
    }

    /// Update the effective icon size after a live `[bar]` reload; the caller re-renders.
    pub fn set_icon_size(&self, size: u32) {
        self.icon_size.set(size);
    }

    /// The host-owned applications-menu state (app cache + recents); shared, so `app.rs` can
    /// populate/refresh it and the menu widget can read + record launches into it.
    pub fn menu(&self) -> Rc<RefCell<crate::menu::MenuState>> {
        self.menu.clone()
    }

    /// Send a launch intent to the shell executor (used by the host-rendered applications menu,
    /// which launches directly rather than round-tripping through a reducer).
    pub fn run_launch(&self, intent: &Launch) {
        (self.launch_sink)(intent);
    }

    /// Distinct timer intervals (seconds) any module subscribes to.
    pub fn timer_intervals(&self) -> Vec<u32> {
        let mut secs: Vec<u32> = self
            .slots
            .borrow()
            .iter()
            .flat_map(|s| s.module.subscribe())
            .filter_map(|t| match t {
                Topic::Timer { secs } => Some(secs),
                _ => None,
            })
            .collect();
        secs.sort_unstable();
        secs.dedup();
        secs
    }

    /// Deliver an event to every module; re-render the ones that report dirty.
    pub fn deliver_event(self: &Rc<Self>, ev: &Event) {
        let n = self.slots.borrow().len();
        for i in 0..n {
            let reaction = {
                let mut slots = self.slots.borrow_mut();
                slots[i].module.on_event(ev)
            };
            self.apply(i, reaction);
        }
    }

    /// Route a user action (a button fired) to the owning module by its slot index.
    /// The slot index is captured in the widget's click closure at build time — never read
    /// from ambient "currently rendering" state (that would cross-wire concurrent modules).
    pub fn dispatch_action(self: &Rc<Self>, slot: usize, action: &ActionId) {
        let reaction = {
            let mut slots = self.slots.borrow_mut();
            if slot >= slots.len() {
                return;
            }
            slots[slot].module.on_action(action)
        };
        self.apply(slot, reaction);
    }

    /// Does any current slot subscribe to `topic`? Used by the structural rebuild to decide which
    /// backends/timers the new module set needs (F2b reconciliation).
    pub fn subscribes(&self, topic: &Topic) -> bool {
        self.slots
            .borrow()
            .iter()
            .any(|s| s.module.subscribe().contains(topic))
    }

    /// Swap the entire slot set in place (structural reload, F2b). The host itself — and all its
    /// wiring (sinks, fd watch, backends holding `Weak<Host>`) — survives, so only the reducers and
    /// their containers are replaced. The caller swaps the grid (which drops the old containers) and
    /// reconciles timers afterward.
    pub fn replace_slots(self: &Rc<Self>, new_slots: Vec<PluginSlot>) {
        {
            // Tear down outgoing plugins *before* they leave the vector, while each still owns its
            // `&mut self`. Every v1 plugin's `teardown` is a no-op — they hold no resources (the
            // pure-reducer invariant; live resources are host-level, see app.rs) — but the hook is
            // honored at a well-defined moment so a future resource-owning plugin gets clean cleanup
            // rather than relying on Drop order.
            let mut slots = self.slots.borrow_mut();
            for slot in slots.iter_mut() {
                slot.module.teardown();
            }
        }
        *self.slots.borrow_mut() = new_slots;
    }

    /// Deliver a live config edit to one slot's module (live reload) and apply its reaction.
    pub fn configure_slot(self: &Rc<Self>, slot: usize, cfg: &wafflebar_core::ModuleConfig) {
        let reaction = {
            let mut slots = self.slots.borrow_mut();
            if slot >= slots.len() {
                return;
            }
            slots[slot].module.configure(cfg)
        };
        self.apply(slot, reaction);
    }

    /// Render every slot's initial view.
    pub fn render_all(self: &Rc<Self>) {
        let n = self.slots.borrow().len();
        for i in 0..n {
            self.rerender(i);
        }
    }

    /// Force-rebuild any `appmenu` slots (E2). `View::AppMenu` is a static marker — its `View`
    /// doesn't change when the host app cache does — so the keyed diff would skip it; clearing
    /// `last_view` forces a full rebuild that reads the refreshed cache. Called by the directory
    /// watch after an install/uninstall reparse.
    pub fn refresh_appmenu(self: &Rc<Self>) {
        let slots: Vec<usize> = self
            .slots
            .borrow()
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == "appmenu")
            .map(|(i, _)| i)
            .collect();
        for i in slots {
            self.slots.borrow_mut()[i].last_view = None;
            self.rerender(i);
        }
    }

    fn apply(self: &Rc<Self>, slot: usize, reaction: Reaction) {
        for cmd in &reaction.commands {
            (self.command_sink)(cmd);
        }
        for argv in &reaction.spawn {
            spawn(argv);
        }
        for intent in &reaction.launch {
            (self.launch_sink)(intent);
        }
        for cmd in &reaction.volume {
            (self.volume_sink)(cmd);
        }
        for cmd in &reaction.tray {
            (self.tray_sink)(cmd);
        }
        if reaction.dirty {
            self.rerender(slot);
        }
    }

    /// Reconcile a slot's widget subtree against its new `View` via a keyed diff: only the nodes
    /// whose data actually changed are rebuilt; unchanged siblings keep their existing widgets.
    fn rerender(self: &Rc<Self>, slot: usize) {
        // Snapshot the new view, take the previous one, grab the container — then drop the borrow
        // before touching widgets (render closures re-enter the host, so we must not hold it).
        let (new_view, old_view, container, kind) = {
            let mut slots = self.slots.borrow_mut();
            if slot >= slots.len() {
                return;
            }
            let s = &mut slots[slot];
            (
                s.module.view(),
                s.last_view.take(),
                s.container.clone(),
                s.kind.clone(),
            )
        };

        // The slot container holds the single root view; model it as a one-element child list so
        // the same keyed diff handles both the root and every nested container.
        let old_slice = old_view.as_slice();
        let new_slice = std::slice::from_ref(&new_view);
        let patch = diff_children(old_slice, new_slice);
        let ops = patch.counts();
        debug!(
            slot, kind,
            creates = ops.creates, updates = ops.updates, destroys = ops.destroys,
            "reconcile"
        );
        apply_children(&container, old_slice, new_slice, &patch, slot, self);

        self.slots.borrow_mut()[slot].last_view = Some(new_view);
    }
}

/// Children of a container view (empty for non-containers) — the slice the keyed diff recurses on.
fn container_children(view: &View) -> &[View] {
    match view {
        View::Row { children, .. } | View::Col { children, .. } => children,
        _ => &[],
    }
}

/// Apply a [`ListPatch`] to `parent`'s children: reuse kept/recursed widgets (re-packed in the new
/// order), rebuild updated/created ones, and drop the rest. `old`/`new` are the child views the
/// patch was computed from; `parent`'s current children correspond 1:1 to `old`.
fn apply_children(
    parent: &gtk4::Box,
    old: &[View],
    new: &[View],
    patch: &ListPatch,
    slot: usize,
    host: &Rc<Host>,
) {
    // Collect the current widgets in order; they line up with `old`. Skip popups (e.g. the clock's
    // calendar popover, which is `set_parent`'d to the slot container): a popover is a child in the
    // widget tree but not a laid-out child, so the keyed diff must neither count nor remove it —
    // that's what lets a host-attached popover survive the per-tick re-render with its open state.
    let mut old_widgets = Vec::with_capacity(old.len());
    let mut cur = parent.first_child();
    while let Some(w) = cur {
        cur = w.next_sibling();
        if w.downcast_ref::<Popover>().is_none() {
            old_widgets.push(w);
        }
    }

    // If reality and our cached view ever disagree, fall back to a clean full rebuild rather than
    // mis-pairing widgets. (Shouldn't happen — rendering is the only mutator — but cheap insurance.)
    if old_widgets.len() != old.len() || patch.children.len() != new.len() {
        for w in &old_widgets {
            parent.remove(w);
        }
        for nv in new {
            parent.append(&render_view(nv, slot, host));
        }
        return;
    }

    // Detach everything (we keep refs in `old_widgets`), then re-append in the new order.
    for w in &old_widgets {
        parent.remove(w);
    }
    let by_key: HashMap<String, (gtk4::Widget, &View)> = old
        .iter()
        .enumerate()
        .map(|(i, v)| (child_key(v, i), (old_widgets[i].clone(), v)))
        .collect();

    for (i, nv) in new.iter().enumerate() {
        let nk = child_key(nv, i);
        match &patch.children[i] {
            ChildPatch::Keep => {
                parent.append(&by_key[&nk].0);
            }
            ChildPatch::Recurse(sub) => {
                let (w, ov) = &by_key[&nk];
                if let Some(b) = w.downcast_ref::<gtk4::Box>() {
                    apply_children(b, container_children(ov), container_children(nv), sub, slot, host);
                }
                parent.append(w);
            }
            ChildPatch::Update | ChildPatch::Create => {
                parent.append(&render_view(nv, slot, host));
            }
        }
    }
    // Widgets whose keys weren't re-appended are dropped when `old_widgets`/`by_key` fall out of
    // scope here — that's the "destroy" half of the diff.
}

/// Render a `View` to a GTK widget. `slot` is baked into any button closures so actions route
/// to the right module regardless of what's rendering elsewhere.
pub fn render_view(view: &View, slot: usize, host: &Rc<Host>) -> gtk4::Widget {
    match view {
        View::Empty => gtk4::Box::new(Orientation::Horizontal, 0).upcast(),
        View::Spacer => {
            let b = gtk4::Box::new(Orientation::Horizontal, 0);
            b.set_hexpand(true);
            b.upcast()
        }
        View::Label { text, classes } => {
            let l = gtk4::Label::new(Some(text));
            add_classes(&l, classes);
            l.upcast()
        }
        View::Icon { name, classes, pixmap, theme_path, .. } => {
            // The host's effective icon size (derived from `[bar] icon_size`/`height`) wins so every
            // bar glyph tracks the bar's thickness uniformly; the per-`View` `size` field is ignored
            // here (kept as a plugin hint / for non-bar contexts).
            let size = host.icon_size();
            let img = build_icon(name, size, theme_path.as_deref(), pixmap.as_ref());
            img.set_pixel_size(size as i32);
            add_classes(&img, classes);
            img.upcast()
        }
        View::Row { children, gap, classes } => {
            container(Orientation::Horizontal, *gap, children, classes, slot, host)
        }
        View::Col { children, gap, classes } => {
            container(Orientation::Vertical, *gap, children, classes, slot, host)
        }
        View::Button { child, action, classes, menu, key: _, scroll_up, scroll_down, action_middle } => {
            let b = gtk4::Box::new(Orientation::Horizontal, 0);
            b.append(&render_view(child, slot, host));
            add_classes(&b, classes);
            b.add_css_class("wb-button");
            // Left-click → primary action.
            let left = GestureClick::new();
            left.set_button(gdk::BUTTON_PRIMARY);
            {
                let host = host.clone();
                let action = action.clone();
                left.connect_released(move |_, _, _, _| host.dispatch_action(slot, &action));
            }
            b.add_controller(left);
            // Middle-click → secondary action (tray SecondaryActivate).
            if let Some(mid) = action_middle {
                let m = GestureClick::new();
                m.set_button(gdk::BUTTON_MIDDLE);
                let host = host.clone();
                let mid = mid.clone();
                m.connect_released(move |_, _, _, _| host.dispatch_action(slot, &mid));
                b.add_controller(m);
            }
            // Right-click → context menu popover (when the plugin supplied one).
            if !menu.is_empty() {
                let popover = build_menu(menu, slot, host);
                popover.set_parent(&b);
                let right = GestureClick::new();
                right.set_button(gdk::BUTTON_SECONDARY);
                right.connect_pressed(move |_, _, _, _| popover.popup());
                b.add_controller(right);
            }
            // Scroll → discrete up/down actions (volume wheel). The View boundary is discrete (one
            // action per tick); the renderer absorbs GTK4 smooth-scroll here so plugins never see
            // sub-tick deltas: touchpads/high-res wheels emit many fractional `dy`s, which we
            // accumulate and turn into whole-tick action dispatches. Direction is normalized by
            // GTK4 (device + natural-scroll already applied), so dy<0 is consistently "up".
            if scroll_up.is_some() || scroll_down.is_some() {
                let scroll = gtk4::EventControllerScroll::new(
                    gtk4::EventControllerScrollFlags::VERTICAL,
                );
                let host = host.clone();
                let up = scroll_up.clone();
                let down = scroll_down.clone();
                let acc = std::cell::Cell::new(0.0_f64); // Fn (not FnMut): interior mutability
                scroll.connect_scroll(move |_, _dx, dy| {
                    let mut total = acc.get() + dy;
                    while total <= -1.0 {
                        if let Some(a) = up.as_ref() {
                            host.dispatch_action(slot, a);
                        }
                        total += 1.0;
                    }
                    while total >= 1.0 {
                        if let Some(a) = down.as_ref() {
                            host.dispatch_action(slot, a);
                        }
                        total -= 1.0;
                    }
                    acc.set(total); // keep the sub-tick remainder for the next event
                    // v1 always consumes. TODO(scroll): expose a `handled` flag in the reaction so
                    // scroll can bubble on unhandled when nested scrollables (e.g. an
                    // applicationsmenu category list) ship.
                    gtk4::glib::Propagation::Stop
                });
                b.add_controller(scroll);
            }
            b.upcast()
        }
        View::Separator { style, expand } => {
            // The bar is horizontal in v1 (top/bottom), so a separator runs along the cross-axis
            // (vertical). `Line` uses a real gtk::Separator; the textured styles are CSS-painted
            // boxes. `expand` is plain GTK box hexpand — shared proportionally with other
            // expanding children (the "[left] | gap | [right]" idiom).
            let w: gtk4::Widget = match style {
                SeparatorStyle::Line => Separator::new(Orientation::Vertical).upcast(),
                _ => gtk4::Box::new(Orientation::Vertical, 0).upcast(),
            };
            w.add_css_class("wb-separator");
            w.add_css_class(match style {
                SeparatorStyle::Transparent => "transparent",
                SeparatorStyle::Line => "line",
                SeparatorStyle::Handle => "handle",
                SeparatorStyle::Dots => "dots",
            });
            w.set_hexpand(*expand);
            w
        }
        View::Popover { trigger, content, classes } => {
            // The apps-menu has a search box → it must be a standalone layer-shell dropdown (a
            // GtkPopover can't get keyboard on dwl). Everything else stays a normal popover.
            if matches!(**content, View::AppMenu { .. }) {
                build_appmenu_dropdown(trigger, content, slot, host)
            } else {
                build_popover(trigger, content, classes, slot, host).0
            }
        }
        View::AppMenu { favorites, show_recents, max_recents } => {
            crate::menu::build_appmenu(favorites, *show_recents, *max_recents, host)
        }
    }
}

/// The apps-menu opens as a standalone layer-shell **dropdown** (not a `GtkPopover`) so its search
/// box can receive keyboard on dwl. The trigger button's click builds the menu fresh in a dropdown
/// window each time; the menu closes the window on launch / Escape (see `crate::menu`).
fn build_appmenu_dropdown(trigger: &View, content: &View, slot: usize, host: &Rc<Host>) -> gtk4::Widget {
    let trigger_w = render_view(trigger, slot, host);
    let gesture = GestureClick::new();
    gesture.set_button(gdk::BUTTON_PRIMARY);
    let content = content.clone();
    let host = host.clone();
    gesture.connect_released(move |_, _, _, _| {
        // Toggle: a second click closes the open menu instead of stacking another grabbing surface.
        if crate::dropdown::is_open() {
            crate::dropdown::close_open();
            return;
        }
        let win = gtk4::Window::builder().default_width(420).default_height(520).build();
        crate::dropdown::panel(&win, host.position());
        win.set_child(Some(&render_view(&content, slot, &host)));
        win.present();
    });
    trigger_w.add_controller(gesture);
    trigger_w
}

/// Build a popover: the trigger widget (returned, with the popover parented to it) plus the
/// `gtk::Popover` itself (returned for tests). Clicking the trigger pops it up; GTK's autohide
/// (on by default) closes it — open/closed is GTK's concern, the reducer only owns the content.
/// The popover opens away from the bar edge (derived from the host's position at render time).
fn build_popover(
    trigger: &View,
    content: &View,
    classes: &[String],
    slot: usize,
    host: &Rc<Host>,
) -> (gtk4::Widget, Popover) {
    let trigger_w = render_view(trigger, slot, host);

    let popover = Popover::new();
    popover.set_child(Some(&render_view(content, slot, host)));
    popover.set_autohide(true);
    popover.set_position(match host.position.get() {
        Position::Top => gtk4::PositionType::Bottom, // top bar → open downward
        Position::Bottom => gtk4::PositionType::Top,  // bottom bar → open upward
    });
    add_classes(&popover, classes);
    popover.set_parent(&trigger_w);

    // Open on trigger click. Content/trigger actions still route via ActionId as usual.
    let gesture = GestureClick::new();
    gesture.set_button(gdk::BUTTON_PRIMARY);
    let pop = popover.clone();
    gesture.connect_released(move |_, _, _, _| pop.popup());
    trigger_w.add_controller(gesture);

    (trigger_w, popover)
}

fn container(
    orientation: Orientation,
    gap: u32,
    children: &[View],
    classes: &[String],
    slot: usize,
    host: &Rc<Host>,
) -> gtk4::Widget {
    let b = gtk4::Box::new(orientation, gap as i32);
    for child in children {
        b.append(&render_view(child, slot, host));
    }
    add_classes(&b, classes);
    b.upcast()
}

/// Build the right-click context-menu popover. Items keep the plugin's order (no re-sorting).
fn build_menu(menu: &[MenuItem], slot: usize, host: &Rc<Host>) -> Popover {
    let popover = Popover::new();
    let vbox = gtk4::Box::new(Orientation::Vertical, 0);
    vbox.add_css_class("wb-menu");
    for item in menu {
        match item {
            MenuItem::Separator => vbox.append(&Separator::new(Orientation::Horizontal)),
            MenuItem::Item { label, action } => {
                let btn = gtk4::Button::with_label(label);
                btn.add_css_class("flat");
                btn.add_css_class("wb-menu-item");
                let host = host.clone();
                let action = action.clone();
                let popover_ref = popover.clone();
                btn.connect_clicked(move |_| {
                    host.dispatch_action(slot, &action);
                    popover_ref.popdown();
                });
                vbox.append(&btn);
            }
        }
    }
    popover.set_child(Some(&vbox));
    popover
}

/// Resolve an icon: themed `name` first (scoped to `theme_path` if the item ships one), then the
/// raw `pixmap`, then GTK's placeholder for `name`.
fn build_icon(
    name: &str,
    size: u32,
    theme_path: Option<&str>,
    pixmap: Option<&wafflebar_core::Pixmap>,
) -> gtk4::Image {
    // A file path (PNG/SVG, optionally `~/`-relative) is loaded directly — lets a user point an
    // icon (e.g. the appmenu button) at a custom image instead of a theme name. The caller sets
    // `pixel_size`, so the image scales to the requested size.
    if let Some(path) = icon_file_path(name) {
        return gtk4::Image::from_file(path);
    }
    if !name.is_empty() {
        if let Some(path) = theme_path {
            if let Some(img) = scoped_icon(name, size, path) {
                return img;
            }
        } else if icon_in_theme(name) || pixmap.is_none() {
            // Default theme resolves it, or there's no pixmap to prefer → let GTK render the name
            // (placeholder if missing). Keeps plain icons (tags, clock, …) on the simple path.
            return gtk4::Image::from_icon_name(name);
        }
    }
    if let Some(px) = pixmap {
        return image_from_pixmap(px);
    }
    gtk4::Image::from_icon_name(name)
}

/// If `name` denotes an image *file* (contains `/`, or ends `.png`/`.svg`/`.jpg`/`.jpeg`), return
/// its expanded (`~/`) existing path; otherwise `None` (treat as a theme icon name).
fn icon_file_path(name: &str) -> Option<std::path::PathBuf> {
    let lower = name.to_ascii_lowercase();
    let looks_like_path = name.contains('/')
        || [".png", ".svg", ".jpg", ".jpeg"].iter().any(|e| lower.ends_with(e));
    if !looks_like_path {
        return None;
    }
    let path = match name.strip_prefix("~/") {
        Some(rest) => std::path::PathBuf::from(std::env::var_os("HOME")?).join(rest),
        None => std::path::PathBuf::from(name),
    };
    path.exists().then_some(path)
}

/// Whether `name` resolves in the current display's icon theme.
fn icon_in_theme(name: &str) -> bool {
    gdk::Display::default()
        .map(|d| gtk4::IconTheme::for_display(&d).has_icon(name))
        .unwrap_or(false)
}

/// Resolve `name` against a theme that includes the item's `IconThemePath`, **scoped to this call**:
/// a fresh `IconTheme` seeded from the display (so standard icons still resolve) with `path` added,
/// never the shared display theme — IconThemePath is per-item, and mutating the global theme would
/// let one item's path pollute another's resolution.
fn scoped_icon(name: &str, size: u32, path: &str) -> Option<gtk4::Image> {
    let theme = gtk4::IconTheme::new();
    if let Some(d) = gdk::Display::default() {
        theme.set_display(Some(&d)); // inherit the display's standard search dirs + theme name
    }
    theme.add_search_path(path);
    if !theme.has_icon(name) {
        return None;
    }
    let paintable = theme.lookup_icon(
        name,
        &[],
        size as i32,
        1,
        gtk4::TextDirection::None,
        gtk4::IconLookupFlags::empty(),
    );
    Some(gtk4::Image::from_paintable(Some(&paintable)))
}

/// Build a `gtk::Image` from a [`Pixmap`] (RGBA bytes → `GdkMemoryTexture`, a paintable). The Image
/// scales it to the requested pixel size at display time.
fn image_from_pixmap(px: &wafflebar_core::Pixmap) -> gtk4::Image {
    let bytes = gtk4::glib::Bytes::from(&px.rgba);
    let texture = gdk::MemoryTexture::new(
        px.width as i32,
        px.height as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &bytes,
        (px.width * 4) as usize, // stride: 4 bytes/pixel, no padding
    );
    gtk4::Image::from_paintable(Some(&texture))
}

fn add_classes(w: &impl IsA<gtk4::Widget>, classes: &[String]) {
    for c in classes {
        w.add_css_class(c);
    }
}

fn spawn(argv: &[String]) {
    if let Some((cmd, args)) = argv.split_first() {
        if let Err(e) = std::process::Command::new(cmd).args(args).spawn() {
            debug!(?argv, error = %e, "spawn failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    #[test]
    fn icon_file_path_detects_paths_not_theme_names() {
        // Theme names (incl. our wb-* symbolic glyphs) are never treated as files.
        assert_eq!(icon_file_path("wb-appmenu-symbolic"), None);
        assert_eq!(icon_file_path("network-wired"), None);
        // Path-like but missing → None (falls back to theme lookup).
        assert_eq!(icon_file_path("/no/such/icon.png"), None);
        // A real file is returned (path-like + exists).
        let f = std::env::temp_dir().join("wb-icon-test.png");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(icon_file_path(f.to_str().unwrap()), Some(f.clone()));
        std::fs::remove_file(&f).ok();
    }

    static INIT: Once = Once::new();
    /// Initialize GTK once; returns false when there's no display (CI without a session) so the
    /// widget tests self-skip rather than fail.
    fn gtk_ready() -> bool {
        INIT.call_once(|| {
            let _ = gtk4::init();
        });
        gtk4::is_initialized()
    }

    fn host() -> Rc<Host> {
        Host::new(
            Vec::new(),
            Box::new(|_: &WmCommand| {}),
            Box::new(|_: &Launch| {}),
            Box::new(|_: &VolumeCommand| {}),
            Box::new(|_: &TrayCommand| {}),
            Position::Top,
            18,
        )
    }

    fn sep(style: SeparatorStyle, expand: bool) -> View {
        View::Separator { style, expand }
    }

    // GTK widget creation is single-threaded (only the thread that called `gtk::init` may build
    // widgets), so all GTK-touching assertions live in ONE test — `cargo test` runs tests on
    // multiple threads, and separate GTK tests would race for the init thread and panic.
    #[test]
    fn renderer_gtk_behaviors() {
        if !gtk_ready() {
            eprintln!("renderer GTK test skipped: no GTK display");
            return; // self-skip rather than fail in a headless CI
        }
        let h = host();

        // --- Separator: `expand` maps to GTK hexpand (fills / shares space). ---
        assert!(render_view(&sep(SeparatorStyle::Line, true), 0, &h).hexpands());
        assert!(!render_view(&sep(SeparatorStyle::Transparent, false), 0, &h).hexpands());
        let row = gtk4::Box::new(Orientation::Horizontal, 0);
        let a = render_view(&sep(SeparatorStyle::Line, true), 0, &h);
        let b = render_view(&sep(SeparatorStyle::Line, true), 0, &h);
        row.append(&a);
        row.append(&b);
        assert!(a.hexpands() && b.hexpands(), "two expanders share the row");

        // --- Popover: built with autohide, content rendered + parented to the trigger. ---
        let (trigger_w, popover) =
            build_popover(&View::icon("x", 16), &View::label("menu item"), &[], 0, &h);
        assert!(popover.is_autohide(), "GTK owns open/close; autohide closes without the reducer");
        assert!(popover.child().is_some(), "content rendered through the same renderer");
        assert_eq!(popover.parent().as_ref(), Some(&trigger_w));
        assert_eq!(popover.position(), gtk4::PositionType::Bottom, "top bar → opens downward");
    }
}
