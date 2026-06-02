//! Line-oriented parser for the dwl `-s` status protocol + coalesce-latest reducer.
//!
//! The grammar lives in `super`'s module docstring (pinned to v0.8 `9b11a49`'s `printstatus()`).
//! This module is the byte-to-state mapping plus the snapshot emitter.
//!
//! **Coalesce-latest by construction.** [`StatusReducer::feed`] *overwrites* per-monitor fields
//! on every applicable line — intermediate values are never queued. [`drain_events`] emits one
//! snapshot per *dirty* monitor (a monitor that had at least one field updated since the last
//! drain), then clears the dirty set. A 100-frame burst of identical title changes from dwl
//! therefore produces one snapshot, not 100. "Never blocks dwl" is satisfied because the
//! reducer's per-byte work is O(1) and the renderer's per-cycle work is O(dirty monitors), both
//! independent of producer rate.

use std::collections::{HashMap, HashSet};

use wafflebar_core::{Tag, TagState, WmEvent};

/// dwl's default tag count. A non-default `TAGCOUNT` would require a downstream patch *and*
/// either a config knob or a higher derived-from-bitmask count. v0.8 ships 9; we render 9 unless
/// any of the four masks references a higher bit (see [`max_tag_index`]).
const DEFAULT_TAGS: u32 = 9;

/// The four bitmasks emitted by the `tags` line, in dwl v0.8's `printstatus()` order.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
struct TagMasks {
    /// `occ`: union of `c->tags` for every client on this monitor.
    occ: u32,
    /// `m->tagset[m->seltags]`: the selected (viewed) tagset for this monitor.
    sel_tagset: u32,
    /// `c->tags` for the focused client (or `0` if no focused client).
    focused_client: u32,
    /// `urg`: union of `c->tags` for every client with `isurgent` on this monitor.
    urg: u32,
}

/// Per-monitor cumulative state. Each successful `apply_line` overwrites the relevant field.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
struct MonitorState {
    title: String,
    appid: String,
    /// `None` when dwl emitted an empty value (no focused client).
    #[allow(dead_code)] // step-3 may surface this to plugins; not in WmEvent yet
    fullscreen: Option<bool>,
    /// `None` when dwl emitted an empty value (no focused client).
    #[allow(dead_code)]
    floating: Option<bool>,
    #[allow(dead_code)] // step-3 may surface this to plugins; not in WmEvent yet
    selmon: bool,
    tags: TagMasks,
    layout: String,
}

/// The reducer. Owns the per-monitor cache and the trailing-byte buffer.
#[derive(Default)]
pub(super) struct StatusReducer {
    monitors: HashMap<String, MonitorState>,
    /// Monitor names that had at least one field overwritten since the last `drain_events`.
    dirty: HashSet<String>,
    /// Bytes received but not yet terminated by `\n` (carried across `feed` calls so a partial
    /// trailing line doesn't desync the parser).
    buf: Vec<u8>,
}

