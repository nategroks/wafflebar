//! Parser + reducer tests, driven by a **live-captured** golden fixture.
//!
//! Source of truth: `tests/fixtures/dwl_stdin_v0.8.txt` is raw bytes captured from
//! `dwl -s 'sh -c "... exec cat > /tmp/wb-fixture-raw.txt"'` running the natewm-asm pinned dwl
//! (v0.8 `9b11a49` + spiral + titlebar patches; `printstatus()` itself unmodified from upstream
//! v0.8). The fixture's job: prove that the parser agrees with what dwl actually puts on the
//! wire — not what `printstatus()` *looks* like it emits (printf format quirks, trailing
//! whitespace, field separators, the empty-value-on-no-focused-client case).
//!
//! Recapture procedure when the dwl pin bumps (rebase-fragile thing):
//!
//! ```sh
//! WLR_BACKENDS=wayland \
//!   /tmp/cage/build/cage -- vendor/dwl/dwl \
//!     -s 'sh -c "foot --title=ALPHA & sleep 0.8; foot --title=BETA & sleep 0.5; \
//!                exec cat > tests/fixtures/dwl_stdin_v0.8.txt"'
//! # let it run ~5s, kill foots, kill cage. Verify the fixture covers map / title-change /
//! # unmap transitions, then rerun the tests.
//! ```

use super::parser::*;
use std::collections::HashMap;
use wafflebar_core::{TagState, WmEvent};

/// Raw bytes captured live from `dwl -s` on the pinned tree. Source of truth for the wire format.
const FIXTURE: &[u8] = include_bytes!("../../../../../tests/fixtures/dwl_stdin_v0.8.txt");

/// Bucket the emitted events by monitor name, then by event-kind. Lets tests assert
/// "for monitor WL-1, the latest ActiveWindow title is X" without dragging through ordering.
fn bucket(events: &[WmEvent]) -> HashMap<String, HashMap<&'static str, &WmEvent>> {
    let mut out: HashMap<String, HashMap<&'static str, &WmEvent>> = HashMap::new();
    for ev in events {
        let (mon, kind) = match ev {
            WmEvent::Tags { output, .. } => (output, "Tags"),
            WmEvent::Layout { output, .. } => (output, "Layout"),
            WmEvent::ActiveWindow { output, .. } => (output, "ActiveWindow"),
            _ => continue,
        };
        out.entry(mon.clone()).or_default().insert(kind, ev);
    }
    out
}

#[test]
fn parses_live_fixture_to_final_unmap_state() {
    // After the full fixture, the last frame in the capture is "all clients gone": empty
    // title/appid, tags `0 1 0 0` (occ=0, sel-tagset=1, focused-client=0, urg=0), layout "(@)".
    let mut r = StatusReducer::new();
    r.feed(FIXTURE);
    let events = r.drain_events();
    let mons = bucket(&events);
    let wl1 = mons.get("WL-1").expect("fixture covers monitor WL-1");

    if let Some(WmEvent::Layout { symbol, .. }) = wl1.get("Layout") {
        assert_eq!(symbol, "(@)", "layout symbol matches the spiral default");
    } else {
        panic!("missing Layout for WL-1");
    }
    if let Some(WmEvent::ActiveWindow { title, app_id, .. }) = wl1.get("ActiveWindow") {
        assert_eq!(title, "", "no focused client → empty title");
        assert_eq!(app_id, "", "no focused client → empty app_id");
    } else {
        panic!("missing ActiveWindow for WL-1");
    }
    if let Some(WmEvent::Tags { tags, .. }) = wl1.get("Tags") {
        // sel-tagset=1 → tag 0 (1-indexed name "1") is Active.
        assert_eq!(tags[0].state, TagState::Active);
        assert!(!tags[0].focused, "no focused client");
        assert!(!tags[0].occupied, "no clients at all");
    } else {
        panic!("missing Tags for WL-1");
    }
}

#[test]
fn fragmentation_byte_by_byte_yields_same_state() {
    // The whole point of the line-buffer: feeding the same bytes one at a time across many
    // `feed()` calls must produce the same final state. This is the natural property — if it
    // fails, the line splitter has an off-by-one in its trailing-byte handling.
    let mut bulk = StatusReducer::new();
    bulk.feed(FIXTURE);
    let bulk_events = bulk.drain_events();
    let bulk_state = bucket(&bulk_events);

    let mut frag = StatusReducer::new();
    for byte in FIXTURE.iter() {
        frag.feed(std::slice::from_ref(byte));
    }
    let frag_events = frag.drain_events();
    let frag_state = bucket(&frag_events);

    // Compare per-monitor finals — event ordering inside a snapshot isn't stable across the
    // HashMap iteration, but the per-monitor values are.
    for (mon, kinds) in &bulk_state {
        let other = frag_state.get(mon).expect("monitor present in both");
        for (kind, ev) in kinds {
            assert_eq!(
                other.get(kind),
                Some(ev),
                "{} {} differs between bulk and fragmented feeds",
                mon,
                kind
            );
        }
    }
}

#[test]
fn coalesce_latest_collapses_burst_to_single_snapshot() {
    // Three title overwrites in one feed → one ActiveWindow event, value = the last one. This is
    // the contract flag 6: backpressure handled by overwrite, not by queue. If this asserts on
    // count > 3, we've grown a queue somewhere.
    let mut r = StatusReducer::new();
    r.feed(b"WL-1 title FIRST\nWL-1 title SECOND\nWL-1 title THIRD\n");
    let events = r.drain_events();
    let active_titles: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            WmEvent::ActiveWindow { output, title, .. } if output == "WL-1" => Some(title.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        active_titles,
        vec!["THIRD"],
        "burst should coalesce to a single latest snapshot"
    );
}

