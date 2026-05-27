//! Tags module — the dwl workspace/tag indicator.
//!
//! A pure reducer (see `docs/ARCHITECTURE.md`): it caches the tag list from `WmEvent::Tags` for
//! its output, renders it as a row of clickable tag buttons, and turns a click into a
//! `FocusTag` command. No GTK, no live WM handle.
//!
//! Note on indexing: `Tag::index` is **0-based** (tag 0 displays as "1"); dwl's tag bitmask is
//! `1 << index`. `ActionId`s and `WmCommand::FocusTag` carry the 0-based index.

use wafflebar_core::{
    ActionId, Event, Plugin, Reaction, Tag, TagState, Topic, View, WmCommand, WmEvent,
};

/// The tags indicator for one output.
pub struct Tags {
    output: String,
    tags: Vec<Tag>,
}

impl Tags {
    pub fn new(output: &str) -> Self {
        Self {
            output: output.to_string(),
            tags: Vec::new(),
        }
    }
}

impl Plugin for Tags {
    fn id(&self) -> &str {
        "tags"
    }

    fn subscribe(&self) -> Vec<Topic> {
        vec![Topic::Wm]
    }

    fn view(&self) -> View {
        let buttons = self
            .tags
            .iter()
            .map(|t| {
                let mut tag = View::label(t.name.clone()).with_class("tag");
                match t.state {
                    TagState::Active => tag = tag.with_class("active"),
                    TagState::Urgent => tag = tag.with_class("urgent"),
                    TagState::None => {}
                }
                if t.occupied {
                    tag = tag.with_class("occupied");
                }
                if t.focused {
                    tag = tag.with_class("focused");
                }
                // Key by tag index so switching active only updates the two affected buttons.
                tag.button(ActionId::new(format!("tag:{}", t.index)))
                    .with_key(format!("tag:{}", t.index))
            })
            .collect();
        View::row(buttons, 2).with_class("tags")
    }

    fn on_event(&mut self, ev: &Event) -> Reaction {
        if let Event::Wm(WmEvent::Tags { output, tags }) = ev {
            // Only dirty on an actual change: dwl re-emits tag state on many triggers (frame,
            // focus activity) with identical contents; rebuilding on every one is wasted work the
            // full-rebuild path used to mask (and a keyed diff would still walk the tree for).
            if *output == self.output && *tags != self.tags {
                self.tags = tags.clone();
                return Reaction::dirty();
            }
        }
        Reaction::none()
    }

    fn on_action(&mut self, action: &ActionId) -> Reaction {
        if let Some(idx) = action.0.strip_prefix("tag:").and_then(|s| s.parse().ok()) {
            return Reaction::command(WmCommand::FocusTag {
                output: self.output.clone(),
                tag: idx,
            });
        }
        Reaction::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wafflebar_core::fake::FakeWm;
    use wafflebar_core::wm::WindowManager;

    /// Collect (label-text, action, has-active-class) for each tag button in a tags View.
    fn tag_buttons(v: &View) -> Vec<(String, ActionId, bool)> {
        let View::Row { children, .. } = v else {
            panic!("tags view should be a Row");
        };
        children
            .iter()
            .map(|child| {
                let View::Button { child, action, .. } = child else {
                    panic!("tag should be a Button");
                };
                let View::Label { text, classes } = child.as_ref() else {
                    panic!("tag button child should be a Label");
                };
                (
                    text.clone(),
                    action.clone(),
                    classes.iter().any(|c| c == "active"),
                )
            })
            .collect()
    }

    #[test]
    fn active_tag_renders_exactly_one_active_pointing_at_it() {
        let mut wm = FakeWm::new("DP-1", 9);
        let mut tags = Tags::new("DP-1");
        for ev in wm.snapshot() {
            tags.on_event(&Event::Wm(ev));
        }

        // Tag index 2 (displayed "3") becomes active.
        let ev = wm.set_active_tag(2);
        assert!(tags.on_event(&Event::Wm(ev)).dirty);

        let buttons = tag_buttons(&tags.view());
        let active: Vec<_> = buttons.iter().filter(|(_, _, a)| *a).collect();
        assert_eq!(active.len(), 1, "exactly one active tag");
        assert_eq!(active[0].0, "3", "display name is 1-based");
        assert_eq!(active[0].1, ActionId::new("tag:2"), "action carries 0-based index");
    }

    #[test]
    fn clicking_a_tag_emits_focus_command() {
        let mut tags = Tags::new("DP-1");
        let r = tags.on_action(&ActionId::new("tag:3"));
        assert_eq!(
            r.commands,
            vec![WmCommand::FocusTag {
                output: "DP-1".to_string(),
                tag: 3
            }]
        );
        assert!(r.spawn.is_empty());
    }

    #[test]
    fn ignores_events_for_other_outputs() {
        let mut tags = Tags::new("DP-1");
        let r = tags.on_event(&Event::Wm(WmEvent::Tags {
            output: "DP-2".to_string(),
            tags: vec![],
        }));
        assert!(!r.dirty, "a different output's tags must not dirty this bar");
    }

    #[test]
    fn urgent_state_classed() {
        let mut wm = FakeWm::new("DP-1", 9);
        let mut tags = Tags::new("DP-1");
        let ev = wm.set_urgent(5);
        tags.on_event(&Event::Wm(ev));
        let View::Row { children, .. } = tags.view() else {
            panic!()
        };
        let View::Button { child, .. } = &children[5] else {
            panic!()
        };
        let View::Label { classes, .. } = child.as_ref() else {
            panic!()
        };
        assert!(classes.iter().any(|c| c == "urgent"));
    }
}
