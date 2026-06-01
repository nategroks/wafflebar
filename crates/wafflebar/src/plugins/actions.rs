//! Actions plugin: a bar button that opens a session/power popover —
//! Lock, Logout, Suspend, Hibernate, Reboot, Shut Down.
//!
//! Pure reducer + `Reaction::spawn`. v1 drives elogind/systemd-logind through
//! the `loginctl` CLI (which proxies to `org.freedesktop.login1`), so no zbus
//! dependency yet. The lock action defers to a user-configurable command since
//! the locker (swaylock, waylock, etc.) is system-policy, not ours.
//!
//! NB: "Log Out" terminates the current login session via `loginctl
//! terminate-session "$XDG_SESSION_ID"`, so the env var has to be present in
//! the bar's process — which it is, since wafflebar is launched from inside
//! the session.

use wafflebar_core::{
    ActionId, ConfigField, Event, ModuleConfig, Plugin, Reaction, Topic, View,
};

const DEFAULT_ICON: &str = "system-shutdown-symbolic";
const DEFAULT_LOCK_CMD: &str = "loginctl lock-session";
const DEFAULT_ACTIONS: &str = "lock,logout,suspend,hibernate,reboot,shutdown";

const ACT_LOCK: &str = "act-lock";
const ACT_LOGOUT: &str = "act-logout";
const ACT_SUSPEND: &str = "act-suspend";
const ACT_HIBERNATE: &str = "act-hibernate";
const ACT_REBOOT: &str = "act-reboot";
const ACT_SHUTDOWN: &str = "act-shutdown";

pub struct Actions {
    icon: String,
    label: String,
    /// Action keys to show, in display order (subset of "lock,logout,suspend,hibernate,reboot,shutdown").
    enabled: Vec<String>,
    /// Shell-style command string; split on whitespace at click time.
    lock_command: String,
}

#[derive(Clone)]
struct Item {
    id: &'static str,
    label: &'static str,
    icon: &'static str,
    argv: Vec<String>,
}

impl Actions {
    pub fn new(cfg: &ModuleConfig) -> Self {
        Self {
            icon: cfg.opt_str("icon").unwrap_or(DEFAULT_ICON).to_string(),
            label: cfg.opt_str("label").unwrap_or_default().to_string(),
            enabled: cfg
                .opt_str("actions")
                .unwrap_or(DEFAULT_ACTIONS)
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            lock_command: cfg.opt_str("lock_command").unwrap_or(DEFAULT_LOCK_CMD).to_string(),
        }
    }

    /// Full action table, filtered by `self.enabled` and ordered as the user listed them.
    /// Lock command splits on whitespace (no quote handling — keep arguments space-free).
    fn items(&self) -> Vec<Item> {
        let s = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let lock_argv: Vec<String> = self.lock_command.split_whitespace().map(String::from).collect();
        let lock_argv = if lock_argv.is_empty() { s(&["loginctl", "lock-session"]) } else { lock_argv };
        let all = [
            ("lock",      Item { id: ACT_LOCK,      label: "Lock Screen", icon: "system-lock-screen-symbolic", argv: lock_argv }),
            ("logout",    Item { id: ACT_LOGOUT,    label: "Log Out",     icon: "system-log-out-symbolic",     argv: s(&["sh", "-c", "loginctl terminate-session \"$XDG_SESSION_ID\""]) }),
            ("suspend",   Item { id: ACT_SUSPEND,   label: "Suspend",     icon: "system-suspend-symbolic",     argv: s(&["loginctl", "suspend"]) }),
            ("hibernate", Item { id: ACT_HIBERNATE, label: "Hibernate",   icon: "system-hibernate-symbolic",   argv: s(&["loginctl", "hibernate"]) }),
            ("reboot",    Item { id: ACT_REBOOT,    label: "Restart",     icon: "system-reboot-symbolic",      argv: s(&["loginctl", "reboot"]) }),
            ("shutdown",  Item { id: ACT_SHUTDOWN,  label: "Shut Down",   icon: "system-shutdown-symbolic",    argv: s(&["loginctl", "poweroff"]) }),
        ];
        self.enabled
            .iter()
            .filter_map(|k| all.iter().find(|(name, _)| *name == k).map(|(_, it)| it.clone()))
            .collect()
    }
}

