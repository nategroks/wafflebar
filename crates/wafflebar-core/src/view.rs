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

impl ActionId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
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
    /// A clickable wrapper; clicking it routes `action` to the owning module.
    Button { child: Box<View>, action: ActionId, classes: Vec<String> },
    /// Expanding empty space (pushes neighbours apart).
    Spacer,
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

    /// Wrap `self` in a clickable button carrying `action`.
    pub fn button(self, action: impl Into<ActionId>) -> View {
        View::Button {
            child: Box::new(self),
            action: action.into(),
            classes: Vec::new(),
        }
    }

    /// Add a CSS class (no-op on variants without a class list, e.g. `Spacer`/`Empty`).
    pub fn with_class(mut self, class: impl Into<String>) -> View {
        if let Some(classes) = self.classes_mut() {
            classes.push(class.into());
        }
        self
    }

    fn classes_mut(&mut self) -> Option<&mut Vec<String>> {
        match self {
            View::Label { classes, .. }
            | View::Icon { classes, .. }
            | View::Row { classes, .. }
            | View::Col { classes, .. }
            | View::Button { classes, .. }
            | View::Popover { classes, .. } => Some(classes),
            View::Spacer | View::Empty => None,
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
