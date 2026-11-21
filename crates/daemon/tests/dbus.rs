//! Drives the built daemon's D-Bus interface on a private bus the test starts, and the daemon with no bus at all.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs::{self, File};
    use std::future::poll_fn;
    use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use pods_core::command::CommandError;
    use rustix::process::{Pid, Signal, kill_process};
    use tempfile::TempDir;
    use tokio::runtime::Builder as RuntimeBuilder;
    use zbus::connection::Builder;
    use zbus::export::futures_core::Stream;
    use zbus::{Connection, MessageStream, OwnedVariant};

    const BINARY: &str = env!("CARGO_BIN_EXE_librepods");

    /// A bus, daemon or reply still missing after this long is hung, and the test fails.
    const HANG_LIMIT: Duration = Duration::from_secs(10);

    /// The whole bus conversation of one test, longer than any one wait inside it so that wait reports first.
    const CONVERSATION_LIMIT: Duration = Duration::from_secs(30);

    /// How often a wait checks its condition again.
    const POLL: Duration = Duration::from_millis(10);

    /// The names the daemon serves, from `src/daemon/dbus.rs`.
    const BUS_NAME: &str = "io.github.thisisgm.Omapods";
    const OBJECT_PATH: &str = "/io/github/thisisgm/Omapods";
    const INTERFACE: &str = "io.github.thisisgm.Omapods1";

    /// How the daemon's line about a missing bus or a taken name starts, from `serve` in `src/daemon/dbus.rs`.
    const NO_BUS_LOG: &str =
        "D-Bus interface off: cannot own io.github.thisisgm.Omapods on the session bus:";

    const BUS_DAEMON: &str = "dbus-daemon";
    const BUS_ADDRESS_VAR: &str = "DBUS_SESSION_BUS_ADDRESS";
    const DRIVER_NAME: &str = "org.freedesktop.DBus";
    const DRIVER_PATH: &str = "/org/freedesktop/DBus";
    const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
    const INTROSPECTABLE_INTERFACE: &str = "org.freedesktop.DBus.Introspectable";
    const CHANGES_RULE: &str = "type='signal',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',path='/io/github/thisisgm/Omapods'";

    #[expect(
        clippy::disallowed_methods,
        reason = "std has no timed wait for a child process or a file, and this is the test harness, not the core"
    )]
    fn pause(duration: Duration) {
        thread::sleep(duration);
    }

    #[expect(
        clippy::print_stderr,
        reason = "a skipped test says why where the test runner shows it"
    )]
    fn say_skipped(why: &str) {
        eprintln!("skipped: {why}");
    }

    /// Waits for `ready` to give a value, failing the test once `HANG_LIMIT` passes.
    fn wait_for<T>(what: &str, mut ready: impl FnMut() -> Option<T>) -> T {
        let started = Instant::now();
        loop {
            if let Some(value) = ready() {
                return value;
            }
            assert!(
                started.elapsed() < HANG_LIMIT,
                "{what} not seen after {HANG_LIMIT:?}"
            );
            pause(POLL);
        }
    }

    /// A child process killed and reaped however the test ends, a panic included.
    struct Reaped {
        child: Child,
        log: PathBuf,
    }

    impl Reaped {
        fn log(&self) -> String {
            fs::read_to_string(&self.log).unwrap()
        }
    }

    impl Drop for Reaped {
        fn drop(&mut self) {
            // A child that already exited cannot be killed, and the wait still reaps it.
            drop(self.child.kill());
            drop(self.child.wait());
        }
    }

    /// One test's temp directory: the daemon's runtime directory, state home and home, and the bus's own directory.
    struct Session {
        dir: TempDir,
    }

    impl Session {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            for sub in ["run", "state", "home", "bus"] {
                fs::create_dir(dir.path().join(sub)).unwrap();
            }
            fs::set_permissions(dir.path().join("run"), fs::Permissions::from_mode(0o700)).unwrap();
            Self { dir }
        }

        fn path(&self, sub: &str) -> PathBuf {
            self.dir.path().join(sub)
        }

        fn socket(&self) -> PathBuf {
            self.path("run").join("librepods.sock")
        }

        /// Starts a private bus with everything it reads or writes in this session, and returns its address.
        fn start_bus(&self) -> (Reaped, String) {
            let bus_dir = self.path("bus");
            let mut child = Command::new(BUS_DAEMON)
                .args(["--session", "--nofork", "--print-address"])
                .arg(format!("--address=unix:dir={}", bus_dir.display()))
                .env_clear()
                .env("HOME", self.path("home"))
                .env("XDG_RUNTIME_DIR", &bus_dir)
                .env("XDG_DATA_HOME", &bus_dir)
                .env("XDG_DATA_DIRS", &bus_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(File::create(bus_dir.join("bus.log")).unwrap())
                .spawn()
                .unwrap();
            let stdout = child.stdout.take().unwrap();
            let bus = Reaped {
                child,
                log: bus_dir.join("bus.log"),
            };
            let (sender, address) = mpsc::channel();
            thread::spawn(move || {
                let mut line = String::new();
                // A bus that dies before printing closes stdout, which leaves the line empty.
                drop(BufReader::new(stdout).read_line(&mut line));
                drop(sender.send(line));
            });
            let address = address.recv_timeout(HANG_LIMIT).unwrap();
            let address = address.trim().to_owned();
            assert!(!address.is_empty(), "no bus address: {}", bus.log());
            (bus, address)
        }

        /// Starts a daemon whose session bus is `bus_address`, and waits until its socket accepts.
        fn start_daemon(&self, bus_address: &str, log_name: &str) -> Reaped {
            let log = self.path(log_name);
            let child = Command::new(BINARY)
                .arg("daemon")
                .env("XDG_RUNTIME_DIR", self.path("run"))
                .env("XDG_STATE_HOME", self.path("state"))
                .env("HOME", self.path("home"))
                .env(BUS_ADDRESS_VAR, bus_address)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(File::create(&log).unwrap())
                .spawn()
                .unwrap();
            let daemon = Reaped { child, log };
            wait_for("the daemon socket", || {
                UnixStream::connect(self.socket()).ok()
            });
            daemon
        }

        fn ask(&self, verb: &str) -> String {
            ask(&self.socket(), verb)
        }
    }

    fn ask(socket: &Path, verb: &str) -> String {
        let mut stream = UnixStream::connect(socket).unwrap();
        stream.write_all(verb.as_bytes()).unwrap();
        stream.set_read_timeout(Some(HANG_LIMIT)).unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        reply
    }

    fn stop(daemon: &mut Reaped) -> ExitStatus {
        let pid = Pid::from_raw(i32::try_from(daemon.child.id()).unwrap()).unwrap();
        kill_process(pid, Signal::TERM).unwrap();
        wait_for("the daemon to exit", || daemon.child.try_wait().unwrap())
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

    async fn get(client: &Connection, property: &str) -> OwnedVariant {
        client
            .call_method(
                Some(BUS_NAME),
                OBJECT_PATH,
                Some(PROPERTIES_INTERFACE),
                "Get",
                &(INTERFACE, property),
            )
            .await
            .unwrap()
            .body()
            .deserialize()
            .unwrap()
    }

    /// The next `PropertiesChanged` from the daemon, as its changed properties.
    async fn next_change(stream: &mut MessageStream) -> HashMap<String, OwnedVariant> {
        let message = poll_fn(|context| Pin::new(&mut *stream).poll_next(context))
            .await
            .unwrap()
            .unwrap();
        let (interface, changed, _): (String, HashMap<String, OwnedVariant>, Vec<String>) =
            message.body().deserialize().unwrap();
        assert_eq!(interface, INTERFACE);
        changed
    }

    /// Whether the bus daemon can be started at all; a host without it skips the test that needs it.
    fn bus_daemon_installed() -> bool {
        match Command::new(BUS_DAEMON).arg("--version").output() {
            Ok(_) => true,
            Err(error) if error.kind() == ErrorKind::NotFound => false,
            Err(error) => panic!("cannot run {BUS_DAEMON}: {error}"),
        }
    }

    #[test]
    fn daemon_owns_the_name_and_busctl_style_get_works() {
        if !bus_daemon_installed() {
            say_skipped(&format!("{BUS_DAEMON} is not installed"));
            return;
        }
        let session = Session::new();
        let (_bus, address) = session.start_bus();
        let mut daemon = session.start_daemon(&address, "daemon.log");
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let wait = async {
            let client = Builder::address(address.as_str()).build().await.unwrap();
            let started = Instant::now();
            while !name_has_owner(&client).await {
                assert!(
                    started.elapsed() < HANG_LIMIT,
                    "{BUS_NAME} never owned: {}",
                    daemon.log()
                );
                tokio::time::sleep(POLL).await;
            }
            let connected = bool::try_from(get(&client, "Connected").await).unwrap();
            assert!(!connected);
            let version = u32::try_from(get(&client, "ApiVersion").await).unwrap();
            assert_eq!(version, 1);
            let xml: String = client
                .call_method(
                    Some(BUS_NAME),
                    OBJECT_PATH,
                    Some(INTROSPECTABLE_INTERFACE),
                    "Introspect",
                    &(),
                )
                .await
                .unwrap()
                .body()
                .deserialize()
                .unwrap();
            assert!(
                xml.contains(&format!(r#"<interface name="{INTERFACE}">"#)),
                "{xml}"
            );
            let mut changes = MessageStream::for_match_rule(CHANGES_RULE, &client, None)
                .await
                .unwrap();
            let refused = client
                .call_method(
                    Some(BUS_NAME),
                    OBJECT_PATH,
                    Some(INTERFACE),
                    "SetNoiseMode",
                    &("anc",),
                )
                .await;
            let Err(zbus::Error::MethodError(name, message, _)) = refused else {
                panic!("SetNoiseMode was not refused: {refused:?}");
            };
            assert_eq!(
                name.as_str(),
                "io.github.thisisgm.Omapods1.Error.NotConnected"
            );
            assert_eq!(
                message.as_deref(),
                Some(CommandError::NotConnected.message())
            );
            // The refused mode still counts as a noise control change, which only the status object shows.
            let counted = next_change(&mut changes).await;
            assert_eq!(counted.keys().collect::<Vec<_>>(), ["StatusJson"]);
            assert_eq!(session.ask("ear:both"), "");
            let ear_changed = next_change(&mut changes).await;
            let ear = String::try_from(ear_changed["EarDetection"].try_clone().unwrap()).unwrap();
            assert_eq!(ear, "both");
            let status = String::try_from(ear_changed["StatusJson"].try_clone().unwrap()).unwrap();
            assert_eq!(format!("{status}\n"), session.ask("status"));
        };
        let finished =
            runtime.block_on(async { tokio::time::timeout(CONVERSATION_LIMIT, wait).await });
        finished.unwrap();

        // A second daemon on the same bus finds the name owned, says so once and still serves its socket.
        let second = Session::new();
        let mut other = second.start_daemon(&address, "second.log");
        wait_for("the second daemon's bus line", || {
            other.log().contains(NO_BUS_LOG).then_some(())
        });
        assert!(second.ask("status").starts_with('{'));
        assert_eq!(
            other.log().matches(NO_BUS_LOG).count(),
            1,
            "{}",
            other.log()
        );
        assert_eq!(stop(&mut other).code(), Some(0));
        assert_eq!(stop(&mut daemon).code(), Some(0));
        assert_eq!(daemon.log(), "SIGTERM received, stopping\n");
    }

    #[test]
    fn daemon_runs_without_a_session_bus() {
        let session = Session::new();
        let missing = format!("unix:path={}", session.path("bus").join("no-bus").display());
        let mut daemon = session.start_daemon(&missing, "daemon.log");
        let status = session.ask("status");
        let state_file = session.path("state").join("librepods").join("status.json");
        assert_eq!(status, fs::read_to_string(state_file).unwrap());
        wait_for("the bus line", || {
            daemon.log().contains(NO_BUS_LOG).then_some(())
        });
        assert_eq!(session.ask("ear:off"), "");
        assert_ne!(session.ask("status"), status);
        assert_eq!(stop(&mut daemon).code(), Some(0));
        let log = daemon.log();
        assert_eq!(log.lines().count(), 2, "{log}");
        assert_eq!(log.matches(NO_BUS_LOG).count(), 1, "{log}");
        assert!(log.ends_with("SIGTERM received, stopping\n"), "{log}");
    }
}
