//! Interface plugin: shows a specific kernel interface's name + throughput, regardless of whether
//! it's NetworkManager's primary connection. Lets you put both `eth0` and `wlan0` on the bar at
//! once (the existing `network` plugin only tracks NM's primary).
//!
//! Pure reducer: it sees only typed [`InterfaceState`]s from `backend` (the sole `/sys/class/net`
//! site). The value-compare guard is on the *derived display* (icon bucket + label), so per-second
//! backend ticks only re-render when the displayed state actually moves.

pub mod backend;

use wafflebar_core::{
    ActionId, ConfigField, Event, InterfaceLink, InterfaceState, ModuleConfig, Plugin, Reaction,
    Topic, View,
};

/// Read the `interface` option (required — empty default means "render nothing").
pub(crate) fn read_interface(cfg: &ModuleConfig) -> String {
    cfg.opt_str("interface").unwrap_or_default().to_string()
}

pub struct Interface {
    /// The kernel interface name this instance tracks, e.g. `wlan0`.
    name: String,
    /// `None` until first event or whenever the interface is missing.
    display: Option<Display>,
}

/// What actually drives the rendered widgets. Coarser than [`InterfaceLink`]: throughput is the
/// only thing that changes at the per-second cadence, so we keep the rate here. (The icon name
/// changes only when wireless/up toggles.)
#[derive(Clone, PartialEq, Eq)]
struct Display {
    icon: &'static str,
    label: String,
}

impl Interface {
    pub fn new(name: String) -> Self {
        Self { name, display: None }
    }
}

fn derive(link: &InterfaceLink) -> Display {
    if !link.up {
        return Display {
            icon: "wb-net-offline-symbolic",
            label: format!("{} down", link.name),
        };
    }
    let icon = if link.wireless {
        // No strength reading in v1 — always use the "good" wifi icon. Could plug iw later.
        "wb-net-wifi-good-symbolic"
    } else {
        "wb-net-wired-symbolic"
    };
    Display {
        icon,
        label: format!(
            "{} ↓{:.1} ↑{:.1}",
            link.name,
            link.rx_bps as f64 / 1e6,
            link.tx_bps as f64 / 1e6,
        ),
    }
}

impl Plugin for Interface {
    fn id(&self) -> &str {
        "interface"
    }

    fn subscribe(&self) -> Vec<Topic> {
        vec![Topic::Interface]
    }

    fn view(&self) -> View {
        let Some(d) = &self.display else {
            return View::Empty;
        };
        View::row(
            vec![
                View::icon(d.icon, 16).with_class("iface-icon"),
                View::label(d.label.clone()).with_class("iface-label"),
            ],
            4,
        )
        .with_class("module")
        .with_class("interface")
    }

    fn on_event(&mut self, ev: &Event) -> Reaction {
        let Event::Interface(state) = ev else {
            return Reaction::none();
        };
        if self.name.is_empty() {
            return Reaction::none(); // misconfigured: no interface → render nothing
        }
        let next = state.find(&self.name).map(derive);
        if self.display == next {
            return Reaction::none();
        }
        self.display = next;
        Reaction::dirty()
    }

    fn on_action(&mut self, _action: &ActionId) -> Reaction {
        Reaction::none()
    }

    fn configure(&mut self, cfg: &ModuleConfig) -> Reaction {
        let new_name = read_interface(cfg);
        if new_name == self.name {
            return Reaction::none();
        }
        self.name = new_name;
        self.display = None;
        Reaction::dirty()
    }

    fn config_schema(&self) -> Vec<ConfigField> {
        vec![ConfigField::text(
            "interface",
            "Kernel interface name (e.g. eth0, wlan0)",
            "",
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(name: &str, wireless: bool, up: bool, rx: u64, tx: u64) -> InterfaceLink {
        InterfaceLink {
            name: name.into(),
            up,
            wireless,
            rx_bps: rx,
            tx_bps: tx,
        }
    }
    fn state(links: Vec<InterfaceLink>) -> Event {
        Event::Interface(InterfaceState { links })
    }

    #[test]
    fn renders_nothing_with_no_interface_configured() {
        let mut i = Interface::new(String::new());
        assert!(!i.on_event(&state(vec![link("wlan0", true, true, 0, 0)])).dirty);
        assert_eq!(i.view(), View::Empty);
    }

    #[test]
    fn renders_when_matching_interface_appears() {
        let mut i = Interface::new("wlan0".into());
        assert!(i.on_event(&state(vec![link("wlan0", true, true, 1_000_000, 500_000)])).dirty);
        match i.view() {
            View::Row { children, .. } => {
                assert!(
                    matches!(&children[0], View::Icon { name, .. } if *name == "wb-net-wifi-good-symbolic")
                );
                assert!(
                    matches!(&children[1], View::Label { text, .. } if text == "wlan0 ↓1.0 ↑0.5")
                );
            }
            _ => panic!("expected Row"),
        }
    }

    #[test]
    fn renders_nothing_when_interface_missing_from_snapshot() {
        let mut i = Interface::new("wlan0".into());
        i.on_event(&state(vec![link("wlan0", true, true, 0, 0)]));
        assert!(i.on_event(&state(vec![link("eth0", false, true, 0, 0)])).dirty);
        assert_eq!(i.view(), View::Empty);
    }

    #[test]
    fn unchanged_display_does_not_dirty() {
        let mut i = Interface::new("wlan0".into());
        i.on_event(&state(vec![link("wlan0", true, true, 1_000_000, 0)]));
        assert!(!i.on_event(&state(vec![link("wlan0", true, true, 1_000_000, 0)])).dirty);
    }
}
