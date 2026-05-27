//! Notification popups (G1b): a layer-shell overlay (top-right) holding a stack of notification
//! widgets built from the [`Notification`]s the server (`notify.rs`) delivers.
//!
//! Host-rendered, like the menu/prefs — the data is GTK-free `core::notification`, the widgets live
//! here. The surface stays mapped but hidden when empty (sized to its contents, so it covers only
//! the notification area and never eats clicks elsewhere). Each notification gets a per-id expiry
//! timer (the TimerSet discipline from F2b); critical urgency never auto-expires. Beyond a visible
//! cap, notifications queue (FIFO) and surface as slots free — never dropped silently. User actions
//! and dismissals route back to the server, which emits `ActionInvoked`/`NotificationClosed`.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Align, Application, ApplicationWindow, Box as GtkBox, Button, GestureClick, Image, Label, Orientation};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use wafflebar_core::{CloseReason, Notification, Timeout, Urgency};

use crate::notify::NotifyServer;

const WIDTH: i32 = 360;
const CAP: usize = 5; // visible at once; overflow queues
const HARD_MAX: usize = 10; // critical may exceed CAP up to here
const MARGIN: i32 = 16;
const GAP: i32 = 6;
const DEFAULT_TIMEOUT_MS: u32 = 5000; // expire_timeout == -1 (server default)

/// The popup stack. Held in `Rc<RefCell<…>>`; event handlers capture a `Weak` to avoid pinning it.
pub struct NotificationStack {
    surface: ApplicationWindow,
    list: GtkBox,
    server: Rc<NotifyServer>,
    visible: Vec<u32>,
    rows: HashMap<u32, GtkBox>,
    timers: HashMap<u32, glib::SourceId>,
    queue: VecDeque<Notification>,
}

impl NotificationStack {
    /// Build the overlay surface (top-right, no keyboard focus, hidden until a notification arrives).
    pub fn new(app: &Application, server: Rc<NotifyServer>) -> Rc<RefCell<Self>> {
        let list = GtkBox::new(Orientation::Vertical, GAP);
        list.add_css_class("notifications");
        let surface = ApplicationWindow::builder().application(app).build();
        surface.init_layer_shell();
        surface.set_layer(Layer::Overlay);
        surface.set_namespace(Some("wafflebar-notifications"));
        surface.set_keyboard_mode(KeyboardMode::None);
        surface.set_anchor(Edge::Top, true);
        surface.set_anchor(Edge::Right, true);
        surface.set_margin(Edge::Top, MARGIN);
        surface.set_margin(Edge::Right, MARGIN);
        surface.set_default_width(WIDTH);
        surface.set_child(Some(&list));
        // Mapped-but-hidden when empty: sized to contents, so it only ever covers the stack region.
        surface.set_visible(false);

        Rc::new(RefCell::new(Self {
            surface,
            list,
            server,
            visible: Vec::new(),
            rows: HashMap::new(),
            timers: HashMap::new(),
            queue: VecDeque::new(),
        }))
    }

    /// A new (or replacing) notification from the server.
    pub fn post(stack: &Rc<RefCell<Self>>, n: Notification) {
        if stack.borrow().rows.contains_key(&n.id) {
            return Self::replace_in_place(stack, n); // replaces_id: update existing, keep position
        }
        let (visible, critical) = {
            let s = stack.borrow();
            (s.visible.len(), n.urgency == Urgency::Critical)
        };
        // Critical bypasses the cap (up to a hard max) so urgent alerts aren't stuck behind a queue.
        if visible < CAP || (critical && visible < HARD_MAX) {
            Self::show(stack, n);
        } else {
            stack.borrow_mut().queue.push_back(n);
        }
    }

    /// `CloseNotification` from a client — the server already emitted `NotificationClosed`, so just
    /// drop the popup.
    pub fn close_external(stack: &Rc<RefCell<Self>>, id: u32) {
        Self::remove(stack, id, None);
    }

    fn show(stack: &Rc<RefCell<Self>>, n: Notification) {
        let row = GtkBox::new(Orientation::Vertical, 4);
        row.set_size_request(WIDTH, -1);
        populate_row(&row, &n, stack);
        let id = n.id;
        {
            let mut s = stack.borrow_mut();
            s.list.append(&row);
            s.rows.insert(id, row);
            s.visible.push(id);
        }
        Self::schedule(stack, id, n.urgency, n.timeout);
        Self::update_visibility(stack);
    }

    fn replace_in_place(stack: &Rc<RefCell<Self>>, n: Notification) {
        let row = stack.borrow().rows.get(&n.id).cloned();
        let Some(row) = row else { return };
        while let Some(child) = row.first_child() {
            row.remove(&child);
        }
        populate_row(&row, &n, stack); // same widget → same stack position, no flicker
        if let Some(t) = stack.borrow_mut().timers.remove(&n.id) {
            t.remove();
        }
        Self::schedule(stack, n.id, n.urgency, n.timeout);
    }

    /// Schedule the auto-expiry timer (critical / `Never` get none).
    fn schedule(stack: &Rc<RefCell<Self>>, id: u32, urgency: Urgency, timeout: Timeout) {
        let ms = match (urgency, timeout) {
            (Urgency::Critical, _) => return, // critical never auto-expires, even with a timeout set
            (_, Timeout::Never) => return,
            (_, Timeout::Default) => DEFAULT_TIMEOUT_MS,
            (_, Timeout::Millis(m)) => m,
        };
        let weak = Rc::downgrade(stack);
        let source = glib::timeout_add_local_once(Duration::from_millis(ms as u64), move || {
            if let Some(stack) = weak.upgrade() {
                Self::remove(&stack, id, Some(CloseReason::Expired));
            }
        });
        stack.borrow_mut().timers.insert(id, source);
    }

