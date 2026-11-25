//! The test lab: the run's directory, the binary under test as a socket client, the shared waits.

use std::cell::Cell;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pods_core::link::STATE_POPULATED_LINE;
use serde_json::{Map, Value};

use super::clock::{Clock, SystemClock};
use super::old_line::Installed;
use super::system::{Waited, bluez, find, journal_cursor, wait_for_entries};
use super::unit::{self, POLL_INTERVAL, run};
use super::{Interrupted, Waits, stopped};

/// How long the daemon under test has to report `connected` after it starts.
pub(crate) const CONNECT_WAIT: Duration = Duration::from_secs(20);
/// How long a restart has to reach a populated state before the trial counts as not populated.
pub(super) const ADOPTION_WAIT: Duration = Duration::from_secs(10);
/// How long `connected` may stay true after `do disconnect` or the adapter going off.
pub(super) const DISCONNECT_WAIT: Duration = Duration::from_secs(5);
/// How long a `do connect` has to reach a populated state, which is `BlueZ`'s page plus the handshake.
pub(super) const RECONNECT_WAIT: Duration = Duration::from_secs(15);
/// How long `librepods status` may take; it answers from memory.
pub(super) const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
/// How long `librepods do` may take; a connect waits on `BlueZ`'s page.
pub(super) const ACTION_TIMEOUT: Duration = Duration::from_secs(30);

/// The run's directory, binary and the `BlueZ` objects of the `AirPods`; the paths are private and never printed.
pub(crate) struct Lab<'a> {
    pub(crate) binary: &'a Path,
    pub(crate) dir: &'a Path,
    pub(crate) uid: u32,
    pub(crate) home: &'a OsStr,
    pub(super) waits: Waits,
    /// What the installed daemon showed before the run stopped it, for C9.
    pub(crate) installed: Installed,
    device: String,
    adapter: String,
    /// A `do disconnect` was sent and no `do connect` has succeeded since.
    disconnect_sent: Cell<bool>,
}

/// How the wait for the `AirPods` after the daemon's start ended.
pub(crate) enum Presence {
    Connected,
    /// The daemon answered and never reported `connected`.
    Absent,
    /// The daemon did not answer `status` at the last look, for the reason given.
    Silent(String),
}

impl<'a> Lab<'a> {
    pub(super) fn new(
        binary: &'a Path,
        dir: &'a Path,
        uid: u32,
        home: &'a OsStr,
        waits: Waits,
        installed: Installed,
    ) -> Self {
        Self {
            binary,
            dir,
            uid,
            home,
            waits,
            installed,
            device: String::new(),
            adapter: String::new(),
            disconnect_sent: Cell::new(false),
        }
    }

    pub(super) fn run_dir(&self) -> PathBuf {
        self.dir.join("run")
    }

    pub(super) fn socket(&self) -> PathBuf {
        self.run_dir().join("librepods.sock")
    }

    pub(super) fn state_file(&self) -> PathBuf {
        self.dir.join("state").join("librepods").join("status.json")
    }

    /// Runs the binary under test as a client of the test unit's socket.
    pub(super) fn librepods(&self, args: &[&str], timeout: Duration) -> Result<unit::Ran, String> {
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        let run_dir = self.run_dir();
        let state_dir = self.dir.join("state");
        let env = [
            ("XDG_RUNTIME_DIR", run_dir.as_os_str()),
            ("XDG_STATE_HOME", state_dir.as_os_str()),
        ];
        run(&self.binary.to_string_lossy(), &args, &env, timeout)
    }

    /// The state object `librepods status` prints.
    pub(super) fn status(&self) -> Result<Map<String, Value>, String> {
        let ran = self.librepods(&["status"], STATUS_TIMEOUT)?;
        if !ran.succeeded() {
            return Err(format!("librepods status exited with {}", ran.status));
        }
        status_object(&ran.stdout)
            .ok_or_else(|| "librepods status printed no state object".to_owned())
    }

    pub(super) fn connected(&self) -> Option<bool> {
        self.status().ok()?.get("connected")?.as_bool()
    }

    pub(super) fn act(&self, action: &str) -> Result<(), String> {
        if action == "disconnect" {
            self.disconnect_sent.set(true);
        }
        let ran = self.librepods(&["do", action], ACTION_TIMEOUT)?;
        if ran.succeeded() {
            if action == "connect" {
                self.disconnect_sent.set(false);
            }
            Ok(())
        } else {
            Err(format!("librepods do {action} exited with {}", ran.status))
        }
    }

