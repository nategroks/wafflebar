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
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context, Result};
use clap::Parser;
use gtk4::prelude::*;
use gtk4::gio::ApplicationFlags;
use gtk4::Application;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use wafflebar_core::{Config, GridEngine};

const APP_ID: &str = "dev.wafflebar.Wafflebar";

/// Set on SIGTERM/SIGINT (via [`signal_handler`]) and on `WmConnection::closed()` after a
/// dispatch (via the WM fd watch). The `build_bars` shutdown timer polls this and, when set,
/// runs the same idempotent [`crate::feeds::someblocks::SomeblocksIntake::cleanup`] from
/// **every** code path before calling `app.quit()` — NATEWM_MODE flag 5 amendment, wired.
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// True iff a graceful exit has been requested.
pub fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::Relaxed)
}

/// Flip the shutdown flag from anywhere (signal handler, WM EOF observer, …).
pub fn request_shutdown() {
    SHUTDOWN_REQUESTED.store(true, Ordering::Relaxed);
}

/// Async-signal-safe — only stores into the atomic. No allocation, no logging, no FFI beyond
/// the atomic store itself (Rust's `Ordering::Relaxed` lowers to a single store).
unsafe extern "C" fn signal_handler(_signum: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Relaxed);
}

fn install_signal_handlers() -> Result<()> {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = signal_handler as *const () as usize;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        for sig in [libc::SIGTERM, libc::SIGINT] {
            if libc::sigaction(sig, &action, std::ptr::null_mut()) != 0 {
                bail!(
                    "sigaction signum {sig}: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
    }
    Ok(())
}

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

    // Install signal handlers BEFORE GTK so SIGTERM/SIGINT routes to our static flag instead of
    // an inherited default (or GTK's own SIGINT trap). The shutdown timer in build_bars observes
    // the flag and runs the feed cleanup before calling app.quit().
    install_signal_handlers().context("installing SIGTERM/SIGINT handlers")?;

    let Cli {
        config: config_arg,
        replace_notifications,
        dwl_status_stdin,
        status_socket,
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
    // Feed socket path policy (NATEWM_MODE flag 4): explicit `--status-socket` wins; otherwise
    // try `$XDG_RUNTIME_DIR/wafflebar/feed.sock`. Both absent → `None` (feed disabled). No silent
    // fallback to `/tmp` — that's the multi-user trap the policy refuses.
    let feed_socket = wafflebar_core::FeedSocketConfig {
        path: status_socket.or_else(|| {
            std::env::var_os("XDG_RUNTIME_DIR").map(|x| {
                std::path::PathBuf::from(x)
                    .join("wafflebar")
                    .join("feed.sock")
            })
        }),
    };
    if feed_socket.path.is_none() {
        info!("feed disabled: XDG_RUNTIME_DIR unset and no --status-socket override");
    }
    // `WAFFLEBAR_NON_UNIQUE=1` opts out of the GTK Application unique-instance behavior — needed
    // for nested test runs (cage'd dwl) where the user's daily-driver wafflebar already owns the
    // APP_ID on the session bus, which would otherwise remote-activate our new process and exit
    // it before it can build any bar. Production: leave unset; the unique-instance behavior is
    // correct for daily use (a second `wafflebar` invocation re-activates the running one
    // instead of double-binding).
    let mut builder = Application::builder().application_id(APP_ID);
    if std::env::var_os("WAFFLEBAR_NON_UNIQUE").is_some() {
        builder = builder.flags(ApplicationFlags::NON_UNIQUE);
        info!("WAFFLEBAR_NON_UNIQUE=1: GTK Application unique-instance behavior disabled");
    }
    let app = builder.build();
    app.connect_activate(move |app| {
        app::build_bars(
            app,
            &config,
            &engine,
            config_path.as_deref(),
            replace_notifications,
            backend_select,
            feed_socket.clone(),
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
