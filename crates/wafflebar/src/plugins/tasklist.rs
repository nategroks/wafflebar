//! Tasklist plugin — the window list (xfce4-panel `plugins/tasklist/`).
//!
//! Mirrors xfce4-panel's tasklist *behavior* on our `Plugin` trait: a button per window
//! (icon + title), click to activate, per-output filtering, focused/minimized styling. It's a
//! pure reducer over `WmEvent::Windows` (WM-agnostic — works on any backend that fills the
//! window list, not just dwl).
//!
//! Deferred (each maps to an upstream function in `tasklist-widget.c`):
//! - grouping by application — `xfce_tasklist_group_button_new` / `set_grouping`
//! - sort order + stable ordering — `xfce_tasklist_button_compare`
//! - scroll-to-cycle windows — `xfce_tasklist_scroll_event`
//! - drag-to-reorder — handled in `xfce_tasklist_button_*` drag handlers
//! - icon-only vs label modes, wireframes — `set_show_labels` / `set_show_wireframes`
//! - right-click window menu (close/move/minimize) — `xfce_tasklist_button_proxy_menu_item`
//! - show-only-minimized — `set_show_only_minimized`

use wafflebar_core::{
    ActionId, ConfigField, Event, ModuleConfig, Plugin, Reaction, Topic, View, Window, WmCommand,
    WmEvent,
};

const FALLBACK_ICON: &str = "application-x-executable";

/// Read `max_chars` (0 = no truncation). Shared by the registry and `configure`.
pub(crate) fn read_max_chars(cfg: &ModuleConfig) -> usize {
    cfg.opt_i64("max_chars").unwrap_or(0).max(0) as usize
}

pub struct Tasklist {
    output: String,
    windows: Vec<Window>,
    max_chars: usize,
}

impl Tasklist {
    pub fn new(output: &str, max_chars: usize) -> Self {
        Self {
            output: output.to_string(),
            windows: Vec::new(),
            max_chars,
        }
    }

    /// Windows shown on this bar: those present on our output (or with no output reported yet).
    /// Mirrors `xfce_tasklist_button_visible` + `set_monitors_to_include`.
    fn visible(&self) -> impl Iterator<Item = &Window> {
        self.windows
            .iter()
            .filter(move |w| w.outputs.is_empty() || w.outputs.iter().any(|o| o == &self.output))
    }

    fn truncate(&self, title: &str) -> String {
        if self.max_chars == 0 || title.chars().count() <= self.max_chars {
            return title.to_string();
        }
        let s: String = title.chars().take(self.max_chars.saturating_sub(1)).collect();
        format!("{s}…")
    }
}

impl Plugin for Tasklist {
    fn id(&self) -> &str {
        "tasklist"
    }

    fn subscribe(&self) -> Vec<Topic> {
        vec![Topic::Wm]
    }

    fn view(&self) -> View {
        let buttons: Vec<View> = self
            .visible()
            .map(|w| {
                let icon_name = if w.app_id.is_empty() {
                    FALLBACK_ICON
                } else {
                    &w.app_id
                };
                let content = View::row(
                    vec![
                        View::icon(icon_name, 16).with_class("task-icon"),
                        View::label(self.truncate(&w.title)).with_class("task-label"),
                    ],
                    4,
                );
                // Key by toplevel id so closing/reordering a window reuses the other buttons
                // instead of rebuilding the whole list.
                let mut btn = content
                    .button(ActionId::new(format!("activate:{}", w.id)))
                    .with_key(format!("win:{}", w.id))
                    .with_class("task");
                if w.focused {
                    btn = btn.with_class("active");
                }
                if w.minimized {
                    btn = btn.with_class("minimized");
                }
                btn
            })
            .collect();
        if buttons.is_empty() {
            View::Empty
        } else {
            View::row(buttons, 4).with_class("tasklist")
        }
    }

    fn on_event(&mut self, ev: &Event) -> Reaction {
        if let Event::Wm(WmEvent::Windows { windows }) = ev {
            // Only dirty on an actual change (see tags.rs): foreign-toplevel re-emits the window
            // list on unrelated activity; rebuilding when it's identical is wasted work.
            if *windows != self.windows {
                self.windows = windows.clone();
                return Reaction::dirty();
            }
        }
        Reaction::none()
    }

    fn on_action(&mut self, action: &ActionId) -> Reaction {
        if let Some(id) = action.0.strip_prefix("activate:").and_then(|s| s.parse().ok()) {
            return Reaction::command(WmCommand::ActivateWindow(id));
        }
        Reaction::none()
    }

