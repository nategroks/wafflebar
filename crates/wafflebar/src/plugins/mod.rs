//! Plugin registry: maps a config `type` to a boxed [`Plugin`].
//!
//! Modules are GTK-free reducers (see `docs/ARCHITECTURE.md`); the host renders their `View`.
//! Unknown types fall back to a [`Placeholder`] module that renders a dim label — *not* a
//! special code path, just another `Plugin`.

pub mod clock;
pub mod dwl;
pub mod launcher;
pub mod separator;
pub mod showdesktop;
pub mod tasklist;
pub mod volume;

use wafflebar_core::{ActionId, Event, ModuleConfig, Plugin, Reaction, Topic, View};

/// Backend capabilities handed to plugins at construction — things a plugin can't derive from its
/// own config and that depend on which compositor is running (e.g. whether show-desktop works).
#[derive(Debug, Clone, Copy, Default)]
pub struct Caps {
    pub show_desktop: bool,
}

/// Construct a module for `kind`, bound to `output` (the bar's monitor), `cfg`, and backend `caps`.
pub fn build(kind: &str, output: &str, cfg: &ModuleConfig, caps: &Caps) -> Box<dyn Plugin> {
    match kind {
        "clock" => Box::new(clock::Clock::new(cfg)),
        "tags" => Box::new(dwl::tags::Tags::new(output)),
        "window" => Box::new(dwl::window::Window::new(
            output,
            cfg.opt_i64("max_chars").unwrap_or(0).max(0) as usize,
        )),
        "tasklist" => Box::new(tasklist::Tasklist::new(
            output,
            cfg.opt_i64("max_chars").unwrap_or(0).max(0) as usize,
        )),
        "launcher" => Box::new(launcher::Launcher::new(cfg)),
        "separator" => Box::new(separator::Separator::new(cfg)),
        "showdesktop" => Box::new(showdesktop::ShowDesktop::new(caps)),
        "volume" => Box::new(volume::Volume::new()),
        other => Box::new(Placeholder::new(other)),
    }
}

/// A dim label for not-yet-implemented module kinds.
pub struct Placeholder {
    kind: String,
}

impl Placeholder {
    pub fn new(kind: &str) -> Self {
        Self {
            kind: kind.to_string(),
        }
    }
}

impl Plugin for Placeholder {
    fn id(&self) -> &str {
        &self.kind
    }
    fn subscribe(&self) -> Vec<Topic> {
        Vec::new()
    }
    fn view(&self) -> View {
        View::label(self.kind.clone())
            .with_class("module")
            .with_class("placeholder")
    }
    fn on_event(&mut self, _ev: &Event) -> Reaction {
        Reaction::none()
    }
    fn on_action(&mut self, _action: &ActionId) -> Reaction {
        Reaction::none()
    }
}
