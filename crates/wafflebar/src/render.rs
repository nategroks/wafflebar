//! The host side of the `Plugin` contract: render a [`View`] to GTK widgets, own the widget
//! tree, and route user actions back to modules.
//!
//! Nothing here leaks GTK back into the module layer — modules only ever produce `View`s and
//! receive `Event`s/`ActionId`s. See `docs/ARCHITECTURE.md` (v1→v2 isolation).

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{gdk, GestureClick, Orientation, Popover, Separator};
use tracing::debug;
use wafflebar_core::{
    ActionId, Event, Launch, MenuItem, Plugin, Reaction, SeparatorStyle, Topic, View, WmCommand,
};

/// One placed module: its kind (for logging), the boxed reducer, and the host-owned container
/// widget whose child is rebuilt on every dirty update.
pub struct PluginSlot {
    pub kind: String,
    pub module: Box<dyn Plugin>,
    pub container: gtk4::Box,
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

    /// Rebuild a slot's widget subtree from its current `View`.
    fn rerender(self: &Rc<Self>, slot: usize) {
        // Snapshot the view + container handle, then drop the borrow before building widgets.
        let (view, container, kind) = {
            let slots = self.slots.borrow();
            if slot >= slots.len() {
                return;
            }
            (
                slots[slot].module.view(),
                slots[slot].container.clone(),
                slots[slot].kind.clone(),
            )
        };
        debug!(slot, kind, "rerender module subtree");
        // PERF: full subtree rebuild on every dirty update. Fine for v1 (per-module, low rate);
        // replace with a keyed diff before a >10Hz module lands (M3 CPU graph is the first).
        while let Some(child) = container.first_child() {
            container.remove(&child);
        }
        container.append(&render_view(&view, slot, self));
    }
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
        View::Button { child, action, classes, menu } => {
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
            return; // no display — nothing to assert against
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