impl StatusReducer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append bytes; parse and apply every complete line. O(input) work; one `Vec` realloc per
    /// `feed` call (we `mem::take` the buffer so we can mutate `self` while iterating slices of
    /// it, then push the trailing partial line back).
    ///
    /// **UTF-8 robustness:** decode happens *per-component* inside [`apply_line`]: the monitor
    /// name and field name must be ASCII-clean (dwl emits exactly that — connector strings like
    /// `WL-1` and labels like `title`); the value is decoded lossily because it includes
    /// client-set window titles, where a misbehaving client can emit invalid bytes. A malformed
    /// title degrades to mojibake (U+FFFD), never a bar-wide panic, and never a silently-dropped
    /// state update on an otherwise valid line.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        let buf = std::mem::take(&mut self.buf);
        let mut start = 0;
        let mut last_terminator = 0;
        for i in 0..buf.len() {
            if buf[i] == b'\n' {
                self.apply_line(&buf[start..i]);
                start = i + 1;
                last_terminator = start;
            }
        }
        // Carry the trailing partial line (if any) into the next feed() call.
        self.buf.extend_from_slice(&buf[last_terminator..]);
    }

    fn apply_line(&mut self, line: &[u8]) {
        // Byte-level split on the first two spaces; the third component (value) keeps any
        // remaining spaces verbatim — matters for titles like "gero@whatsit ~/code/…" and layout
        // symbols like "(@)" / "[]=".
        let mut it = line.splitn(3, |&b| b == b' ');
        let mon_bytes = match it.next() {
            Some(b) if !b.is_empty() => b,
            _ => return,
        };
        let field_bytes = match it.next() {
            Some(b) if !b.is_empty() => b,
            _ => return,
        };
        let value_bytes = it.next().unwrap_or(&[]);

        // Monitor + field must be valid UTF-8 (in practice ASCII). If not, the line is
        // structurally bogus (dwl wouldn't emit it); drop without touching state.
        let mon = match std::str::from_utf8(mon_bytes) {
            Ok(s) => s,
            Err(_) => return,
        };
        let field = match std::str::from_utf8(field_bytes) {
            Ok(s) => s,
            Err(_) => return,
        };
        // Value is whatever the wayland client set its title to. A spec-compliant client emits
        // UTF-8; a misbehaving one emits bytes. Lossy-decode so the field update still applies
        // (mojibake → renderer; better than the stale cached title showing for the rest of the
        // session). `Cow<str>` avoids the allocation when the bytes are valid UTF-8 (the common
        // case); we only materialize a `String` if we actually need to.
        let value: std::borrow::Cow<'_, str> = String::from_utf8_lossy(value_bytes);

        let state = self.monitors.entry(mon.to_string()).or_default();
        let changed = match field {
            "title" => {
                state.title = value.into_owned();
                true
            }
            "appid" => {
                state.appid = value.into_owned();
                true
            }
            "fullscreen" => {
                state.fullscreen = parse_optional_bool(&value);
                true
            }
            "floating" => {
                state.floating = parse_optional_bool(&value);
                true
            }
            "selmon" => {
                state.selmon = &*value == "1";
                true
            }
            "tags" => parse_tags(&value)
                .map(|m| {
                    state.tags = m;
                    true
                })
                .unwrap_or(false),
            "layout" => {
                state.layout = value.into_owned();
                true
            }
            _ => false, // unknown field — silently skip (forward-compat with future dwl versions)
        };
        if changed {
            self.dirty.insert(mon.to_string());
        }
    }

    /// Coalesce-latest: emit one snapshot per dirty monitor; clear the dirty set.
    pub fn drain_events(&mut self) -> Vec<WmEvent> {
        let dirty = std::mem::take(&mut self.dirty);
        let mut events = Vec::with_capacity(dirty.len() * 3);
        for mon_name in dirty {
            if let Some(state) = self.monitors.get(&mon_name) {
                push_snapshot(&mut events, &mon_name, state);
            }
        }
        events
    }

    /// Full snapshot of every known monitor, for [`super::WindowManager::snapshot`]. Does NOT
    /// touch the dirty set — this is read-only.
    pub fn full_snapshot(&self) -> Vec<WmEvent> {
        let mut events = Vec::with_capacity(self.monitors.len() * 3);
        for (name, state) in &self.monitors {
            push_snapshot(&mut events, name, state);
        }
        events
    }
}

fn push_snapshot(out: &mut Vec<WmEvent>, mon_name: &str, state: &MonitorState) {
    out.push(WmEvent::Tags {
        output: mon_name.to_string(),
        tags: build_tags(state.tags),
    });
    out.push(WmEvent::Layout {
        output: mon_name.to_string(),
        symbol: state.layout.clone(),
    });
    out.push(WmEvent::ActiveWindow {
        output: mon_name.to_string(),
        title: state.title.clone(),
        app_id: state.appid.clone(),
    });
}

fn parse_optional_bool(s: &str) -> Option<bool> {
    if s.is_empty() {
        None // dwl emits empty when no focused client
    } else {
        Some(s == "1")
    }
}

fn parse_tags(value: &str) -> Option<TagMasks> {
    let mut it = value.split(' ');
    let occ = it.next()?.parse().ok()?;
    let sel_tagset = it.next()?.parse().ok()?;
    let focused_client = it.next()?.parse().ok()?;
    let urg = it.next()?.parse().ok()?;
    Some(TagMasks {
        occ,
        sel_tagset,
        focused_client,
        urg,
    })
}

fn max_tag_index(t: TagMasks) -> u32 {
    let combined = t.occ | t.sel_tagset | t.focused_client | t.urg;
    if combined == 0 {
        DEFAULT_TAGS
    } else {
        // Highest set bit + 1, clamped to a sensible upper bound. dwl's tag count is `u32`-bit
        // but a patched config above ~9 is exceptional; the clamp prevents a runaway render if
        // a malformed line slips through.
        (32 - combined.leading_zeros()).max(DEFAULT_TAGS).min(32)
    }
}

fn build_tags(t: TagMasks) -> Vec<Tag> {
    let n = max_tag_index(t);
    (0..n)
        .map(|i| {
            let bit = 1u32 << i;
            let state = if t.urg & bit != 0 {
                TagState::Urgent
            } else if t.sel_tagset & bit != 0 {
                TagState::Active
            } else {
                TagState::None
            };
            Tag {
                index: i,
                name: (i + 1).to_string(),
                state,
                focused: t.focused_client & bit != 0,
                occupied: t.occ & bit != 0,
            }
        })
        .collect()
}
