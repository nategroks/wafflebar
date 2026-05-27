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
    diff_children, ActionId, ChildPatch, Event, Launch, ListPatch, MenuItem, Plugin, Reaction,
    SeparatorStyle, Topic, View, WmCommand,
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
}

impl Host {
    pub fn new(
        slots: Vec<PluginSlot>,
        command_sink: Box<dyn Fn(&WmCommand)>,
        launch_sink: Box<dyn Fn(&Launch)>,
    ) -> Rc<Self> {
        Rc::new(Self {
            slots: RefCell::new(slots),
            command_sink,
            launch_sink,
        })
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

    /// Render every slot's initial view.
    pub fn render_all(self: &Rc<Self>) {
        let n = self.slots.borrow().len();
        for i in 0..n {
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
    // Collect the current widgets in order; they line up with `old`.
    let mut old_widgets = Vec::with_capacity(old.len());
    let mut cur = parent.first_child();
    while let Some(w) = cur {
        cur = w.next_sibling();
        old_widgets.push(w);
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
        View::Icon { name, size, classes } => {
            let img = gtk4::Image::from_icon_name(name);
            img.set_pixel_size(*size as i32);
            add_classes(&img, classes);
            img.upcast()
        }
        View::Row { children, gap, classes } => {
            container(Orientation::Horizontal, *gap, children, classes, slot, host)
        }
        View::Col { children, gap, classes } => {
            container(Orientation::Vertical, *gap, children, classes, slot, host)
        }
        View::Button { child, action, classes, menu, key: _ } => {
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
            // Right-click → context menu popover (when the plugin supplied one).
            if !menu.is_empty() {
                let popover = build_menu(menu, slot, host);
                popover.set_parent(&b);
                let right = GestureClick::new();
                right.set_button(gdk::BUTTON_SECONDARY);
                right.connect_pressed(move |_, _, _, _| popover.popup());
                b.add_controller(right);
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
        // v1: popover content is unused (no v1 module emits Popover); render the trigger.
        // TODO(M3): wrap in a gtk::Popover and render `content` on demand.
        View::Popover { trigger, .. } => render_view(trigger, slot, host),
    }
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
        Host::new(Vec::new(), Box::new(|_: &WmCommand| {}), Box::new(|_: &Launch| {}))
    }

    fn sep(style: SeparatorStyle, expand: bool) -> View {
        View::Separator { style, expand }
    }

    #[test]
    fn separator_expand_is_honored_by_renderer() {
        if !gtk_ready() {
            eprintln!("separator renderer test skipped: no GTK display");
            return; // self-skip rather than fail in a headless CI
        }
        let h = host();
        // expand flag maps to GTK hexpand (which is what makes it fill / share space).
        assert!(render_view(&sep(SeparatorStyle::Line, true), 0, &h).hexpands());
        assert!(!render_view(&sep(SeparatorStyle::Transparent, false), 0, &h).hexpands());

        // Two expanding separators in one row both hexpand → GTK shares the row proportionally.
        let row = gtk4::Box::new(Orientation::Horizontal, 0);
        let a = render_view(&sep(SeparatorStyle::Line, true), 0, &h);
        let b = render_view(&sep(SeparatorStyle::Line, true), 0, &h);
        row.append(&a);
        row.append(&b);
        assert!(a.hexpands() && b.hexpands());
    }
}
