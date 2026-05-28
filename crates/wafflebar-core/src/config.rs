//! wafflebar configuration: the versioned TOML schema and its loader.
//!
//! The schema is intentionally small and hand-editable. A `schema` version is stored at
//! the top level so the (future) GUI configurator can refuse to write configs it does not
//! understand. Per-module options live in a flattened table ([`ModuleConfig::options`]) so
//! new modules can add settings without changing this struct.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The config schema version this build understands. Bumped on breaking schema changes.
pub const SCHEMA_VERSION: u32 = 1;

/// Errors that can occur while loading a [`Config`].
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The config file could not be read.
    #[error("reading config file {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// The config file was not valid TOML / did not match the schema.
    #[error("parsing config: {0}")]
    Parse(#[from] toml::de::Error),
    /// The config declares a schema version this build cannot handle.
    #[error("config schema version {found} is newer than supported version {supported}; upgrade wafflebar")]
    UnsupportedSchema { found: u32, supported: u32 },
}

/// Top-level wafflebar configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Schema version (see [`SCHEMA_VERSION`]).
    #[serde(default = "default_schema")]
    pub schema: u32,
    /// Bar-wide settings (position, height, theme).
    #[serde(default)]
    pub bar: BarConfig,
    /// The grid track the modules are placed on.
    #[serde(default)]
    pub grid: GridConfig,
    /// Modules placed on the bar, in declaration order.
    #[serde(default, rename = "modules")]
    pub modules: Vec<ModuleConfig>,
}

fn default_schema() -> u32 {
    SCHEMA_VERSION
}

/// Where the bar sits and how it looks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BarConfig {
    /// Which monitor(s): `"primary"` (default) / `"left"` / `"center"` / `"right"` (by layout
    /// position), `"all"` (one bar per monitor), or an exact connector / model name.
    #[serde(default = "default_monitor")]
    pub monitor: String,
    /// Top or bottom edge.
    #[serde(default)]
    pub position: Position,
    /// How modules are arranged: `"pack"` (default — xfce4-panel-style, group by `align` and pack
    /// tight to each edge) or `"grid"` (explicit `rows × columns` cell placement).
    #[serde(default)]
    pub layout: Layout,
    /// Bar thickness in pixels (also the layer-shell exclusive zone).
    #[serde(default = "default_height")]
    pub height: u32,
    /// Optional path to a GTK CSS theme file.
    #[serde(default)]
    pub theme: Option<String>,
}

impl Default for BarConfig {
    fn default() -> Self {
        Self {
            monitor: default_monitor(),
            position: Position::Top,
            layout: Layout::default(),
            height: default_height(),
            theme: None,
        }
    }
}

fn default_monitor() -> String {
    // Panels default to a single screen (like xfce4-panel / plasma); use "all" for one per monitor.
    "primary".to_string()
}
fn default_height() -> u32 {
    26
}

/// Which screen edge the bar anchors to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    #[default]
    Top,
    Bottom,
}

/// How the bar arranges its modules.
///
/// `Pack` (default) is the xfce4-panel model: modules are grouped by their [`Align`]
/// (`Start` packs flush-left, `End` flush-right, `Center` centered), sized to their content, with
/// the slack between groups absorbed automatically — `cell`/`colspan`/`rowspan` are ignored.
/// `Grid` keeps the explicit `rows × columns` track with per-module `cell` placement. Any config
/// with `rows > 1` uses the grid path regardless of this setting (packing is single-row).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    #[default]
    Pack,
    Grid,
}

/// The grid track: a `rows × columns` matrix that modules are placed into.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridConfig {
    /// Number of rows (>= 1).
    #[serde(default = "default_rows")]
    pub rows: u32,
    /// Number of columns (>= 1).
    #[serde(default = "default_columns")]
    pub columns: u32,
}

impl Default for GridConfig {
    fn default() -> Self {
        Self {
            rows: default_rows(),
            columns: default_columns(),
        }
    }
}

fn default_rows() -> u32 {
    1
}
fn default_columns() -> u32 {
    12
}

/// One module placed on the bar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleConfig {
    /// Module type, e.g. `"clock"`, `"tags"`, `"cpu"`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Where the module sits on the grid.
    pub cell: Cell,
    /// Alignment of the widget within its cell.
    #[serde(default)]
    pub align: Align,
    /// Per-module options (e.g. clock `format`), kept generic so modules own their schema.
    #[serde(flatten, default)]
    pub options: BTreeMap<String, toml::Value>,
}

impl ModuleConfig {
    /// A minimal config for a kind with no options and a 1×1 cell at the origin. Used to build a
    /// throwaway reducer purely to read its static `config_schema()` (F3) — never placed on a grid.
    pub fn bare(kind: &str) -> Self {
        Self {
            kind: kind.to_string(),
            cell: Cell { row: 0, col: 0, rowspan: 1, colspan: 1 },
            align: Align::default(),
            options: BTreeMap::new(),
        }
    }

