//! A scriptable [`WindowManager`] for tests.
//!
//! Lets module and host logic be exercised without a live compositor. Setters mutate the fake's
//! state **and return the [`WmEvent`] a real backend would have emitted**, so a test reads like:
//!
//! ```
//! use wafflebar_core::fake::FakeWm;
//! use wafflebar_core::wm::WindowManager;
//! let mut wm = FakeWm::new("DP-1", 9);
//! let ev = wm.set_active_tag(2);            // tag index 2 becomes active
//! // feed `ev` to a module's on_event, then assert its View …
//! assert!(wm.snapshot().len() >= 1);
//! ```
//!
//! Executed commands are recorded so click-handlers can be asserted.

use std::cell::RefCell;
use std::rc::Rc;

use crate::wm::{Output, Tag, TagState, Window, WindowId, WindowManager, WmCommand, WmEvent};

/// A fake, fully scriptable window manager.
#[derive(Debug, Clone)]
pub struct FakeWm {
    output: String,
    tags: Vec<Tag>,
    layout: String,
    active_title: String,
    active_appid: String,
    windows: Vec<Window>,
    executed: Rc<RefCell<Vec<WmCommand>>>,
}

impl FakeWm {
    /// A fake with `ntags` empty tags named "1".."N" on `output`.
    pub fn new(output: &str, ntags: u32) -> Self {
        let tags = (0..ntags)
            .map(|i| Tag {
                index: i,
                name: (i + 1).to_string(),
                state: TagState::None,
                focused: false,
                occupied: false,
            })
            .collect();
        Self {
            output: output.to_string(),
            tags,
            layout: "[]=".to_string(),
            active_title: String::new(),
            active_appid: String::new(),
            windows: Vec::new(),
            executed: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// Make tag `idx` the active/focused one (others become `None`/unfocused).
    pub fn set_active_tag(&mut self, idx: u32) -> WmEvent {
        for t in &mut self.tags {
            t.focused = t.index == idx;
            t.state = if t.index == idx {
                TagState::Active
            } else if t.state == TagState::Active {
                TagState::None
            } else {
                t.state
            };
        }
        self.tags_event()
    }

    /// Set whether tag `idx` is occupied.
    pub fn set_occupied(&mut self, idx: u32, occupied: bool) -> WmEvent {
        if let Some(t) = self.tags.iter_mut().find(|t| t.index == idx) {
            t.occupied = occupied;
        }
        self.tags_event()
    }

    /// Mark tag `idx` urgent.
    pub fn set_urgent(&mut self, idx: u32) -> WmEvent {
        if let Some(t) = self.tags.iter_mut().find(|t| t.index == idx) {
            t.state = TagState::Urgent;
        }
        self.tags_event()
    }

    /// Set the focused-window title/app_id.
    pub fn set_active_window(&mut self, title: &str, app_id: &str) -> WmEvent {
        self.active_title = title.to_string();
        self.active_appid = app_id.to_string();
        WmEvent::ActiveWindow {
            output: self.output.clone(),
            title: self.active_title.clone(),
            app_id: self.active_appid.clone(),
        }
    }

    /// Replace the whole window list.
    pub fn set_windows(&mut self, windows: Vec<Window>) -> WmEvent {
        self.windows = windows;
        self.windows_event()
    }

    /// Add a window; returns the updated `Windows` event.
    pub fn add_window(&mut self, w: Window) -> WmEvent {
        self.windows.push(w);
        self.windows_event()
    }

    /// Remove a window by id (simulates a foreign-toplevel `closed`).
    pub fn remove_window(&mut self, id: WindowId) -> WmEvent {
        self.windows.retain(|w| w.id != id);
        self.windows_event()
    }

    /// The commands `execute` has recorded, in order.
    pub fn executed(&self) -> Vec<WmCommand> {
        self.executed.borrow().clone()
    }

    /// The output this fake represents.
    pub fn output(&self) -> Output {
        Output {
            name: self.output.clone(),
            x: 0,
        }
    }

    fn tags_event(&self) -> WmEvent {
        WmEvent::Tags {
            output: self.output.clone(),
            tags: self.tags.clone(),
        }
    }

    fn windows_event(&self) -> WmEvent {
        WmEvent::Windows {
            windows: self.windows.clone(),
        }
    }
}

impl WindowManager for FakeWm {
    fn snapshot(&self) -> Vec<WmEvent> {
        vec![
            self.tags_event(),
            WmEvent::Layout {
                output: self.output.clone(),
                symbol: self.layout.clone(),
            },
            WmEvent::ActiveWindow {
                output: self.output.clone(),
                title: self.active_title.clone(),
                app_id: self.active_appid.clone(),
            },
            self.windows_event(),
        ]
    }

    fn execute(&mut self, cmd: &WmCommand) {
        self.executed.borrow_mut().push(cmd.clone());
    }
}
