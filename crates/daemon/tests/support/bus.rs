//! A private `dbus-daemon` per bus a test needs, everything it reads or writes inside the session.

use std::fs::{self, File};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tokio::runtime::Runtime;
use tokio::time::timeout;
use zbus::Connection;
use zbus::connection::Builder;

use super::{BINARY, Daemon, HANG_LIMIT, Session, wait_for};

/// The names the daemon serves on the session bus, from `src/daemon/dbus.rs`.
pub(crate) const BUS_NAME: &str = "io.github.thisisgm.Omapods";
pub(crate) const OBJECT_PATH: &str = "/io/github/thisisgm/Omapods";
pub(crate) const INTERFACE: &str = "io.github.thisisgm.Omapods1";

const BUS_DAEMON: &str = "dbus-daemon";
const SOCKET_NAME: &str = "bus.sock";

const BUS_ADDRESS_VAR: &str = "DBUS_SESSION_BUS_ADDRESS";
const DRIVER_NAME: &str = "org.freedesktop.DBus";
const DRIVER_PATH: &str = "/org/freedesktop/DBus";

/// The whole bus conversation of one test, longer than any one wait inside it so that wait reports first.
pub(crate) const CONVERSATION_LIMIT: Duration = Duration::from_secs(30);

/// How often the async wait for the bus name asks again; the harness wait is blocking and cannot run here.
const BUS_POLL: Duration = Duration::from_millis(10);

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

/// Runs one bus exchange on `runtime`, failing the test once `HANG_LIMIT` passes.
pub(crate) fn in_time<T>(runtime: &Runtime, work: impl Future<Output = T>) -> T {
    let finished = runtime.block_on(async { timeout(HANG_LIMIT, work).await });
    finished.unwrap_or_else(|_| panic!("the bus client still waiting after {HANG_LIMIT:?}"))
}

/// Starts a daemon whose session bus is `bus_address`, and waits until its socket accepts.
pub(crate) fn start_daemon(session: &Session, bus_address: &str, log_name: &str) -> Daemon {
    let mut command = session.command(Path::new(BINARY), &["daemon"]);
    let daemon = session.spawn_command(command.env(BUS_ADDRESS_VAR, bus_address), log_name);
    session.wait_accepting();
    daemon
}

/// A client on the bus at `address`, returned once `daemon` owns its name there.
pub(crate) async fn owned_connection(address: &str, daemon: &Daemon) -> Connection {
    let client = Builder::address(address)
        .method_timeout(HANG_LIMIT)
        .build()
        .await
        .unwrap();
    let started = Instant::now();
    while !name_has_owner(&client).await {
        assert!(
            started.elapsed() < HANG_LIMIT,
            "{BUS_NAME} never owned: {}",
            daemon.log()
        );
        tokio::time::sleep(BUS_POLL).await;
    }
    client
}

async fn name_has_owner(client: &Connection) -> bool {
    client
        .call_method(
            Some(DRIVER_NAME),
            DRIVER_PATH,
            Some(DRIVER_NAME),
            "NameHasOwner",
            &(BUS_NAME,),
        )
        .await
        .unwrap()
        .body()
        .deserialize()
        .unwrap()
}
