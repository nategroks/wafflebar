use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Top-level panel configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanelConfig {
    #[serde(default)]
    pub panel: PanelSettings,
    #[serde(default)]
    pub theme: ThemeSettings,
    #[serde(default)]
    pub segments: SegmentsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanelSettings {
    /// Panel position on screen.
    #[serde(default = "default_position")]
    pub position: String,
    /// Panel height in pixels.
    #[serde(default = "default_height")]
    pub height: u32,
    /// Monitor selection: "all", "primary", or monitor name.
    #[serde(default = "default_monitor")]
    pub monitor: String,
    /// Icon theme name (follows freedesktop icon theme spec).
    #[serde(default = "default_icon_theme")]
    pub icon_theme: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeSettings {
    #[serde(default = "default_bg")]
    pub background: String,
    #[serde(default = "default_fg")]
    pub foreground: String,
    #[serde(default = "default_font_family")]
    pub font_family: String,
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    /// Renderer backend: "auto", "cpu", or "gpu".
    #[serde(default = "default_renderer")]
    pub renderer: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SegmentsConfig {
    #[serde(default)]
    pub left: Vec<WidgetConfig>,
    #[serde(default)]
    pub center: Vec<WidgetConfig>,
    #[serde(default)]
    pub right: Vec<WidgetConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetConfig {
    pub widget: String,
    #[serde(flatten)]
    pub settings: toml::Table,
}

fn default_position() -> String {
    "top".into()
}
fn default_height() -> u32 {
    32
}
fn default_monitor() -> String {
    "all".into()
}
fn default_icon_theme() -> String {
    "hicolor".into()
}
fn default_bg() -> String {
    "#2e3440".into()
}
fn default_fg() -> String {
    "#eceff4".into()
}
fn default_font_family() -> String {
    "sans-serif".into()
}
fn default_font_size() -> f32 {
    14.0
}
fn default_renderer() -> String {
    "auto".into()
}

impl Default for PanelSettings {
    fn default() -> Self {
        Self {
            position: default_position(),
            height: default_height(),
            monitor: default_monitor(),
            icon_theme: default_icon_theme(),
        }
    }
}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            background: default_bg(),
            foreground: default_fg(),
            font_family: default_font_family(),
            font_size: default_font_size(),
            renderer: default_renderer(),
        }
    }
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            panel: PanelSettings::default(),
            theme: ThemeSettings::default(),
            segments: SegmentsConfig {
                left: Vec::new(),
                center: Vec::new(),
                right: vec![WidgetConfig {
                    widget: "clock".into(),
                    settings: toml::Table::new(),
                }],
            },
        }
    }
}

impl PanelConfig {
    /// Returns the XDG config path for wafflebar.
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("wafflebar")
            .join("config.toml")
    }

    /// Load config from the default XDG path, or return defaults if not found.
    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if path.exists() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read config from {}", path.display()))?;
            let config: PanelConfig = toml::from_str(&content)
                .with_context(|| format!("failed to parse config from {}", path.display()))?;
            Ok(config)
        } else {
            tracing::info!("no config file found at {}, using defaults", path.display());
            Ok(Self::default())
        }
    }

    /// Write the default config to the XDG path (creates directories as needed).
    pub fn write_default() -> Result<PathBuf> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(&Self::default())?;
        std::fs::write(&path, content)?;
        Ok(path)
    }
}
