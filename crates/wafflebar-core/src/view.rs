//! `View` — the GTK-free description a [`Plugin`](crate::plugin::Plugin) returns.
//!
//! **The v1→v2 isolation invariant (see `docs/ARCHITECTURE.md`):** nothing in this module may
//! reference `gtk4` (or any toolkit). A `View` is a serializable *description* of UI; the host
//! (the `wafflebar` binary) renders it to real widgets and owns the widget tree. Clicks route
//! back to the owning module by [`ActionId`] — no widget or callback ever crosses the boundary.
//! Because of this, moving a module into its own process later is a transport wrapper around the
//! same trait, not a rewrite.

use serde::{Deserialize, Serialize};

/// Identifies an interactive element. Routed back to the owning module's
/// [`Plugin::on_action`](crate::plugin::Plugin::on_action). Opaque + serializable on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActionId(pub String);

/// An entry in a [`View::Button`]'s right-click context menu. Order is preserved as given
/// (e.g. desktop-file `Actions=` order — never re-sorted).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MenuItem {
    /// A clickable label routing `action` to the owning plugin.
    Item { label: String, action: ActionId },
    /// A horizontal separator.
    Separator,
}

impl ActionId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

/// A [`View::Separator`]'s visual style — mirrors xfce4-panel's separator plugin styles
/// (`plugins/separator/separator.c`). Thickness/size come from CSS, not config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SeparatorStyle {
    /// Blank, fully transparent space.
    Transparent,
    /// A single rule (xfce4-panel's default).
    #[default]
    Line,
    /// The dotted-bar grip ("handle").
    Handle,
    /// A column/row of dots.
    Dots,
}

impl SeparatorStyle {
    /// Parse a config string; unknown/empty falls back to the default ([`SeparatorStyle::Line`]).
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "transparent" => SeparatorStyle::Transparent,
            "handle" => SeparatorStyle::Handle,
            "dots" => SeparatorStyle::Dots,
            _ => SeparatorStyle::Line,
        }
    }
}

impl<T: Into<String>> From<T> for ActionId {
    fn from(s: T) -> Self {
        ActionId(s.into())
    }
}

/// A GTK-free description of a module's UI subtree. The host renders this to widgets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum View {
    /// A text label.
    Label { text: String, classes: Vec<String> },
    /// An icon by freedesktop name (resolved against the host's GTK icon theme).
    Icon { name: String, size: u32, classes: Vec<String> },
    /// A horizontal container.
    Row { children: Vec<View>, gap: u32, classes: Vec<String> },
    /// A vertical container.
    Col { children: Vec<View>, gap: u32, classes: Vec<String> },
    /// A clickable wrapper; left-click routes `action` to the owning module. An optional
    /// right-click context `menu` (empty = none) lets a plugin offer secondary actions
    /// (e.g. the launcher's `Actions=`); each item routes its own `ActionId`.
    Button {
        child: Box<View>,
        action: ActionId,
        classes: Vec<String>,
        menu: Vec<MenuItem>,
        /// Stable identity for keyed reconciliation when this button is a list element (e.g. a
        /// tasklist entry keyed by toplevel handle id). `None` → the renderer keys it by position.
        /// See `reconcile`. Plugins set this when the list is non-positional so reordering/removal
        /// reuses the right widgets instead of rebuilding siblings.
        key: Option<String>,
        /// Actions dispatched on scroll up / down over the button (the volume plugin's ±5% wheel).
        /// `None` → no scroll handling. Minimal first scroll affordance; not generalized.
        scroll_up: Option<ActionId>,
        scroll_down: Option<ActionId>,
    },
    /// Expanding empty space (pushes neighbours apart).
    Spacer,
    /// A separator between plugins. `expand: true` makes it take all available space (the
    /// xfce4-panel "Expand" option) — the idiom for "left plugins | gap | right plugins".
    /// Thickness/size are CSS, not config. The host renders this along the bar's cross-axis.
    Separator { style: SeparatorStyle, expand: bool },
    /// A trigger that reveals `content` in a popover. The affordance exists in the contract from
    /// v1 (see ARCHITECTURE.md) so popover-bearing modules add content, not plumbing; v1 modules
    /// (tags/window/taskbar) do not emit this yet.
    Popover {
        trigger: Box<View>,
        content: Box<View>,
        classes: Vec<String>,
    },
    /// Nothing (an empty cell).
    Empty,
}

impl View {
    /// A plain label.
    pub fn label(text: impl Into<String>) -> View {
        View::Label {
            text: text.into(),
            classes: Vec::new(),
        }
    }

