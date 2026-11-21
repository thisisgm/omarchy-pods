//! A private `dbus-daemon` per bus a test needs, everything it reads or writes inside the session.

use std::fs::{self, File};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::{Daemon, Session, wait_for};

const BUS_DAEMON: &str = "dbus-daemon";
const SOCKET_NAME: &str = "bus.sock";

/// A running private bus, ended with the guard.
pub(crate) struct Bus {
    pub(crate) address: String,
    _daemon: Daemon,
}

fn bus_dir(session: &Session, name: &str) -> PathBuf {
    session.dir.path().join(name)
}

/// The address the bus `name` listens on, known before it runs.
pub(crate) fn bus_address(session: &Session, name: &str) -> String {
    format!(
        "unix:path={}",
        bus_dir(session, name).join(SOCKET_NAME).display()
    )
}

/// Starts the bus `name` and returns once it accepts.
pub(crate) fn start_bus(session: &Session, name: &str) -> Bus {
    let dir = bus_dir(session, name);
    fs::create_dir(&dir).unwrap();
    let log = dir.join("bus.log");
    let child = Command::new(BUS_DAEMON)
        .args(["--session", "--nofork"])
        .arg(format!("--address={}", bus_address(session, name)))
        .env_clear()
        .env("HOME", session.dir.path().join("home"))
        .env("XDG_RUNTIME_DIR", &dir)
        .env("XDG_DATA_HOME", &dir)
        .env("XDG_DATA_DIRS", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(File::create(&log).unwrap())
        .spawn()
        .unwrap_or_else(|error| panic!("cannot start {BUS_DAEMON}: {error}"));
    let daemon = Daemon { child, log };
    let socket = dir.join(SOCKET_NAME);
    wait_for("the private bus to accept", || {
        UnixStream::connect(&socket).ok()
    });
    Bus {
        address: bus_address(session, name),
        _daemon: daemon,
    }
}
