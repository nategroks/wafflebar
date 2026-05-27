//! Host executor: performs [`Launch`] intents emitted by plugins (the launcher, today).
//!
//! This is the **only** place `zbus` and process spawning appear in B2b — same discipline as the
//! `WmCommand` sink. Plugins emit pure-data `Launch` intents; the executor turns them into DBus
//! activations or spawned processes. Keeping it behind the [`DbusActivator`]/[`Spawner`] traits
//! lets the failure modes (activation timeout, spawn failure) be tested with fakes, no live bus.

use std::collections::HashMap;
use std::time::Duration;

use tracing::{error, warn};
use wafflebar_core::Launch;

/// How long to wait for DBus activation before giving up and falling back to `Exec=`.
const DBUS_ACTIVATE_TIMEOUT: Duration = Duration::from_secs(2);

/// Activates an application over `org.freedesktop.Application`. Abstracted so tests can inject a
/// never-responding / failing bus without a real session.
pub trait DbusActivator {
    fn activate(
        &self,
        bus_name: &str,
        object_path: &str,
        files: &[String],
    ) -> Result<(), ActivateError>;
}

/// Spawns a process from an already-expanded argv. Abstracted for tests.
pub trait Spawner {
    fn spawn(&self, argv: &[String]) -> std::io::Result<()>;
}

#[derive(Debug)]
pub enum ActivateError {
    /// The activation didn't complete within [`DBUS_ACTIVATE_TIMEOUT`].
    Timeout,
    /// The bus returned an error (no such service, method failed, no session bus, …).
    Bus(String),
}

impl std::fmt::Display for ActivateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActivateError::Timeout => write!(f, "DBus activation timed out"),
            ActivateError::Bus(e) => write!(f, "DBus activation error: {e}"),
        }
    }
}

/// Performs `Launch` intents using a [`DbusActivator`] and a [`Spawner`].
pub struct Executor<D, S> {
    dbus: D,
    spawner: S,
}

impl Executor<ZbusActivator, ProcessSpawner> {
    /// The real executor: session-bus activation + `std::process` spawning.
    pub fn real() -> Self {
        Self::new(ZbusActivator, ProcessSpawner)
    }
}

impl<D: DbusActivator, S: Spawner> Executor<D, S> {
    pub fn new(dbus: D, spawner: S) -> Self {
        Self { dbus, spawner }
    }

    /// Perform one launch intent. Never panics: failures are logged (and, for DBus, fall back to
    /// the carried `Exec=`).
    pub fn execute(&self, launch: &Launch) {
        match launch {
            Launch::Exec { argv } => self.spawn(argv),
            Launch::DBus {
                bus_name,
                object_path,
                files,
                fallback_exec,
                ..
            } => {
                if let Err(e) = self.dbus.activate(bus_name, object_path, files) {
                    // Deliberate xfce4-panel/GIO behavior: on activation failure, spawn Exec=.
                    warn!(bus_name, error = %e, "DBus activation failed; falling back to Exec=");
                    if fallback_exec.is_empty() {
                        error!(bus_name, "no Exec= fallback for failed DBus activation; giving up");
                    } else {
                        self.spawn(fallback_exec);
                    }
                }
            }
        }
    }

    fn spawn(&self, argv: &[String]) {
        if argv.is_empty() {
            error!("launch: empty argv, nothing to spawn");
            return;
        }
        if let Err(e) = self.spawner.spawn(argv) {
            // argv[0] not on PATH, not executable, etc. Log and move on — never crash the bar.
            error!(?argv, error = %e, "launch: failed to spawn process");
            // TODO(notifications): surface launch failures to the user once the notifications
            // shell (src/shell/notifications) lands.
        }
    }
}

/// Real session-bus activator. Runs the blocking `zbus` call on a worker thread so the activation
/// can be bounded by [`DBUS_ACTIVATE_TIMEOUT`] without blocking the GLib main loop indefinitely.
pub struct ZbusActivator;

impl DbusActivator for ZbusActivator {
    fn activate(
        &self,
        bus_name: &str,
        object_path: &str,
        files: &[String],
    ) -> Result<(), ActivateError> {
        let (bus_name, object_path, files) =
            (bus_name.to_owned(), object_path.to_owned(), files.to_vec());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(zbus_activate(&bus_name, &object_path, &files));
        });
        match rx.recv_timeout(DBUS_ACTIVATE_TIMEOUT) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(ActivateError::Bus(e)),
            Err(_) => Err(ActivateError::Timeout), // worker still running; the bar moves on
        }
    }
}