    fn configure(&mut self, cfg: &ModuleConfig) -> Reaction {
        // Re-read the option; `output` and the live window list are preserved.
        self.max_chars = read_max_chars(cfg);
        Reaction::dirty()
    }

    fn config_schema(&self) -> Vec<ConfigField> {
        vec![ConfigField::int("max_chars", "Max characters per title (0 = unlimited)", 0, 200, 0)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::test_module_config as cfg;
    use wafflebar_core::fake::FakeWm;

    #[test]
    fn configure_re_reads_max_chars_preserving_output() {
        let mut t = Tasklist::new("eDP-1", 0);
        let r = t.configure(&cfg(&[("max_chars", toml::Value::from(20))]));
        assert!(r.dirty);
        assert_eq!(t.max_chars, 20);
        assert_eq!(t.output, "eDP-1");
    }

    fn win(id: u64, title: &str, app_id: &str, focused: bool, outputs: &[&str]) -> Window {
        Window {
            id,
            title: title.to_string(),
            app_id: app_id.to_string(),
            focused,
            minimized: false,
            outputs: outputs.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Collect (action, has-active-class) per task button.
    fn tasks(v: &View) -> Vec<(ActionId, bool)> {
        let View::Row { children, .. } = v else {
            return Vec::new();
        };
        children
            .iter()
            .filter_map(|c| match c {
                View::Button { action, classes, .. } => {
                    Some((action.clone(), classes.iter().any(|c| c == "active")))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn lists_windows_with_focused_marked_active() {
        let mut wm = FakeWm::new("DP-1", 9);
        let mut tl = Tasklist::new("DP-1", 0);
        let ev = wm.set_windows(vec![
            win(1, "Firefox", "firefox", false, &["DP-1"]),
            win(2, "kitty", "kitty", true, &["DP-1"]),
        ]);
        assert!(tl.on_event(&Event::Wm(ev)).dirty);
        let t = tasks(&tl.view());
        assert_eq!(t.len(), 2);
        let active: Vec<_> = t.iter().filter(|(_, a)| *a).collect();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].0, ActionId::new("activate:2"));
    }

    #[test]
    fn click_activates_window() {
        let mut tl = Tasklist::new("DP-1", 0);
        let r = tl.on_action(&ActionId::new("activate:7"));
        assert_eq!(r.commands, vec![WmCommand::ActivateWindow(7)]);
    }

    #[test]
    fn filters_by_output() {
        let mut tl = Tasklist::new("DP-1", 0);
        tl.on_event(&Event::Wm(WmEvent::Windows {
            windows: vec![
                win(1, "here", "a", false, &["DP-1"]),
                win(2, "elsewhere", "b", false, &["DP-2"]),
                win(3, "everywhere", "c", false, &[]), // no output reported -> shown
            ],
        }));
        let ids: Vec<_> = tasks(&tl.view()).into_iter().map(|(a, _)| a).collect();
        assert_eq!(ids, vec![ActionId::new("activate:1"), ActionId::new("activate:3")]);
    }

    #[test]
    fn closed_while_active_disappears() {
        let mut wm = FakeWm::new("DP-1", 9);
        let mut tl = Tasklist::new("DP-1", 0);
        tl.on_event(&Event::Wm(wm.set_windows(vec![
            win(1, "a", "a", false, &["DP-1"]),
            win(2, "focused", "b", true, &["DP-1"]),
        ])));
        // window 2 (the active one) closes
        let ev = wm.remove_window(2);
        tl.on_event(&Event::Wm(ev));
        let ids: Vec<_> = tasks(&tl.view()).into_iter().map(|(a, _)| a).collect();
        assert_eq!(ids, vec![ActionId::new("activate:1")]);
    }

    #[test]
    fn app_id_arriving_late_still_renders() {
        let mut tl = Tasklist::new("DP-1", 0);
        // first frame: no app_id yet
        tl.on_event(&Event::Wm(WmEvent::Windows {
            windows: vec![win(1, "Loading", "", false, &["DP-1"])],
        }));
        assert_eq!(tasks(&tl.view()).len(), 1, "renders with fallback icon before app_id");
        // later frame: app_id filled in
        tl.on_event(&Event::Wm(WmEvent::Windows {
            windows: vec![win(1, "Firefox", "firefox", false, &["DP-1"])],
        }));
        assert_eq!(tasks(&tl.view()).len(), 1);
    }

    #[test]
    fn empty_list_renders_nothing() {
        let tl = Tasklist::new("DP-1", 0);
        assert_eq!(tl.view(), View::Empty);
    }
}
