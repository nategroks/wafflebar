//! Volume plugin: default-sink volume + mute. Mirrors xfce4-pulseaudio-plugin's panel button —
//! icon by level (`audio-volume-{muted,low,medium,high}`) + percent text, click toggles mute,
//! scroll changes volume by 5%.
//!
//! Pure reducer: it sees only typed [`VolumeEvent`]s (from `backend`, the sole libpulse site) and
//! emits [`VolumeCommand`]s. The value-compare guard is on the *derived* display percent, so
//! sub-percent backend jitter produces no re-render (the C2 reducer discipline).

pub mod backend;

use wafflebar_core::{
    ActionId, Event, Plugin, Reaction, Topic, View, VolumeCommand, VolumeEvent, VOLUME_NORM,
};

// Click opens the host-attached mixer popover (slider + mute) — the host owns that interaction
// (it needs GTK + the volume sink), so the plugin's click is an inert sentinel; the trigger stays a
// Button only for the scroll affordance + display. Mute now lives in the mixer, not a click-toggle.
const ACTION_OPEN: &str = "open-mixer";
const ACTION_UP: &str = "vol-up";
const ACTION_DOWN: &str = "vol-down";
const STEP_PERCENT: i32 = 5;

pub struct Volume {
    /// `None` until the first event (or after `Unavailable`) — drives the render-nothing path.
    state: Option<SinkState>,
}

/// The plugin's *derived* state: display percent (0..=100) + mute. Two backend events that round to
/// the same pair are equal here, so they don't dirty.
#[derive(Clone, Copy, PartialEq, Eq)]
struct SinkState {
    percent: u8,
    muted: bool,
}

impl Volume {
    pub fn new() -> Self {
        Self { state: None }
    }
}

impl Default for Volume {
    fn default() -> Self {
        Self::new()
    }
}

/// Round a [`VOLUME_NORM`]-scaled volume to a display percent (0..=100).
fn to_percent(volume: u32) -> u8 {
    (((volume as u64) * 100 + (VOLUME_NORM as u64) / 2) / (VOLUME_NORM as u64)).min(100) as u8
}

/// Icon name by level/mute — xfce4-pulseaudio-plugin's convention.
fn icon_for(s: SinkState) -> &'static str {
    if s.muted || s.percent == 0 {
        "wb-vol-muted-symbolic"
    } else if s.percent < 34 {
        "wb-vol-low-symbolic"
    } else if s.percent < 67 {
        "wb-vol-medium-symbolic"
    } else {
        "wb-vol-high-symbolic"
    }
}

impl Plugin for Volume {
    fn id(&self) -> &str {
        "volume"
    }

    fn subscribe(&self) -> Vec<Topic> {
        vec![Topic::Audio]
    }

    fn view(&self) -> View {
        let Some(s) = self.state else {
            return View::Empty; // no backend / no default sink — render nothing, not a dead button
        };
        let label = if s.muted {
            "muted".to_string()
        } else {
            format!("{}%", s.percent)
        };
        View::row(
            vec![
                View::icon(icon_for(s), 16).with_class("vol-icon"),
                View::label(label).with_class("vol-label"),
            ],
            4,
        )
        .button(ActionId::new(ACTION_OPEN))
        .with_scroll(ActionId::new(ACTION_UP), ActionId::new(ACTION_DOWN))
        .with_class("module")
        .with_class("volume")
    }

    fn on_event(&mut self, ev: &Event) -> Reaction {
        match ev {
            Event::Volume(VolumeEvent::SinkVolumeChanged { volume, muted, .. }) => {
                let next = SinkState { percent: to_percent(*volume), muted: *muted };
                // Compare derived state: jitter below display resolution → no re-render.
                if self.state == Some(next) {
                    return Reaction::none();
                }
                self.state = Some(next);
                Reaction::dirty()
            }
            Event::Volume(VolumeEvent::Unavailable) => {
                if self.state.is_none() {
                    return Reaction::none();
                }
                self.state = None;
                Reaction::dirty()
            }
            _ => Reaction::none(),
        }
    }

    fn on_action(&mut self, action: &ActionId) -> Reaction {
        match action.0.as_str() {
            // The click is handled host-side (opens the mixer popover); nothing to do here.
            ACTION_OPEN => Reaction::none(),
            ACTION_UP => Reaction::volume(VolumeCommand::Adjust { delta: STEP_PERCENT }),
            ACTION_DOWN => Reaction::volume(VolumeCommand::Adjust { delta: -STEP_PERCENT }),
            _ => Reaction::none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink(volume: u32, muted: bool) -> Event {
        Event::Volume(VolumeEvent::SinkVolumeChanged { sink_index: 0, volume, muted })
    }

    #[test]
    fn unchanged_derived_state_does_not_dirty() {
        let mut v = Volume::new();
        // First event paints.
        assert!(v.on_event(&sink(VOLUME_NORM / 2, false)).dirty);
        // Two consecutive events with the SAME volume+muted fields → zero dirty cycles.
        assert!(!v.on_event(&sink(VOLUME_NORM / 2, false)).dirty);
        // Sub-percent jitter (different raw, same rounded percent) → still no dirty.
        assert!(!v.on_event(&sink(VOLUME_NORM / 2 + 10, false)).dirty);
    }

    #[test]
    fn real_change_dirties() {
        let mut v = Volume::new();
        v.on_event(&sink(VOLUME_NORM / 2, false));
        assert!(v.on_event(&sink(VOLUME_NORM, false)).dirty, "50% -> 100% repaints");
        assert!(v.on_event(&sink(VOLUME_NORM, true)).dirty, "mute change repaints");
    }

    #[test]
    fn unavailable_renders_nothing() {
        let mut v = Volume::new();
        v.on_event(&sink(VOLUME_NORM, false));
        assert!(matches!(v.view(), View::Button { .. }));
        assert!(v.on_event(&Event::Volume(VolumeEvent::Unavailable)).dirty);
        assert_eq!(v.view(), View::Empty);
        // A second Unavailable is a no-op (already nothing).
        assert!(!v.on_event(&Event::Volume(VolumeEvent::Unavailable)).dirty);
    }

    #[test]
    fn default_sink_change_re_derives() {
        // A sink-change arrives as another SinkVolumeChanged (new index/volume) → button updates.
        let mut v = Volume::new();
        v.on_event(&sink(VOLUME_NORM, false));
        let r = v.on_event(&Event::Volume(VolumeEvent::SinkVolumeChanged {
            sink_index: 7,
            volume: VOLUME_NORM / 4,
            muted: false,
        }));
        assert!(r.dirty);
        match v.view() {
            View::Button { child, .. } => {
                // 25% → low icon
                let View::Row { children, .. } = *child else { panic!("row") };
                assert!(matches!(&children[0], View::Icon { name, .. } if name == "wb-vol-low-symbolic"));
            }
            _ => panic!("expected button"),
        }
    }

    #[test]
    fn actions_map_to_commands() {
        let mut v = Volume::new();
        // Click is inert at the plugin level (the host's mixer popover owns it).
        assert!(v.on_action(&ActionId::new(ACTION_OPEN)).volume.is_empty());
        assert_eq!(
            v.on_action(&ActionId::new(ACTION_UP)).volume,
            vec![VolumeCommand::Adjust { delta: 5 }]
        );
        assert_eq!(
            v.on_action(&ActionId::new(ACTION_DOWN)).volume,
            vec![VolumeCommand::Adjust { delta: -5 }]
        );
    }
}
