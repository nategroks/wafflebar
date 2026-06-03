//! Plugin registry: maps a config `type` to a boxed [`Plugin`].
//!
//! Modules are GTK-free reducers (see `docs/ARCHITECTURE.md`); the host renders their `View`.
//! Unknown types fall back to a [`Placeholder`] module that renders a dim label — *not* a
//! special code path, just another `Plugin`.

pub mod appmenu;
pub mod bluetooth;
pub mod clock;
pub mod cpu;
pub mod dwl;
pub mod feedblocks;
pub mod launcher;
pub mod memory;
pub mod network;
pub mod power;
pub mod separator;
pub mod showdesktop;
pub mod statustray;
pub mod tasklist;
pub mod volume;

use wafflebar_core::{ActionId, ConfigField, Event, ModuleConfig, Plugin, Reaction, Topic, View};

/// Backend capabilities handed to plugins at construction — things a plugin can't derive from its
/// own config and that depend on which compositor is running (e.g. whether show-desktop works).
#[derive(Debug, Clone, Copy, Default)]
pub struct Caps {
    pub show_desktop: bool,
}

/// Construct a module for `kind`, bound to `output` (the bar's monitor), `cfg`, and backend `caps`.
pub fn build(kind: &str, output: &str, cfg: &ModuleConfig, caps: &Caps) -> Box<dyn Plugin> {
    match kind {
        "appmenu" => Box::new(appmenu::AppMenu::new(cfg)),
        "clock" => Box::new(clock::Clock::new(cfg)),
        "tags" => Box::new(dwl::tags::Tags::new(output)),
        "layout" => Box::new(dwl::layout::Layout::new(output)),
        "window" => Box::new(dwl::window::Window::new(output, dwl::window::read_max_chars(cfg))),
        "tasklist" => Box::new(tasklist::Tasklist::new(output, tasklist::read_max_chars(cfg))),
        "launcher" => Box::new(launcher::Launcher::new(cfg)),
        "separator" => Box::new(separator::Separator::new(cfg)),
        "showdesktop" => Box::new(showdesktop::ShowDesktop::new(caps)),
        "volume" => Box::new(volume::Volume::new()),
        "network" => Box::new(network::Network::new(network::read_max_chars(cfg))),
        "memory" => Box::new(memory::Memory::new()),
        "cpu" => Box::new(cpu::Cpu::new()),
        "statustray" => Box::new(statustray::StatusTray::new()),
        "bluetooth" => Box::new(bluetooth::Bluetooth::new()),
        "power" => Box::new(power::Power::new(cfg)),
        "feedblocks" => Box::new(feedblocks::FeedBlocks::new(cfg)),
        other => Box::new(Placeholder::new(other)),
    }
}

/// Static metadata for a plugin kind, for the Add Items dialog (F4). Lives in the binary because the
/// compiled-in plugin set is known at build time — plugins describe what they *are* statically;
/// instances are dynamic. (Metadata is binary-time, not runtime — so it's not a `Plugin` trait
/// method.)
pub struct PluginInfo {
    pub kind: &'static str,
    pub name: &'static str,
    /// Shown in the dialog. Backend-class kinds carry their F2c "needs a restart" caveat here so the
    /// limitation is visible at the point of adding (the chosen shape over a confirmation dialog).
    pub description: &'static str,
    /// A freedesktop symbolic icon name; the picker falls back to a generic if it doesn't resolve.
    pub icon: &'static str,
    /// `true` for a kind that must not have a second instance. Currently only `statustray`, which
    /// owns the SNI Watcher bus name — a second instance fails to acquire it and renders empty
    /// (silent failure, so we prevent it). The per-output plugins (tags/window/showdesktop) are
    /// *not* unique: a second instance on a different output is legitimate. TODO(prefs): a
    /// `Uniqueness::PerOutput` scope if same-output duplicates prove confusing.
    pub unique: bool,
}