#[test]
fn multi_monitor_state_is_independent() {
    // dwl emits one block per monitor; the parser must key state per monitor and not cross-pollinate.
    // The live fixture only has WL-1 (cage is single-output), so construct multi-monitor input
    // by hand here — the *wire format* is what the fixture pins, not the per-monitor key.
    let mut r = StatusReducer::new();
    r.feed(b"WL-1 title alpha\nWL-2 title beta\nWL-1 layout (@)\nWL-2 layout []=\n");
    let events = r.drain_events();
    let mons = bucket(&events);

    let wl1_title = match mons["WL-1"]["ActiveWindow"] {
        WmEvent::ActiveWindow { title, .. } => title.as_str(),
        _ => unreachable!(),
    };
    let wl2_title = match mons["WL-2"]["ActiveWindow"] {
        WmEvent::ActiveWindow { title, .. } => title.as_str(),
        _ => unreachable!(),
    };
    let wl1_layout = match mons["WL-1"]["Layout"] {
        WmEvent::Layout { symbol, .. } => symbol.as_str(),
        _ => unreachable!(),
    };
    let wl2_layout = match mons["WL-2"]["Layout"] {
        WmEvent::Layout { symbol, .. } => symbol.as_str(),
        _ => unreachable!(),
    };

    assert_eq!(wl1_title, "alpha");
    assert_eq!(wl2_title, "beta");
    assert_eq!(wl1_layout, "(@)");
    assert_eq!(wl2_layout, "[]=");
}

#[test]
fn title_with_spaces_is_preserved_verbatim() {
    // The captured fixture proved bash's PROMPT_COMMAND retitles to "gero@whatsit dwl" — a
    // multi-word title. `splitn(3, ' ')` keeps the third+ pieces in the value, so this works.
    // If someone "fixes" the parser to use `split_whitespace`, this test catches it.
    let mut r = StatusReducer::new();
    r.feed(b"WL-1 title gero@whatsit dwl\n");
    let events = r.drain_events();
    let title = events
        .iter()
        .find_map(|e| match e {
            WmEvent::ActiveWindow { title, .. } => Some(title.clone()),
            _ => None,
        })
        .expect("ActiveWindow expected");
    assert_eq!(title, "gero@whatsit dwl");
}

#[test]
fn tags_bitmask_decodes_per_field_in_pinned_order() {
    // The headline catch: in v0.8, the four `tags` bitmasks are occ / sel-tagset /
    // focused-client / urg — in *that exact order*. Construct a discriminating input so a
    // wrong-order parser fails this test (rather than passing on a fixture where 3 of the 4
    // happen to be zero).
    //
    //   occ=3 (bits 0,1), sel=1 (bit 0), focused=2 (bit 1), urg=4 (bit 2)
    let mut r = StatusReducer::new();
    r.feed(b"WL-1 tags 3 1 2 4\n");
    let events = r.drain_events();
    let tags = events
        .iter()
        .find_map(|e| match e {
            WmEvent::Tags { tags, .. } => Some(tags.clone()),
            _ => None,
        })
        .expect("Tags expected");

    // tag 0: occ=1, sel=1, focused=0, urg=0 → Active, not focused, occupied
    assert!(tags[0].occupied);
    assert!(!tags[0].focused);
    assert_eq!(tags[0].state, TagState::Active);
    // tag 1: occ=1, sel=0, focused=1, urg=0 → None (not selected, not urgent), focused, occupied
    assert!(tags[1].occupied);
    assert!(tags[1].focused);
    assert_eq!(tags[1].state, TagState::None);
    // tag 2: occ=0, sel=0, focused=0, urg=1 → Urgent (urgent wins over None), unoccupied
    assert!(!tags[2].occupied);
    assert!(!tags[2].focused);
    assert_eq!(tags[2].state, TagState::Urgent);
    // tag 3+: all zero
    assert_eq!(tags[3].state, TagState::None);
    assert!(!tags[3].occupied);
}

#[test]
fn empty_value_lines_register_as_field_updates() {
    // The "no focused client" case emits `WL-1 title \n` (trailing space, empty value). The
    // parser must mark the monitor dirty so the renderer clears the title — otherwise an unmap
    // leaves a stale title on the bar. (Subtle; the fixture covers it but the assertion makes
    // the intent explicit.)
    let mut r = StatusReducer::new();
    r.feed(b"WL-1 title old\n");
    let _ = r.drain_events(); // clear initial dirty
    r.feed(b"WL-1 title \n");
    let events = r.drain_events();
    let title = events
        .iter()
        .find_map(|e| match e {
            WmEvent::ActiveWindow { title, .. } => Some(title.clone()),
            _ => None,
        })
        .expect("dirty flag must trigger ActiveWindow re-emit on empty value");
    assert_eq!(title, "", "empty value overwrites the previous title");
}

#[test]
fn malformed_line_is_silently_skipped() {
    // Forward-compat: an unknown field name or a malformed tags line shouldn't crash the bar
    // or poison subsequent valid lines.
    let mut r = StatusReducer::new();
    r.feed(b"WL-1 future_field something_weird\nWL-1 tags not_a_number 1 2 3\nWL-1 title ok\n");
    let events = r.drain_events();
    let title = events
        .iter()
        .find_map(|e| match e {
            WmEvent::ActiveWindow { title, .. } => Some(title.clone()),
            _ => None,
        })
        .expect("valid line after malformed ones must still apply");
    assert_eq!(title, "ok");
}