    /// An icon of the given pixel size.
    pub fn icon(name: impl Into<String>, size: u32) -> View {
        View::Icon {
            name: name.into(),
            size,
            classes: Vec::new(),
        }
    }

    /// A horizontal row of children with a pixel gap.
    pub fn row(children: Vec<View>, gap: u32) -> View {
        View::Row {
            children,
            gap,
            classes: Vec::new(),
        }
    }

    /// Wrap `self` in a clickable button carrying `action` (no context menu).
    pub fn button(self, action: impl Into<ActionId>) -> View {
        View::Button {
            child: Box::new(self),
            action: action.into(),
            classes: Vec::new(),
            menu: Vec::new(),
            key: None,
            scroll_up: None,
            scroll_down: None,
        }
    }

    /// Attach scroll-up / scroll-down actions (no-op unless `self` is a `Button`).
    pub fn with_scroll(mut self, up: impl Into<ActionId>, down: impl Into<ActionId>) -> View {
        if let View::Button { scroll_up, scroll_down, .. } = &mut self {
            *scroll_up = Some(up.into());
            *scroll_down = Some(down.into());
        }
        self
    }

    /// Set the reconciliation key (no-op unless `self` is a `Button`). Use for list elements whose
    /// identity is data-derived (tasklist handle id, tag index) rather than positional.
    pub fn with_key(mut self, key: impl Into<String>) -> View {
        if let View::Button { key: k, .. } = &mut self {
            *k = Some(key.into());
        }
        self
    }

    /// Attach a right-click context menu (no-op unless `self` is a `Button`).
    pub fn with_menu(mut self, items: Vec<MenuItem>) -> View {
        if let View::Button { menu, .. } = &mut self {
            *menu = items;
        }
        self
    }

    /// Add a CSS class (no-op on variants without a class list, e.g. `Spacer`/`Empty`).
    pub fn with_class(mut self, class: impl Into<String>) -> View {
        if let Some(classes) = self.classes_mut() {
            classes.push(class.into());
        }
        self
    }

    /// Short tag naming this node's variant — part of the default (positional) reconcile key, so a
    /// `Label` and an `Icon` at the same index never alias.
    pub fn variant_tag(&self) -> &'static str {
        match self {
            View::Label { .. } => "label",
            View::Icon { .. } => "icon",
            View::Row { .. } => "row",
            View::Col { .. } => "col",
            View::Button { .. } => "button",
            View::Spacer => "spacer",
            View::Separator { .. } => "sep",
            View::Popover { .. } => "popover",
            View::Empty => "empty",
        }
    }

    /// The plugin-set reconcile key, if any (only `Button` carries one today).
    pub fn explicit_key(&self) -> Option<&str> {
        match self {
            View::Button { key, .. } => key.as_deref(),
            _ => None,
        }
    }

    /// Number of nodes in this subtree (self + descendants). Instrumentation aid: a full rebuild
    /// recreates this many widgets, so logging it exposes any plugin rebuilding far more than its
    /// data changed (a reactivity bug the full-rebuild shortcut would otherwise mask).
    pub fn node_count(&self) -> usize {
        1 + match self {
            View::Row { children, .. } | View::Col { children, .. } => {
                children.iter().map(View::node_count).sum()
            }
            View::Button { child, .. } => child.node_count(),
            View::Popover { trigger, content, .. } => trigger.node_count() + content.node_count(),
            _ => 0,
        }
    }

    fn classes_mut(&mut self) -> Option<&mut Vec<String>> {
        match self {
            View::Label { classes, .. }
            | View::Icon { classes, .. }
            | View::Row { classes, .. }
            | View::Col { classes, .. }
            | View::Button { classes, .. }
            | View::Popover { classes, .. } => Some(classes),
            View::Spacer | View::Separator { .. } | View::Empty => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_and_classes() {
        let v = View::label("hi").with_class("clock").button("click-me");
        match v {
            View::Button { child, action, .. } => {
                assert_eq!(action, ActionId::new("click-me"));
                assert!(matches!(*child, View::Label { .. }));
            }
            _ => panic!("expected button"),
        }
    }

    #[test]
    fn nested_view_builds() {
        // Serializability itself is guaranteed by the derives (a serde format dep would be
        // needed to round-trip in a test; deferred until we actually wire v2 transport).
        let v = View::row(vec![View::icon("firefox", 16), View::label("Firefox")], 4);
        match v {
            View::Row { children, gap, .. } => {
                assert_eq!(children.len(), 2);
                assert_eq!(gap, 4);
            }
            _ => panic!("expected row"),
        }
    }
}