/// Every addable plugin kind, in catalog order (what Add Items lists).
pub fn catalog() -> Vec<PluginInfo> {
    let info =
        |kind, name, description, icon, unique| PluginInfo { kind, name, description, icon, unique };
    vec![
        info("appmenu", "Applications", "A searchable menu of installed applications.", "view-app-grid-symbolic", false),
        info("clock", "Clock", "Date and time.", "preferences-system-time-symbolic", false),
        info("launcher", "Launcher", "Pinned application shortcuts.", "applications-other-symbolic", false),
        info("separator", "Separator", "Blank space, a line, or a grip; can expand to push items apart.", "view-list-symbolic", false),
        info("tasklist", "Task list", "A button per open window.", "view-list-symbolic", false),
        info("tags", "Tags", "Workspace / tag indicator (dwl).", "view-grid-symbolic", false),
        info("layout", "Layout symbol", "Current tiling layout symbol (dwl).", "view-grid-symbolic", false),
        info("window", "Window title", "Title of the focused window.", "window-new-symbolic", false),
        info("showdesktop", "Show desktop", "Minimize all windows (where the compositor supports it).", "user-desktop-symbolic", false),
        info("memory", "Memory", "RAM usage.", "utilities-system-monitor-symbolic", false),
        info("cpu", "CPU", "Processor load.", "utilities-system-monitor-symbolic", false),
        info("volume", "Volume", "Audio volume. Starting its backend needs a restart.", "audio-volume-medium-symbolic", false),
        info("network", "Network", "Connection status. Starting its backend needs a restart.", "network-wireless-symbolic", false),
        info("statustray", "System tray", "Status-notifier icons from running apps.", "preferences-system-notifications-symbolic", true),
        info("bluetooth", "Bluetooth", "Adapter power + paired devices. Starting its backend needs a restart.", "bluetooth-symbolic", true),
        info("power", "Power", "Sign out, hibernate, reboot, or shut down.", "system-shutdown-symbolic", false),
        info("feedblocks", "Feed blocks", "Render someblocks-style external status blocks (natewm-mode channel 2).", "view-list-symbolic", true),
    ]
}

/// The config schema a module kind exposes to the preferences UI (F3). Builds a throwaway reducer
/// with empty options — schemas are static (they describe fields, not values), so output/caps and
/// the absent values don't affect the result — and returns its declared fields. Unknown kinds and
/// option-less modules yield no fields.
pub fn config_schema(kind: &str) -> Vec<ConfigField> {
    build(kind, "", &ModuleConfig::bare(kind), &Caps::default()).config_schema()
}

/// Build a `ModuleConfig` with the given options for tests (the `configure` migration tests).
#[cfg(test)]
pub(crate) fn test_module_config(options: &[(&str, toml::Value)]) -> ModuleConfig {
    use wafflebar_core::Cell;
    ModuleConfig {
        kind: "test".into(),
        cell: Cell { row: 0, col: 0, rowspan: 1, colspan: 1 },
        align: Default::default(),
        options: options.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
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

#[cfg(test)]
mod schema_tests {
    use super::config_schema;
    use wafflebar_core::FieldKind;

    #[test]
    fn schemas_describe_known_option_keys() {
        let clock = config_schema("clock");
        assert_eq!(clock[0].key, "format");
        assert!(matches!(clock[0].kind, FieldKind::Text { .. }));
        assert!(clock
            .iter()
            .any(|f| f.key == "timezone" && matches!(f.kind, FieldKind::Text { .. })));
        assert!(clock
            .iter()
            .any(|f| f.key == "show_week_numbers" && matches!(f.kind, FieldKind::Bool { .. })));

        assert!(config_schema("separator")
            .iter()
            .any(|f| f.key == "style" && matches!(f.kind, FieldKind::Choice { .. })));
        assert!(config_schema("separator")
            .iter()
            .any(|f| f.key == "expand" && matches!(f.kind, FieldKind::Bool { .. })));
        assert!(config_schema("memory")
            .iter()
            .any(|f| f.key == "interval" && matches!(f.kind, FieldKind::Int { .. })));
        // Launcher's items render as the F4 application-list editor.
        assert!(matches!(config_schema("launcher")[0].kind, FieldKind::DesktopList));

        // Option-less and unknown kinds expose nothing.
        assert!(config_schema("volume").is_empty());
        assert!(config_schema("nonexistent").is_empty());
    }
}
