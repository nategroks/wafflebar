//! Output (monitor) ordering and `bar.monitor` selection — GTK-free, so it is unit-testable.
//!
//! **Why this is not just `sort_by(x)`.** Connector names (`DP-1`, `HDMI-A-1`) are assigned by
//! *port*, not by display: move a cable and the names swap. Anything keyed on them — a positional
//! bar, a kanshi profile — then follows the port instead of the screen. So selection here is by
//! *layout position*, and the ordering is total: `x`, then `y`, then connector name as the final
//! tie-break. Two outputs at the same origin (a mirrored pair, or a compositor that has not laid
//! them out yet) must still order the same way on every run, or "the left-most monitor" changes
//! identity between restarts for no visible reason.
//!
//! The GTK adapter lives in `crates/wafflebar/src/app.rs`; it converts `gdk::Monitor` into
//! [`OutputGeom`] and applies the indices this module returns.

/// One output's placement and identity, as much as selection needs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutputGeom {
    /// Connector name (`DP-1`). Port-assigned — identity for *matching*, never for ordering.
    pub connector: String,
    /// Monitor model as reported by EDID, when known.
    pub model: Option<String>,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl OutputGeom {
    /// Test/adapter helper: an output at `(x, y)` sized `w × h`.
    pub fn new(connector: impl Into<String>, x: i32, y: i32, width: i32, height: i32) -> Self {
        Self { connector: connector.into(), model: None, x, y, width, height }
    }
}

/// What `bar.monitor` asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitorSelect {
    /// One bar per output.
    All,
    /// The left-most / centre / right-most output *by layout position*.
    Left,
    Center,
    Right,
    /// An exact connector or model name.
    Named(String),
}

impl MonitorSelect {
    pub fn parse(want: &str) -> Self {
        match want.trim().to_ascii_lowercase().as_str() {
            "all" => Self::All,
            "left" => Self::Left,
            "right" => Self::Right,
            // "primary" is a layout position here, not an X11-style primary flag: Wayland has no
            // such flag, and the centre screen is what people mean on a three-head desk.
            "primary" | "center" => Self::Center,
            _ => Self::Named(want.trim().to_string()),
        }
    }
}

/// The result of resolving `bar.monitor` against the live output set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// Indices into the `outputs` slice that was passed in, in display order.
    pub indices: Vec<usize>,
    /// `true` when a named monitor did not match and the centre was substituted — the caller warns.
    pub fell_back: bool,
}

/// Indices of `outputs` in a total, deterministic left→right, top→bottom order.
///
/// Ties on `(x, y)` break on connector name so the order never depends on the order the compositor
/// happened to announce outputs in.
pub fn order_outputs(outputs: &[OutputGeom]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..outputs.len()).collect();
    idx.sort_by(|&a, &b| {
        let (oa, ob) = (&outputs[a], &outputs[b]);
        oa.x.cmp(&ob.x)
            .then(oa.y.cmp(&ob.y))
            .then_with(|| oa.connector.cmp(&ob.connector))
    });
    idx
}

