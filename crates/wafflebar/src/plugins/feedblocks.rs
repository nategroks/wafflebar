//! `feedblocks` — render the latest [`FeedEvent::Frame`] as a horizontal row of labels.
//!
//! Read-only consumer of the someblocks-style external feed (NATEWM_MODE channel 2). The
//! producer (e.g. `natewm-status`) writes one line per refresh to wafflebar's status socket;
//! [`crate::feeds::someblocks`] parses each line into a `FeedEvent::Frame { blocks }` and
//! the host delivers it as `Event::Feed`. We overwrite our cached blocks and re-render — the
//! producer owns ordering and timing, the bar renders whatever arrived most recently.
//!
//! **Parity, not novelty.** The natewm contract calls for parity with the dwlb right-strip
//! (clock + mem), not for additional features. This plugin renders the blocks producer-side
//! delivers; it does not add menus, popovers, or click handlers. Per-block CSS class
//! (`feedblock` plus `b0`/`b1`/… for positional theming via Nord palette in nord.css).
//!
//! Empty state — before the first frame arrives — renders `View::Empty` so the slot doesn't
//! reserve visible space until a producer connects.

use wafflebar_core::{ActionId, ConfigField, Event, FeedEvent, ModuleConfig, Plugin, Reaction, Topic, View};

const DEFAULT_GAP: u32 = 8;

pub struct FeedBlocks {
    /// Pixel gap between adjacent block labels in the row.
    gap: u32,
    /// The latest frame's blocks; empty until the first `FeedEvent::Frame` arrives. We overwrite
    /// on every event — coalesce-latest at the plugin level too, mirroring the intake and the
    /// dwl_stdin reducer (NATEWM_MODE flag 6).
    blocks: Vec<wafflebar_core::FeedBlock>,
}

impl FeedBlocks {
    pub fn new(cfg: &ModuleConfig) -> Self {
        Self {
            gap: cfg.opt_i64("gap").map(|n| n.max(0) as u32).unwrap_or(DEFAULT_GAP),
            blocks: Vec::new(),
        }
    }
}

impl Plugin for FeedBlocks {
    fn id(&self) -> &str {
        "feedblocks"
    }

    fn subscribe(&self) -> Vec<Topic> {
        vec![Topic::Feed]
    }

    fn view(&self) -> View {
        if self.blocks.is_empty() {
            return View::Empty;
        }
        let children: Vec<View> = self
            .blocks
            .iter()
            .enumerate()
            .map(|(i, b)| {
                // Per-block class for positional theming (the producer's first block is
                // typically a clock; CSS can target `.feedblock.b0` for special styling).
                View::label(b.text.clone())
                    .with_class("feedblock")
                    .with_class(format!("b{i}"))
            })
            .collect();
        View::row(children, self.gap).with_class("feedblocks")
    }

    fn on_event(&mut self, ev: &Event) -> Reaction {
        match ev {
            Event::Feed(FeedEvent::Frame { blocks }) => {
                // Overwrite-in-place — coalesce-latest. A burst of 100 frames produces 100
                // `on_event` calls but only one render at the next GTK tick.
                self.blocks = blocks.clone();
                Reaction::dirty()
            }
            _ => Reaction::none(),
        }
    }

    fn on_action(&mut self, _action: &ActionId) -> Reaction {
        // Read-only: producer-driven, no interactive surface. Matches dwlb right-strip parity.
        Reaction::none()
    }

    fn configure(&mut self, cfg: &ModuleConfig) -> Reaction {
        self.gap = cfg.opt_i64("gap").map(|n| n.max(0) as u32).unwrap_or(DEFAULT_GAP);
        Reaction::dirty()
    }

    fn config_schema(&self) -> Vec<ConfigField> {
        vec![ConfigField::int(
            "gap",
            "Pixel gap between blocks",
            0,
            64,
            DEFAULT_GAP as i64,
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::test_module_config as cfg;
    use wafflebar_core::FeedBlock;

    #[test]
    fn empty_until_first_frame() {
        let f = FeedBlocks::new(&cfg(&[]));
        assert!(matches!(f.view(), View::Empty));
    }

    #[test]
    fn overwrites_blocks_on_feed_event() {
        let mut f = FeedBlocks::new(&cfg(&[]));
        let frame = FeedEvent::Frame {
            blocks: vec![
                FeedBlock {
                    name: "b0".into(),
                    text: "14:23".into(),
                },
                FeedBlock {
                    name: "b1".into(),
                    text: "mem 7421/64000Mi".into(),
                },
            ],
        };
        let r = f.on_event(&Event::Feed(frame));
        assert!(r.dirty);
        match f.view() {
            View::Row { children, .. } => {
                assert_eq!(children.len(), 2);
                // Spot check label content
                if let View::Label { text, .. } = &children[0] {
                    assert_eq!(text, "14:23");
                } else {
                    panic!("expected Label");
                }
            }
            other => panic!("expected Row, got {other:?}"),
        }
    }

    #[test]
    fn second_frame_replaces_first_no_accumulation() {
        let mut f = FeedBlocks::new(&cfg(&[]));
        f.on_event(&Event::Feed(FeedEvent::Frame {
            blocks: vec![FeedBlock {
                name: "b0".into(),
                text: "old".into(),
            }],
        }));
        f.on_event(&Event::Feed(FeedEvent::Frame {
            blocks: vec![
                FeedBlock {
                    name: "b0".into(),
                    text: "new-1".into(),
                },
                FeedBlock {
                    name: "b1".into(),
                    text: "new-2".into(),
                },
            ],
        }));
        if let View::Row { children, .. } = f.view() {
            assert_eq!(children.len(), 2);
            if let View::Label { text, .. } = &children[0] {
                assert_eq!(text, "new-1", "second frame replaces first");
            }
        } else {
            panic!("expected Row");
        }
    }

    #[test]
    fn ignores_unrelated_events() {
        let mut f = FeedBlocks::new(&cfg(&[]));
        let r = f.on_event(&Event::Tick { secs: 1 });
        assert!(!r.dirty);
        assert!(f.blocks.is_empty());
    }
}
