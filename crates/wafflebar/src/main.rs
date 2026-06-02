//! wafflebar — a griddy Wayland status bar. Binary entry point.
//!
//! Responsibilities here are intentionally thin: parse CLI args, initialise logging, load
//! and validate the config (failing fast with a clear message), then hand off to [`app`]
//! which owns all GTK/layer-shell concerns.

mod app;
mod config_reload;
mod dropdown;
mod event_loop;
mod feeds;
mod menu;
mod notify;
mod notify_ui;
mod plugins;
mod prefs;
mod theme;
mod render;
mod shell;
mod wm;

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
    /// Take over `org.freedesktop.Notifications` from an existing daemon (mako, dunst, …).
    #[arg(long)]
    replace_notifications: bool,
    /// Read dwl's `-s` status protocol from our stdin (vanilla-dwl mode, no IPC patch required).
    /// Implies that this process was spawned by `dwl -s wafflebar`. Disables `WmCommand`
    /// execution (the stdin channel is one-way). See `docs/NATEWM_MODE.md`.
    #[arg(long)]
    dwl_status_stdin: bool,
    /// UNIX socket path for the someblocks-style external block feed (clock / mem / …).
    /// Default: `$XDG_RUNTIME_DIR/wafflebar/feed.sock`. Unset = feed disabled.
    #[arg(long, value_name = "PATH")]
    status_socket: Option<PathBuf>,
}

fn main() -> Result<()> {
    // GTK4's default Vulkan renderer spams `VK_SUBOPTIMAL_KHR` on NVIDIA + wlroots
    // layer-shell surfaces. A status bar doesn't need Vulkan, so default to the new
    // OpenGL renderer ("ngl" — note plain "gl" warns on GTK >= 4.18). User override honored.
    if std::env::var_os("GSK_RENDERER").is_none() {
        std::env::set_var("GSK_RENDERER", "ngl");
    }

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_env("WAFFLEBAR_LOG").unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let Cli {
        config: config_arg,
        replace_notifications,
        dwl_status_stdin,
        status_socket: _status_socket,
    } = Cli::parse();
    let (config, config_path) = load_config(config_arg)?;

    // Validate the grid up front so a bad layout fails before we open any windows.
    let engine = GridEngine::build(&config).context("invalid bar layout")?;
    info!(
        modules = engine.placements.len(),
        grid = format!("{}x{}", engine.rows, engine.cols),
        "layout validated"
    );

    let backend_select = wm::BackendSelect {
        force_dwl_stdin: dwl_status_stdin,
    };
    // `_status_socket` plumbs through to the someblocks intake in NATEWM_MODE step 2 — kept off
    // the call signature today so the scaffold doesn't fake-wire something it can't honor.
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(move |app| {
        app::build_bars(
            app,
            &config,
            &engine,
            config_path.as_deref(),
            replace_notifications,
            backend_select,
        );
    });

    // We parse our own args with clap, so don't let GTK touch argv.
    let empty: [&str; 0] = [];
    app.run_with_args(&empty);
    Ok(())
}

/// Load config from an explicit path, the XDG default, or fall back to a built-in default. Returns
/// the path it loaded from (for the live-reload watcher) — `None` when running on defaults.
fn load_config(explicit: Option<PathBuf>) -> Result<(Config, Option<PathBuf>)> {
    let path = explicit.or_else(default_config_path);
    match path {
        Some(p) if p.exists() => {
            info!(path = %p.display(), "loading config");
            let cfg = Config::load(&p).with_context(|| format!("loading config {}", p.display()))?;
            Ok((cfg, Some(p)))
        }
        Some(p) => {
            warn!(path = %p.display(), "no config file; using built-in default");
            Ok((Config::default(), None))
        }
        None => {
            warn!("could not determine config path; using built-in default");
            Ok((Config::default(), None))
        }
    }
}

fn default_config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("wafflebar").join("config.toml"))
}
