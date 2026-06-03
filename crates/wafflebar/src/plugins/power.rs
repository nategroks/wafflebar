//! Power plugin: a session / power menu. A trigger icon opens a click-out popover with rows to
//! sign out, hibernate, reboot, and shut down. Each row spawns a `loginctl` subcommand.
//!
//! Pure reducer, same discipline as the launcher: it resolves its action list at construction
//! (reading `XDG_SESSION_ID` from the environment for the sign-out target is a host-side read, fine
//! in the binary crate) and only ever emits `Reaction::spawn(argv)` — process spawning is the
//! host executor's job. The menu is a plain `View::Popover` of button rows (no host-rendered marker,
//! no keyboard), so it reuses the existing popover + `ActionId` routing and works click-only on dwl.

use wafflebar_core::{ActionId, Event, ModuleConfig, Plugin, Reaction, Topic, View};

/// Default trigger icon (the most-read affordance: a power glyph).
const DEFAULT_ICON: &str = "system-shutdown-symbolic";

/// One menu entry: a stable action id, a symbolic icon, a label, and the argv to spawn on click.
struct PowerAction {
    id: &'static str,
    icon: &'static str,
    label: &'static str,
    argv: Vec<String>,
}

pub struct Power {
    /// Trigger icon shown on the bar.
    icon: String,
    actions: Vec<PowerAction>,
}

/// `&["loginctl", "poweroff"]` → owned `Vec<String>`.
fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

impl Power {
    /// Build from config. Each action's command is overridable via a `*_cmd` string-list key
    /// (`signout_cmd`, `hibernate_cmd`, `reboot_cmd`, `poweroff_cmd`); the trigger icon via `icon`.
    ///
    /// Sign-out terminates *this* login session. `loginctl terminate-session` needs the session id,
    /// which elogind/pam exports as `XDG_SESSION_ID` into the compositor env (inherited here). If
    /// it's absent we fall back to the literal `self`, which loginctl resolves to the caller's
    /// session.
    pub fn new(cfg: &ModuleConfig) -> Self {
        let signout_default = match std::env::var("XDG_SESSION_ID") {
            Ok(id) if !id.is_empty() => vec!["loginctl".into(), "terminate-session".into(), id],
            _ => argv(&["loginctl", "terminate-session", "self"]),
        };

        // Config override if the key is present and non-empty, else the built-in default.
        let cmd = |key: &str, default: Vec<String>| {
            let list = cfg.opt_str_list(key);
            if list.is_empty() {
                default
            } else {
                list
            }
        };

        let actions = vec![
            PowerAction {
                id: "power-signout",
                icon: "system-log-out-symbolic",
                label: "Sign out",
                argv: cmd("signout_cmd", signout_default),
            },
            PowerAction {
                id: "power-hibernate",
                icon: "system-hibernate-symbolic",
                label: "Hibernate",
                argv: cmd("hibernate_cmd", argv(&["loginctl", "hibernate"])),
            },
            PowerAction {
                id: "power-reboot",
                icon: "system-reboot-symbolic",
                label: "Reboot",
                argv: cmd("reboot_cmd", argv(&["loginctl", "reboot"])),
            },
            PowerAction {
                id: "power-poweroff",
                icon: "system-shutdown-symbolic",
                label: "Shut down",
                argv: cmd("poweroff_cmd", argv(&["loginctl", "poweroff"])),
            },
        ];

        Self {
            icon: cfg.opt_str("icon").unwrap_or(DEFAULT_ICON).to_string(),
            actions,
        }
    }
}

/// The popover: one icon+label button row per action.
fn menu(actions: &[PowerAction]) -> View {
    let children = actions
        .iter()
        .map(|a| {
            View::row(
                vec![
                    View::icon(a.icon, 16).with_class("power-item-icon"),
                    View::label(a.label).with_class("power-item-label"),
                ],
                8,
            )
            .with_class("power-item")
            .button(ActionId::new(a.id))
        })
        .collect();
    View::Col { children, gap: 2, classes: vec!["power-menu".to_string()] }
}

impl Plugin for Power {
    fn id(&self) -> &str {
        "power"
    }

    fn subscribe(&self) -> Vec<Topic> {
        Vec::new() // static menu — no live state to track.
    }

    fn view(&self) -> View {
        View::Popover {
            trigger: Box::new(View::icon(&self.icon, 16).with_class("power-icon")),
            content: Box::new(menu(&self.actions)),
            classes: Vec::new(),
        }
        .with_class("module")
        .with_class("power")
    }

    fn on_event(&mut self, _ev: &Event) -> Reaction {
        Reaction::none()
    }

    fn on_action(&mut self, action: &ActionId) -> Reaction {
        for a in &self.actions {
            if action.0 == a.id {
                return Reaction::spawn(a.argv.clone());
            }
        }
        Reaction::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wafflebar_core::Cell;

    fn cfg(options: &[(&str, toml::Value)]) -> ModuleConfig {
        ModuleConfig {
            kind: "power".into(),
            cell: Cell { row: 0, col: 0, rowspan: 1, colspan: 1 },
            align: Default::default(),
            options: options.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        }
    }

    #[test]
    fn defaults_expose_the_four_actions() {
        let p = Power::new(&cfg(&[]));
        let labels: Vec<_> = p.actions.iter().map(|a| a.label).collect();
        assert_eq!(labels, ["Sign out", "Hibernate", "Reboot", "Shut down"]);
    }

    #[test]
    fn poweroff_defaults_to_loginctl() {
        let p = Power::new(&cfg(&[]));
        let off = p.actions.iter().find(|a| a.id == "power-poweroff").unwrap();
        assert_eq!(off.argv, vec!["loginctl".to_string(), "poweroff".to_string()]);
    }

    #[test]
    fn cmd_override_replaces_the_default() {
        let p = Power::new(&cfg(&[(
            "reboot_cmd",
            toml::Value::Array(vec!["systemctl".into(), "reboot".into()]),
        )]));
        let reboot = p.actions.iter().find(|a| a.id == "power-reboot").unwrap();
        assert_eq!(reboot.argv, vec!["systemctl".to_string(), "reboot".to_string()]);
    }

    #[test]
    fn clicking_an_action_spawns_its_argv() {
        let mut p = Power::new(&cfg(&[]));
        let r = p.on_action(&ActionId::new("power-poweroff"));
        assert_eq!(r.spawn, vec![vec!["loginctl".to_string(), "poweroff".to_string()]]);
    }

    #[test]
    fn unknown_action_is_a_noop() {
        let mut p = Power::new(&cfg(&[]));
        assert_eq!(p.on_action(&ActionId::new("nope")), Reaction::none());
    }

    #[test]
    fn icon_is_configurable() {
        let p = Power::new(&cfg(&[("icon", toml::Value::String("system-lock-screen-symbolic".into()))]));
        assert_eq!(p.icon, "system-lock-screen-symbolic");
    }
}
