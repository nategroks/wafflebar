//! wafflebar — a griddy Wayland status bar. Binary entry point.
//!
//! Responsibilities here are intentionally thin: parse CLI args, initialise logging, load
//! and validate the config (failing fast with a clear message), then hand off to [`app`]
//! which owns all GTK/layer-shell concerns.

mod app;
mod modules;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use gtk4::prelude::*;
use gtk4::Application;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use wafflebar_core::{Config, GridEngine};

const APP_ID: &str = "dev.wafflebar.Wafflebar";

/// A griddy Wayland status bar. No bloat.
#[derive(Debug, Parser)]
#[command(name = "wafflebar", version, about)]
struct Cli {
    /// Path to a config file (default: $XDG_CONFIG_HOME/wafflebar/config.toml).
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,
}

fn main() -> Result<()> {
    // GTK4's default Vulkan renderer spams `VK_SUBOPTIMAL_KHR` on NVIDIA + wlroots
    // layer-shell surfaces. A status bar doesn't need Vulkan, so default to the GL
    // renderer — unless the user has explicitly chosen one via GSK_RENDERER.
    if std::env::var_os("GSK_RENDERER").is_none() {
        std::env::set_var("GSK_RENDERER", "gl");
    }

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_env("WAFFLEBAR_LOG").unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let cli = Cli::parse();
    let config = load_config(cli.config)?;

    // Validate the grid up front so a bad layout fails before we open any windows.
    let engine = GridEngine::build(&config).context("invalid bar layout")?;
    info!(
        modules = engine.placements.len(),
        grid = format!("{}x{}", engine.rows, engine.cols),
        "layout validated"
    );

    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(move |app| {
        app::build_bars(app, &config, &engine);
    });

    // We parse our own args with clap, so don't let GTK touch argv.
    let empty: [&str; 0] = [];
    app.run_with_args(&empty);
    Ok(())
}

/// Load config from an explicit path, the XDG default, or fall back to a built-in default.
fn load_config(explicit: Option<PathBuf>) -> Result<Config> {
    let path = explicit.or_else(default_config_path);
    match path {
        Some(p) if p.exists() => {
            info!(path = %p.display(), "loading config");
            Config::load(&p).with_context(|| format!("loading config {}", p.display()))
        }
        Some(p) => {
            warn!(path = %p.display(), "no config file; using built-in default");
            Ok(Config::default())
        }
        None => {
            warn!("could not determine config path; using built-in default");
            Ok(Config::default())
        }
    }
}

fn default_config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("wafflebar").join("config.toml"))
}
