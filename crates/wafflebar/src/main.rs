//! wafflebar - a griddy bar with yummy customization. No bloat. Built in Rust.

use anyhow::{bail, Result};
use wafflebar_core::config::PanelConfig;
use wafflebar_core::panel::{Panel, Segment, SegmentAlign};
use wafflebar_core::widget::Widget;
use wafflebar_widgets::ClockWidget;

fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("wafflebar starting");

    // Parse CLI args
    let args: Vec<String> = std::env::args().collect();
    let mut backend = None;
    let mut config_path = None;
    let mut init_config = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--backend" | "-b" => {
                i += 1;
                if i < args.len() {
                    backend = Some(args[i].clone());
                }
            }
            "--config" | "-c" => {
                i += 1;
                if i < args.len() {
                    config_path = Some(args[i].clone());
                }
            }
            "--init-config" => {
                init_config = true;
            }
            "--help" | "-h" => {
                println!("wafflebar - a griddy bar with yummy customization");
                println!();
                println!("USAGE:");
                println!("    wafflebar [OPTIONS]");
                println!();
                println!("OPTIONS:");
                println!(
                    "    -b, --backend <BACKEND>    Renderer backend: x11 (default: auto-detect)"
                );
                println!("    -c, --config <PATH>        Config file path (default: ~/.config/wafflebar/config.toml)");
                println!("        --init-config          Write default config and exit");
                println!("    -h, --help                 Show this help message");
                return Ok(());
            }
            other => {
                bail!("unknown argument: {other}. Use --help for usage.");
            }
        }
        i += 1;
    }

    // Handle --init-config
    if init_config {
        let path = PanelConfig::write_default()?;
        println!("wrote default config to {}", path.display());
        return Ok(());
    }

    // Load config
    let config = if let Some(path) = config_path {
        let content = std::fs::read_to_string(&path)?;
        toml::from_str(&content)?
    } else {
        PanelConfig::load()?
    };

    tracing::info!(
        "config: position={}, height={}, renderer={}",
        config.panel.position,
        config.panel.height,
        config.theme.renderer
    );

    // Build widget instances from config
    let segments = build_segments(&config);

    // Create panel
    let panel = Panel::new(config, segments);

    // Select and run backend
    let backend = backend.unwrap_or_else(|| "x11".to_string());
    match backend.as_str() {
        "x11" => {
            tracing::info!("starting X11 backend");
            wafflebar_x11::X11Backend::run(panel)?;
        }
        other => {
            bail!("unknown backend: {other}. Supported: x11");
        }
    }

    Ok(())
}

/// Build segments with widget instances from the panel config.
fn build_segments(config: &PanelConfig) -> Vec<Segment> {
    let mut segments = Vec::new();

    let build_widgets =
        |widget_configs: &[wafflebar_core::config::WidgetConfig]| -> Vec<Box<dyn Widget>> {
            widget_configs
                .iter()
                .enumerate()
                .filter_map(|(idx, wc)| -> Option<Box<dyn Widget>> {
                    match wc.widget.as_str() {
                        "clock" => {
                            let format = wc
                                .settings
                                .get("format")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            Some(Box::new(ClockWidget::new(format!("clock-{idx}"), format)))
                        }
                        other => {
                            tracing::warn!("unknown widget type: {other}, skipping");
                            None
                        }
                    }
                })
                .collect()
        };

    if !config.segments.left.is_empty() {
        segments.push(Segment {
            align: SegmentAlign::Left,
            widgets: build_widgets(&config.segments.left),
        });
    }

    if !config.segments.center.is_empty() {
        segments.push(Segment {
            align: SegmentAlign::Center,
            widgets: build_widgets(&config.segments.center),
        });
    }

    if !config.segments.right.is_empty() {
        segments.push(Segment {
            align: SegmentAlign::Right,
            widgets: build_widgets(&config.segments.right),
        });
    }

    // If no segments defined, add a default clock on the right
    if segments.is_empty() {
        segments.push(Segment {
            align: SegmentAlign::Right,
            widgets: vec![Box::new(ClockWidget::new(
                "clock-default".to_string(),
                None,
            ))],
        });
    }

    segments
}