/// Resolve `bar.monitor` to the outputs that should carry a bar.
///
/// An empty output set yields an empty selection (the caller draws an unanchored bar). A named
/// monitor that is not present falls back to the centre with `fell_back = true` — that is what makes
/// a config naming a display that is currently unplugged degrade to a visible bar instead of none.
pub fn select_outputs(outputs: &[OutputGeom], want: &str) -> Selection {
    let ordered = order_outputs(outputs);
    if ordered.is_empty() {
        return Selection { indices: Vec::new(), fell_back: false };
    }
    let center = || vec![ordered[ordered.len() / 2]];
    match MonitorSelect::parse(want) {
        MonitorSelect::All => Selection { indices: ordered, fell_back: false },
        MonitorSelect::Left => Selection { indices: vec![ordered[0]], fell_back: false },
        MonitorSelect::Right => {
            Selection { indices: vec![ordered[ordered.len() - 1]], fell_back: false }
        }
        MonitorSelect::Center => Selection { indices: center(), fell_back: false },
        MonitorSelect::Named(name) => {
            // Exact match on connector or model, scanned in display order so a duplicated model
            // name resolves to the left-most of them rather than to whichever came first on the
            // wire.
            let hit = ordered.iter().copied().find(|&i| {
                outputs[i].connector == name || outputs[i].model.as_deref() == Some(name.as_str())
            });
            match hit {
                Some(i) => Selection { indices: vec![i], fell_back: false },
                None => Selection { indices: center(), fell_back: true },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(outputs: &[OutputGeom], sel: &Selection) -> Vec<String> {
        sel.indices.iter().map(|&i| outputs[i].connector.clone()).collect()
    }

    #[test]
    fn single_output_is_left_center_and_right_at_once() {
        let m = [OutputGeom::new("eDP-1", 0, 0, 1920, 1080)];
        for want in ["left", "center", "primary", "right", "all"] {
            assert_eq!(names(&m, &select_outputs(&m, want)), ["eDP-1"], "want = {want}");
        }
    }

    #[test]
    fn side_by_side_orders_by_x_not_by_announcement() {
        // Announced right-first, as a compositor is free to do.
        let m = [
            OutputGeom::new("DP-2", 1920, 0, 1920, 1080),
            OutputGeom::new("DP-1", 0, 0, 1920, 1080),
        ];
        assert_eq!(names(&m, &select_outputs(&m, "left")), ["DP-1"]);
        assert_eq!(names(&m, &select_outputs(&m, "right")), ["DP-2"]);
        assert_eq!(names(&m, &select_outputs(&m, "all")), ["DP-1", "DP-2"]);
    }

    #[test]
    fn stacked_outputs_order_top_to_bottom() {
        // Same x — the old x-only sort left these in announcement order.
        let m = [
            OutputGeom::new("DP-2", 0, 1080, 1920, 1080),
            OutputGeom::new("DP-1", 0, 0, 1920, 1080),
        ];
        assert_eq!(names(&m, &select_outputs(&m, "all")), ["DP-1", "DP-2"]);
        assert_eq!(names(&m, &select_outputs(&m, "left")), ["DP-1"], "upper one is first");
    }

    #[test]
    fn identical_geometry_still_has_one_stable_order() {
        // A mirrored pair, or outputs the compositor has not laid out yet: the connector name is
        // the last tie-break, so this cannot flip between runs.
        let a = [
            OutputGeom::new("HDMI-A-1", 0, 0, 1920, 1080),
            OutputGeom::new("DP-1", 0, 0, 1920, 1080),
        ];
        let b = [a[1].clone(), a[0].clone()]; // announced the other way round
        assert_eq!(names(&a, &select_outputs(&a, "all")), ["DP-1", "HDMI-A-1"]);
        assert_eq!(names(&b, &select_outputs(&b, "all")), ["DP-1", "HDMI-A-1"]);
    }

    #[test]
    fn center_of_three_is_the_middle_screen_and_survives_unplugging_it() {
        let three = [
            OutputGeom::new("DP-1", 0, 0, 1920, 1080),
            OutputGeom::new("DP-2", 1920, 0, 1920, 1080),
            OutputGeom::new("DP-3", 3840, 0, 1920, 1080),
        ];
        assert_eq!(names(&three, &select_outputs(&three, "center")), ["DP-2"]);
        // Middle display unplugged: the remaining pair still resolves, no panic, no empty bar.
        let two = [three[0].clone(), three[2].clone()];
        assert_eq!(names(&two, &select_outputs(&two, "center")), ["DP-3"], "len/2 of two");
        assert_eq!(names(&two, &select_outputs(&two, "left")), ["DP-1"]);
    }

    #[test]
    fn named_matches_connector_or_model_and_falls_back_when_absent() {
        let mut m = [
            OutputGeom::new("DP-1", 0, 0, 1920, 1080),
            OutputGeom::new("DP-2", 1920, 0, 1920, 1080),
        ];
        m[1].model = Some("DELL U2723QE".to_string());

        let by_connector = select_outputs(&m, "DP-1");
        assert_eq!(names(&m, &by_connector), ["DP-1"]);
        assert!(!by_connector.fell_back);

        let by_model = select_outputs(&m, "DELL U2723QE");
        assert_eq!(names(&m, &by_model), ["DP-2"]);
        assert!(!by_model.fell_back);

        // Named display currently unplugged → centre, and the caller is told to warn.
        let missing = select_outputs(&m, "DP-9");
        assert_eq!(names(&m, &missing), ["DP-2"], "centre of two = index 1");
        assert!(missing.fell_back);
    }

    #[test]
    fn no_outputs_selects_nothing_rather_than_panicking() {
        let none: [OutputGeom; 0] = [];
        for want in ["left", "center", "right", "all", "DP-1"] {
            let sel = select_outputs(&none, want);
            assert!(sel.indices.is_empty(), "want = {want}");
            assert!(!sel.fell_back, "an empty set is not a fallback, want = {want}");
        }
    }

    #[test]
    fn select_is_case_insensitive_on_positions_but_not_on_names() {
        let m = [
            OutputGeom::new("DP-1", 0, 0, 1920, 1080),
            OutputGeom::new("DP-2", 1920, 0, 1920, 1080),
        ];
        assert_eq!(names(&m, &select_outputs(&m, "  LEFT ")), ["DP-1"]);
        assert_eq!(names(&m, &select_outputs(&m, "All")), ["DP-1", "DP-2"]);
        // Connector names are case-sensitive: "dp-1" is not a connector, so it falls back.
        assert!(select_outputs(&m, "dp-1").fell_back);
    }
}
