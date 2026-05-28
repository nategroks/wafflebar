//! Declarative config-field schema (F3 — preferences UI).
//!
//! A plugin describes its config *surface* — which option keys exist, their labels, kinds, and
//! constraints — but never its current *values*. The TOML is the single source of truth: the host
//! reads the current value per-key to populate a form and writes it back per-key on edit, then the
//! existing file-watch reload (F2) reflects the change to the plugin via `configure()`. The plugin
//! never holds or exposes a current-value snapshot across the trait boundary — same v1→v2 isolation
//! discipline as `configure()` returning a `Reaction` and `View` carrying no widgets.
//!
//! Note the `default` carried in each [`FieldKind`] is *static* schema data (the value used when the
//! key is absent from TOML), not a current-value snapshot — it tells the host what to show in an
//! otherwise-empty field, and must match the plugin's own fallback.

/// One field a plugin (or the bar itself) exposes to the preferences UI.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigField {
    /// The TOML option key under the module's `[[modules]]` table (or `[bar]`), e.g. `"format"`.
    pub key: String,
    /// Human-readable label for the form row.
    pub label: String,
    /// What input the host renders, with its constraints and static default.
    pub kind: FieldKind,
}

/// The input kind for a [`ConfigField`] — drives both the widget and the TOML value type.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    /// Free text → a `gtk::Entry`.
    Text { default: String },
    /// Like [`Text`](FieldKind::Text), plus a "Browse…" button that opens a file chooser — for a
    /// value that may be a name *or* a file path (e.g. an icon: a theme name or a PNG/SVG path).
    File { default: String },
    /// Bounded integer → a `gtk::SpinButton`.
    Int { min: i64, max: i64, default: i64 },
    /// On/off → a `gtk::Switch`.
    Bool { default: bool },
    /// One of a fixed set → a `gtk::DropDown`. `(value, label)` pairs; `value` is written to TOML.
    Choice { options: Vec<(String, String)>, default: String },
    /// A field that exists but isn't UI-editable yet. Renders as a static note, not an input —
    /// names the limitation and its future closer rather than greying out a control. General
    /// read-only vocabulary; currently unused after F4 moved the launcher to `DesktopList`.
    Note(String),
    /// An ordered list of `.desktop` references (the launcher's `items`). The host renders an
    /// add/remove/reorder editor with an application picker (F4); the TOML value is an array of
    /// strings. The field's `key` (on [`ConfigField`]) names the array.
    DesktopList,
}

impl ConfigField {
    pub fn text(key: &str, label: &str, default: &str) -> Self {
        Self { key: key.into(), label: label.into(), kind: FieldKind::Text { default: default.into() } }
    }

    /// Text entry + a "Browse…" file chooser (for a name-or-path value, e.g. an icon).
    pub fn file(key: &str, label: &str, default: &str) -> Self {
        Self { key: key.into(), label: label.into(), kind: FieldKind::File { default: default.into() } }
    }

    pub fn int(key: &str, label: &str, min: i64, max: i64, default: i64) -> Self {
        Self { key: key.into(), label: label.into(), kind: FieldKind::Int { min, max, default } }
    }

    pub fn bool(key: &str, label: &str, default: bool) -> Self {
        Self { key: key.into(), label: label.into(), kind: FieldKind::Bool { default } }
    }

    /// `options` are `(toml-value, display-label)` pairs; `default` is the toml-value used when the
    /// key is absent (should be one of the option values).
    pub fn choice(key: &str, label: &str, options: &[(&str, &str)], default: &str) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            kind: FieldKind::Choice {
                options: options.iter().map(|(v, l)| (v.to_string(), l.to_string())).collect(),
                default: default.into(),
            },
        }
    }

    /// A non-editable informational row. `key` is informational only (nothing is written).
    pub fn note(key: &str, label: &str, text: &str) -> Self {
        Self { key: key.into(), label: label.into(), kind: FieldKind::Note(text.into()) }
    }

    /// An ordered `.desktop` reference list edited via the application picker (F4).
    pub fn desktop_list(key: &str, label: &str) -> Self {
        Self { key: key.into(), label: label.into(), kind: FieldKind::DesktopList }
    }
}