/// Call `Activate` (no files) or `Open` (with files) on `org.freedesktop.Application`.
fn zbus_activate(bus_name: &str, object_path: &str, files: &[String]) -> Result<(), String> {
    use zbus::zvariant::Value;
    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let platform_data: HashMap<String, Value> = HashMap::new();
    let iface = Some("org.freedesktop.Application");
    let dest = Some(bus_name);
    if files.is_empty() {
        conn.call_method(dest, object_path, iface, "Activate", &(platform_data,))
            .map_err(|e| e.to_string())?;
    } else {
        conn.call_method(dest, object_path, iface, "Open", &(files, platform_data))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Real process spawner.
pub struct ProcessSpawner;

impl Spawner for ProcessSpawner {
    fn spawn(&self, argv: &[String]) -> std::io::Result<()> {
        let (cmd, args) = argv.split_first().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty argv")
        })?;
        std::process::Command::new(cmd).args(args).spawn()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Records every argv it's asked to spawn; can be set to fail (e.g. argv[0] not on PATH).
    struct FakeSpawner {
        calls: RefCell<Vec<Vec<String>>>,
        fail: bool,
    }
    impl FakeSpawner {
        fn ok() -> Self {
            Self { calls: RefCell::new(Vec::new()), fail: false }
        }
        fn failing() -> Self {
            Self { calls: RefCell::new(Vec::new()), fail: true }
        }
    }
    impl Spawner for FakeSpawner {
        fn spawn(&self, argv: &[String]) -> std::io::Result<()> {
            self.calls.borrow_mut().push(argv.to_vec());
            if self.fail {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"))
            } else {
                Ok(())
            }
        }
    }

    /// A DBus activator that always fails the given way (stands in for a never-responding bus).
    struct FakeDbus(ActivateError);
    impl DbusActivator for FakeDbus {
        fn activate(&self, _: &str, _: &str, _: &[String]) -> Result<(), ActivateError> {
            Err(match &self.0 {
                ActivateError::Timeout => ActivateError::Timeout,
                ActivateError::Bus(s) => ActivateError::Bus(s.clone()),
            })
        }
    }

    /// A DBus activator that succeeds (records that it was asked).
    struct OkDbus(RefCell<u32>);
    impl DbusActivator for OkDbus {
        fn activate(&self, _: &str, _: &str, _: &[String]) -> Result<(), ActivateError> {
            *self.0.borrow_mut() += 1;
            Ok(())
        }
    }

    fn dbus_launch(fallback: Vec<String>) -> Launch {
        Launch::DBus {
            bus_name: "org.gnome.Calculator".into(),
            object_path: "/org/gnome/Calculator".into(),
            action: None,
            files: vec![],
            fallback_exec: fallback,
        }
    }

    #[test]
    fn exec_launch_spawns_argv() {
        let spawner = FakeSpawner::ok();
        let exec = Executor::new(OkDbus(RefCell::new(0)), spawner);
        exec.execute(&Launch::Exec { argv: vec!["firefox".into(), "--new".into()] });
        assert_eq!(exec.spawner.calls.borrow().as_slice(), &[vec!["firefox".to_string(), "--new".into()]]);
    }

    #[test]
    fn dbus_timeout_falls_back_to_exec() {
        // Fake bus that never responds → Timeout → spawn the carried Exec= fallback.
        let exec = Executor::new(FakeDbus(ActivateError::Timeout), FakeSpawner::ok());
        exec.execute(&dbus_launch(vec!["gnome-calculator".into()]));
        assert_eq!(exec.spawner.calls.borrow().as_slice(), &[vec!["gnome-calculator".to_string()]]);
    }

    #[test]
    fn dbus_success_does_not_spawn() {
        let exec = Executor::new(OkDbus(RefCell::new(0)), FakeSpawner::ok());
        exec.execute(&dbus_launch(vec!["gnome-calculator".into()]));
        assert!(exec.spawner.calls.borrow().is_empty());
        assert_eq!(*exec.dbus.0.borrow(), 1);
    }

    #[test]
    fn dbus_failure_without_fallback_is_a_noop_not_panic() {
        let exec = Executor::new(FakeDbus(ActivateError::Bus("no service".into())), FakeSpawner::ok());
        exec.execute(&dbus_launch(vec![]));
        assert!(exec.spawner.calls.borrow().is_empty());
    }

    #[test]
    fn spawn_failure_is_logged_not_fatal() {
        // argv[0] not on PATH: spawner errors; execute must not panic, and it tried exactly once.
        let exec = Executor::new(OkDbus(RefCell::new(0)), FakeSpawner::failing());
        exec.execute(&Launch::Exec { argv: vec!["does-not-exist".into()] });
        assert_eq!(exec.spawner.calls.borrow().len(), 1);
    }
}