    /// Fetch a string option by key (e.g. `format` for the clock module).
    pub fn opt_str(&self, key: &str) -> Option<&str> {
        self.options.get(key).and_then(toml::Value::as_str)
    }

    /// Fetch an integer option by key.
    pub fn opt_i64(&self, key: &str) -> Option<i64> {
        self.options.get(key).and_then(toml::Value::as_integer)
    }

    /// Fetch a boolean option by key (e.g. the separator's `expand`).
    pub fn opt_bool(&self, key: &str) -> Option<bool> {
        self.options.get(key).and_then(toml::Value::as_bool)
    }

    /// Fetch an array-of-strings option by key (e.g. the launcher's `items`).
    /// Non-array values and non-string elements are dropped; missing key → empty.
    pub fn opt_str_list(&self, key: &str) -> Vec<String> {
        self.options
            .get(key)
            .and_then(toml::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(toml::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A grid cell occupied by a module, with optional spans.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub row: u32,
    pub col: u32,
    #[serde(default = "default_span")]
    pub rowspan: u32,
    #[serde(default = "default_span")]
    pub colspan: u32,
}

fn default_span() -> u32 {
    1
}

/// Widget alignment within a grid cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
    Fill,
}

impl Config {
    /// Load and validate a config from `path`.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&text)
    }

    /// Parse and validate a config from a TOML string.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let cfg: Config = toml::from_str(text)?;
        if cfg.schema > SCHEMA_VERSION {
            return Err(ConfigError::UnsupportedSchema {
                found: cfg.schema,
                supported: SCHEMA_VERSION,
            });
        }
        Ok(cfg)
    }
}

impl Default for Config {
    /// A minimal sensible default (used when no config file exists): a top pack-bar with an
    /// applications menu flush-left and a clock flush-right.
    fn default() -> Self {
        let appmenu = ModuleConfig {
            kind: "appmenu".to_string(),
            cell: Cell {
                row: 0,
                col: 0,
                rowspan: 1,
                colspan: 1,
            },
            align: Align::Start,
            options: BTreeMap::new(),
        };
        let mut clock = ModuleConfig {
            kind: "clock".to_string(),
            cell: Cell {
                row: 0,
                col: 1,
                rowspan: 1,
                colspan: 1,
            },
            align: Align::End,
            options: BTreeMap::new(),
        };
        clock.options.insert(
            "format".to_string(),
            toml::Value::String("%a %d %b   %H:%M".to_string()),
        );
        Self {
            schema: SCHEMA_VERSION,
            bar: BarConfig::default(),
            grid: GridConfig {
                rows: 1,
                columns: 2,
            },
            modules: vec![appmenu, clock],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config() {
        let toml = r#"
            schema = 1
            [bar]
            height = 30
            [grid]
            rows = 1
            columns = 3
            [[modules]]
            type = "clock"
            cell = { row = 0, col = 1 }
            format = "%H:%M"
        "#;
        let cfg = Config::parse(toml).expect("should parse");
        assert_eq!(cfg.schema, 1);
        assert_eq!(cfg.bar.height, 30);
        assert_eq!(cfg.grid.columns, 3);
        assert_eq!(cfg.modules.len(), 1);
        assert_eq!(cfg.modules[0].kind, "clock");
        assert_eq!(cfg.modules[0].opt_str("format"), Some("%H:%M"));
        assert_eq!(cfg.modules[0].cell.colspan, 1); // default span
    }

    #[test]
    fn defaults_fill_in() {
        let cfg = Config::parse("schema = 1").expect("parse");
        assert_eq!(cfg.bar.position, Position::Top);
        assert_eq!(cfg.bar.height, 26);
        assert_eq!(cfg.grid.rows, 1);
        assert!(cfg.modules.is_empty());
    }

    #[test]
    fn rejects_future_schema() {
        let err = Config::parse("schema = 9999").unwrap_err();
        assert!(matches!(err, ConfigError::UnsupportedSchema { found: 9999, .. }));
    }

    #[test]
    fn default_config_is_appmenu_and_clock_packed() {
        let cfg = Config::default();
        assert_eq!(cfg.bar.layout, Layout::Pack);
        let kinds: Vec<&str> = cfg.modules.iter().map(|m| m.kind.as_str()).collect();
        assert_eq!(kinds, ["appmenu", "clock"]);
        assert_eq!(cfg.modules[0].align, Align::Start, "menu packs left");
        assert_eq!(cfg.modules[1].align, Align::End, "clock packs right");
    }
}