    fn remove(stack: &Rc<RefCell<Self>>, id: u32, emit: Option<CloseReason>) {
        let (removed, server) = {
            let mut s = stack.borrow_mut();
            if let Some(t) = s.timers.remove(&id) {
                t.remove();
            }
            let removed = s.rows.remove(&id);
            if let Some(row) = &removed {
                s.list.remove(row);
            }
            s.visible.retain(|&x| x != id);
            (removed.is_some(), s.server.clone())
        };
        if !removed {
            return;
        }
        if let Some(reason) = emit {
            server.emit_closed(id, reason);
        }
        Self::update_visibility(stack);
        Self::drain(stack);
    }

    /// Promote queued notifications into freed visible slots (FIFO).
    fn drain(stack: &Rc<RefCell<Self>>) {
        loop {
            let next = {
                let mut s = stack.borrow_mut();
                if s.visible.len() >= CAP {
                    break;
                }
                s.queue.pop_front()
            };
            match next {
                Some(n) => Self::show(stack, n),
                None => break,
            }
        }
    }

    fn update_visibility(stack: &Rc<RefCell<Self>>) {
        let s = stack.borrow();
        s.surface.set_visible(!s.visible.is_empty());
    }
}

/// Fill `row` with a notification's widgets (header: image + summary/body + close; action buttons).
/// Handlers capture a `Weak` to the stack and route dismiss/action back through it. Shared by
/// initial show and in-place replace.
fn populate_row(row: &GtkBox, n: &Notification, stack: &Rc<RefCell<NotificationStack>>) {
    row.add_css_class("notification");
    row.add_css_class(match n.urgency {
        Urgency::Low => "notification-low",
        Urgency::Normal => "notification-normal",
        Urgency::Critical => "notification-critical",
    });

    let header = GtkBox::new(Orientation::Horizontal, 8);
    let image = resolve_image(n);
    image.set_pixel_size(40);
    image.set_valign(Align::Start);
    header.append(&image);

    let text = GtkBox::new(Orientation::Vertical, 2);
    text.set_hexpand(true);
    let summary = Label::new(Some(&n.summary));
    summary.set_xalign(0.0);
    summary.set_wrap(true);
    summary.add_css_class("notification-summary");
    text.append(&summary);
    if !n.body.is_empty() {
        let body = Label::new(None);
        body.set_xalign(0.0);
        body.set_wrap(true);
        body.set_markup(&n.body); // spec body is Pango-subset markup; malformed renders harmlessly
        body.add_css_class("notification-body");
        text.append(&body);
    }
    header.append(&text);

    let close = Button::from_icon_name("window-close-symbolic");
    close.add_css_class("notification-close");
    close.set_valign(Align::Start);
    wire(&close, stack, n.id, move |stack, id| {
        NotificationStack::remove(stack, id, Some(CloseReason::Dismissed))
    });
    header.append(&close);
    row.append(&header);

    // Default action fires on body click; other actions render as buttons.
    let mut buttons = Vec::new();
    for (key, label) in &n.actions {
        if key == "default" {
            continue;
        }
        let button = Button::with_label(label);
        let key = key.clone();
        wire(&button, stack, n.id, move |stack, id| invoke(stack, id, &key));
        buttons.push(button);
    }
    if !buttons.is_empty() {
        let actions = GtkBox::new(Orientation::Horizontal, 6);
        actions.set_halign(Align::End);
        for b in buttons {
            actions.append(&b);
        }
        row.append(&actions);
    }
    if n.actions.iter().any(|(k, _)| k == "default") {
        let gesture = GestureClick::new();
        let (weak, id) = (Rc::downgrade(stack), n.id);
        gesture.connect_released(move |_, _, _, _| {
            if let Some(stack) = weak.upgrade() {
                invoke(&stack, id, "default");
            }
        });
        header.add_controller(gesture);
    }
}

/// Invoke an action: emit `ActionInvoked`, then close the notification (dismissed by the user).
fn invoke(stack: &Rc<RefCell<NotificationStack>>, id: u32, key: &str) {
    stack.borrow().server.emit_action(id, key);
    NotificationStack::remove(stack, id, Some(CloseReason::Dismissed));
}

/// Wire a button to a stack action, capturing a `Weak` so the handler never pins the stack.
fn wire(
    button: &Button,
    stack: &Rc<RefCell<NotificationStack>>,
    id: u32,
    action: impl Fn(&Rc<RefCell<NotificationStack>>, u32) + 'static,
) {
    let weak = Rc::downgrade(stack);
    button.connect_clicked(move |_| {
        if let Some(stack) = weak.upgrade() {
            action(&stack, id);
        }
    });
}

/// Resolve the popup image: `image-path` hint, then `app_icon` (icon name or absolute path), then a
/// generic fallback. (The raw `image-data` hint is a TODO.)
fn resolve_image(n: &Notification) -> Image {
    if let Some(path) = &n.image_path {
        let path = path.strip_prefix("file://").unwrap_or(path);
        return Image::from_file(path);
    }
    if !n.app_icon.is_empty() {
        if n.app_icon.starts_with('/') {
            return Image::from_file(&n.app_icon);
        }
        return Image::from_icon_name(&n.app_icon);
    }
    Image::from_icon_name("dialog-information-symbolic")
}