impl Plugin for Actions {
    fn id(&self) -> &str { "actions" }

    fn subscribe(&self) -> Vec<Topic> { Vec::new() }

    fn view(&self) -> View {
        let trigger = if self.label.is_empty() {
            View::icon(&self.icon, 18)
        } else {
            View::row(vec![View::icon(&self.icon, 18), View::label(&self.label)], 6)
        };
        let buttons: Vec<View> = self
            .items()
            .into_iter()
            .map(|it| {
                View::row(
                    vec![
                        View::icon(it.icon, 18).with_class("actions-icon"),
                        View::label(it.label).with_class("actions-label"),
                    ],
                    10,
                )
                .button(ActionId::new(it.id))
                .with_class("actions-item")
            })
            .collect();
        let content = View::Col {
            children: buttons,
            gap: 4,
            classes: vec!["actions-menu".to_string()],
        };
        View::Popover {
            trigger: Box::new(trigger),
            content: Box::new(content),
            classes: Vec::new(),
        }
        .with_class("actions")
    }

    fn on_event(&mut self, _ev: &Event) -> Reaction { Reaction::none() }

    fn on_action(&mut self, action: &ActionId) -> Reaction {
        let Some(it) = self.items().into_iter().find(|i| i.id == action.0) else {
            return Reaction::none();
        };
        if it.argv.is_empty() { return Reaction::none(); }
        Reaction::spawn(it.argv)
    }

    fn configure(&mut self, cfg: &ModuleConfig) -> Reaction {
        *self = Self::new(cfg);
        Reaction::dirty()
    }

    fn config_schema(&self) -> Vec<ConfigField> {
        vec![
            ConfigField::file("icon", "Button icon (theme name or PNG/SVG)", DEFAULT_ICON),
            ConfigField::text("label", "Button text (blank = icon only)", ""),
            ConfigField::text(
                "actions",
                "Comma-separated: lock,logout,suspend,hibernate,reboot,shutdown",
                DEFAULT_ACTIONS,
            ),
            ConfigField::text(
                "lock_command",
                "Lock command (default sends loginctl lock-session — needs a locker like swaylock)",
                DEFAULT_LOCK_CMD,
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::test_module_config;

    fn make(actions: &str) -> Actions {
        Actions::new(&test_module_config(&[("actions", toml::Value::String(actions.into()))]))
    }

    #[test]
    fn default_actions_render_six_items() {
        let a = Actions::new(&test_module_config(&[]));
        let View::Popover { content, .. } = a.view() else { panic!("expected popover") };
        let View::Col { children, .. } = *content else { panic!("expected col") };
        assert_eq!(children.len(), 6);
    }

    #[test]
    fn enabled_filter_preserves_order() {
        let a = make("shutdown,lock");
        let View::Popover { content, .. } = a.view() else { panic!("popover") };
        let View::Col { children, .. } = *content else { panic!("col") };
        assert_eq!(children.len(), 2);
        let View::Button { child: c0, .. } = &children[0] else { panic!("button") };
        let View::Row { children: c0_row, .. } = c0.as_ref() else { panic!("row") };
        assert!(matches!(&c0_row[1], View::Label { text, .. } if text == "Shut Down"));
    }

    #[test]
    fn click_emits_spawn() {
        let mut a = Actions::new(&test_module_config(&[]));
        let r = a.on_action(&ActionId::new(ACT_REBOOT));
        assert_eq!(r.spawn, vec![vec!["loginctl".to_string(), "reboot".to_string()]]);
    }

    #[test]
    fn lock_command_splits_on_whitespace() {
        let mut a = Actions::new(&test_module_config(&[
            ("lock_command", toml::Value::String("swaylock -f -c 1c1c1f".into())),
        ]));
        let r = a.on_action(&ActionId::new(ACT_LOCK));
        assert_eq!(
            r.spawn,
            vec![vec![
                "swaylock".to_string(),
                "-f".to_string(),
                "-c".to_string(),
                "1c1c1f".to_string(),
            ]]
        );
    }

    #[test]
    fn unknown_action_is_inert() {
        let mut a = Actions::new(&test_module_config(&[]));
        assert!(a.on_action(&ActionId::new("act-bogus")).spawn.is_empty());
    }
}