    /// True when a `do disconnect` was sent and no `do connect` has succeeded since, whatever `connected` reads yet.
    pub(super) fn disconnect_sent(&self) -> bool {
        self.disconnect_sent.get()
    }

    /// Waits for the daemon under test to report `connected`; a miss is judged by the last answer, so a daemon that dies is not taken for absent `AirPods`.
    pub(crate) fn wait_present(&self) -> Result<Presence, Interrupted> {
        let mut last = Err(String::new());
        let found = wait_until(self.waits.connect, || {
            last = self.status().and_then(|state| {
                state
                    .get("connected")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| "librepods status printed no connected key".to_owned())
            });
            last == Ok(true)
        })?;
        Ok(match last {
            _ if found => Presence::Connected,
            Ok(_) => Presence::Absent,
            Err(cause) => Presence::Silent(cause),
        })
    }

    /// C2, C5 and C7 read the device and adapter it remembers.
    pub(crate) fn find_device(&mut self) -> Result<(), String> {
        let device = bluez()?
            .connected()
            .ok_or("BlueZ reports no connected AirPods while the daemon reports connected")?;
        self.device = device.path;
        self.adapter = device.adapter;
        Ok(())
    }

    /// Starts the test unit again and waits until it is populated; the error names the step that failed.
    pub(super) fn start_again(&self) -> Result<Result<(), String>, Interrupted> {
        let cursor = match journal_cursor() {
            Ok(cursor) => cursor,
            Err(error) => return Ok(Err(error)),
        };
        if let Err(error) = unit::start_under_test(self.binary, self.dir, self.uid) {
            return Ok(Err(error));
        }
        let waited = wait_for_entries(cursor.as_deref(), ADOPTION_WAIT, |entries| {
            find(entries, STATE_POPULATED_LINE, 0).is_some()
        })?;
        Ok(match waited {
            Waited::Found(_) => Ok(()),
            Waited::TimedOut => Err(format!(
                "not populated {} s after the start",
                ADOPTION_WAIT.as_secs()
            )),
            Waited::ReadFailed(error) => Err(error),
        })
    }
}

/// Ignores stop signals: the clean up after a stop uses it.
pub(super) fn settle(timeout: Duration, ready: impl FnMut() -> bool) -> bool {
    settle_on(&SystemClock, timeout, ready)
}

fn settle_on(clock: &dyn Clock, timeout: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let deadline = clock.deadline(timeout);
    loop {
        if ready() {
            return true;
        }
        if deadline.is_none_or(|deadline| clock.now() >= deadline) {
            return false;
        }
        clock.pause(POLL_INTERVAL);
    }
}

/// A stop signal ends the wait.
pub(super) fn wait_until(
    timeout: Duration,
    ready: impl FnMut() -> bool,
) -> Result<bool, Interrupted> {
    wait_until_on(&SystemClock, timeout, ready)
}

pub(super) fn wait_until_on(
    clock: &dyn Clock,
    timeout: Duration,
    mut ready: impl FnMut() -> bool,
) -> Result<bool, Interrupted> {
    let mut interrupted = false;
    let ready = settle_on(clock, timeout, || {
        interrupted = stopped();
        interrupted || ready()
    });
    if interrupted {
        Err(Interrupted)
    } else {
        Ok(ready)
    }
}

pub(super) fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Sample input `{"adaptive_noise_level":37,"connected":true,...}\n`.
pub(super) fn status_object(text: &str) -> Option<Map<String, Value>> {
    match serde_json::from_str(text.lines().next()?) {
        Ok(Value::Object(object)) => Some(object),
        _ => None,
    }
}

impl Lab<'_> {
    pub(super) fn device_connected(&self) -> Result<bool, String> {
        Ok(bluez()?
            .devices
            .iter()
            .any(|device| device.path == self.device && device.connected))
    }

    pub(super) fn adapter_state(&self) -> Result<(bool, bool), String> {
        let bluez = bluez()?;
        let adapter = bluez
            .adapters
            .iter()
            .find(|adapter| adapter.path == self.adapter)
            .ok_or("BlueZ lost the AirPods' adapter")?;
        Ok((adapter.powered, adapter.discovering))
    }
}
