//! Drives the built daemon's D-Bus interface on a private bus the test starts, and the daemon with no bus at all.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod support;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs::{self, File};
    use std::future::poll_fn;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::process::{Command, ExitStatus, Stdio};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use pods_core::command::CommandError;
    use rustix::process::{Signal, kill_process};
    use tokio::runtime::Builder as RuntimeBuilder;
    use zbus::connection::Builder;
    use zbus::export::futures_core::Stream;
    use zbus::{Connection, MessageStream, OwnedVariant};

    use super::support::{BINARY, Daemon, HANG_LIMIT, Session, wait_for};

    /// The whole bus conversation of one test, longer than any one wait inside it so that wait reports first.
    const CONVERSATION_LIMIT: Duration = Duration::from_secs(30);

    /// How often the async wait for the bus name asks again; the harness wait is blocking and cannot run here.
    const BUS_POLL: Duration = Duration::from_millis(10);

    /// The names the daemon serves, from `src/daemon/dbus.rs`.
    const BUS_NAME: &str = "io.github.thisisgm.Omapods";
    const OBJECT_PATH: &str = "/io/github/thisisgm/Omapods";
    const INTERFACE: &str = "io.github.thisisgm.Omapods1";

    /// How the daemon's line about a missing bus or a taken name starts, from `serve` in `src/daemon/dbus.rs`.
    const NO_BUS_LOG: &str =
        "D-Bus interface off: cannot own io.github.thisisgm.Omapods on the session bus:";

    /// How zbus 6 words a name another connection owns, after `NO_BUS_LOG`.
    const NAME_TAKEN: &str = "name already taken on the bus";

    /// How zbus 6 starts an address it cannot reach, after `NO_BUS_LOG`; the address follows in backquotes.
    const CONNECT_FAILED: &str = "failed to connect to";

    const BUS_DAEMON: &str = "dbus-daemon";
    const BUS_ADDRESS_VAR: &str = "DBUS_SESSION_BUS_ADDRESS";
    const DRIVER_NAME: &str = "org.freedesktop.DBus";
    const DRIVER_PATH: &str = "/org/freedesktop/DBus";
    const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
    const INTROSPECTABLE_INTERFACE: &str = "org.freedesktop.DBus.Introspectable";
    const CHANGES_RULE: &str = "type='signal',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',path='/io/github/thisisgm/Omapods'";

    /// The session's directory for the private bus, its socket and its log.
    fn bus_dir(session: &Session) -> PathBuf {
        session.dir.path().join("bus")
    }

    /// Starts a private bus with everything it reads or writes in this session, and returns it with its address.
    fn start_bus(session: &Session) -> (Daemon, String) {
        let dir = bus_dir(session);
        fs::create_dir(&dir).unwrap();
        let log = dir.join("bus.log");
        let mut child = Command::new(BUS_DAEMON)
            .args(["--session", "--nofork", "--print-address"])
            .arg(format!("--address=unix:dir={}", dir.display()))
            .env_clear()
            .env("HOME", session.dir.path().join("home"))
            .env("XDG_RUNTIME_DIR", &dir)
            .env("XDG_DATA_HOME", &dir)
            .env("XDG_DATA_DIRS", &dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(File::create(&log).unwrap())
            .spawn()
            .unwrap_or_else(|error| panic!("cannot start {BUS_DAEMON}: {error}"));
        let stdout = child.stdout.take().unwrap();
        let bus = Daemon { child, log };
        let (sender, address) = mpsc::channel();
        thread::spawn(move || {
            let mut line = String::new();
            // A bus that dies before printing closes stdout, which leaves the line empty.
            drop(BufReader::new(stdout).read_line(&mut line));
            // The receiver is gone only after `recv_timeout` gave up, and the test has failed by then.
            drop(sender.send(line));
        });
        let address = address.recv_timeout(HANG_LIMIT).unwrap();
        let address = address.trim().to_owned();
        assert!(!address.is_empty(), "no bus address: {}", bus.log());
        (bus, address)
    }

    /// Starts a daemon whose session bus is `bus_address`, and waits until its socket accepts.
    fn start_daemon(session: &Session, bus_address: &str, log_name: &str) -> Daemon {
        let log = session.dir.path().join(log_name);
        let child = session
            .command(Path::new(BINARY), &["daemon"])
            .env(BUS_ADDRESS_VAR, bus_address)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let daemon = Daemon { child, log };
        wait_for("the daemon socket", || {
            UnixStream::connect(session.socket()).ok()
        });
        daemon
    }

    fn stop(daemon: &mut Daemon) -> ExitStatus {
        kill_process(daemon.pid(), Signal::TERM).unwrap();
        daemon.wait_exit()
    }

    /// What `librepods <args>` prints, after checking that it exited 0.
    fn client(session: &Session, args: &[&str]) -> String {
        let output = session.run_client(Path::new(BINARY), args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
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
        let next = poll_fn(|context| Pin::new(&mut *stream).poll_next(context));
        let message = tokio::time::timeout(HANG_LIMIT, next)
            .await
            .unwrap_or_else(|_| panic!("no PropertiesChanged within {HANG_LIMIT:?}"))
            .unwrap()
            .unwrap();
        let (interface, changed, _): (String, HashMap<String, OwnedVariant>, Vec<String>) =
            message.body().deserialize().unwrap();
        assert_eq!(interface, INTERFACE);
        changed
    }

    fn text(value: &OwnedVariant) -> String {
        String::try_from(value.try_clone().unwrap()).unwrap()
    }

    #[test]
    fn daemon_owns_the_name_and_busctl_style_get_works() {
        let session = Session::new();
        let (_bus, address) = start_bus(&session);
        let mut daemon = start_daemon(&session, &address, "daemon.log");
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let wait = async {
            let client_connection = Builder::address(address.as_str())
                .method_timeout(HANG_LIMIT)
                .build()
                .await
                .unwrap();
            let bus = &client_connection;
            let started = Instant::now();
            while !name_has_owner(bus).await {
                assert!(
                    started.elapsed() < HANG_LIMIT,
                    "{BUS_NAME} never owned: {}",
                    daemon.log()
                );
                tokio::time::sleep(BUS_POLL).await;
            }
            let connected = bool::try_from(get(bus, "Connected").await).unwrap();
            assert!(!connected);
            let version = u32::try_from(get(bus, "ApiVersion").await).unwrap();
            assert_eq!(version, 1);
            let xml: String = bus
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
            let mut changes = MessageStream::for_match_rule(CHANGES_RULE, bus, None)
                .await
                .unwrap();
            let refused = bus
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
            // The legacy verb and the protocol 1 request each move the property, and the socket agrees.
            assert_eq!(session.send(b"ear:both"), b"");
            let ear_changed = next_change(&mut changes).await;
            assert_eq!(text(&ear_changed["EarDetection"]), "both");
            let status = text(&ear_changed["StatusJson"]);
            assert_eq!(format!("{status}\n"), session.status());
            assert_eq!(client(&session, &["set", "ear_detection", "off"]), "");
            let ear_changed = next_change(&mut changes).await;
            assert_eq!(text(&ear_changed["EarDetection"]), "off");
            let status = text(&ear_changed["StatusJson"]);
            assert_eq!(format!("{status}\n"), client(&session, &["status"]));
            assert_eq!(text(&get(bus, "StatusJson").await), status);
        };
        let finished =
            runtime.block_on(async { tokio::time::timeout(CONVERSATION_LIMIT, wait).await });
        finished.unwrap();

        // A second daemon on the same bus finds the name owned, says so once and still serves its socket.
        let second = Session::new();
        let mut other = start_daemon(&second, &address, "second.log");
        let taken = format!("{NO_BUS_LOG} {NAME_TAKEN}");
        wait_for("the second daemon's bus line", || {
            other.log().contains(&taken).then_some(())
        });
        assert!(second.status().starts_with('{'));
        assert_eq!(stop(&mut other).code(), Some(0));
        let log = other.log();
        assert_eq!(log.matches(NO_BUS_LOG).count(), 1, "{log}");
        assert_eq!(stop(&mut daemon).code(), Some(0));
        assert_eq!(daemon.log(), "SIGTERM received, stopping\n");
    }

    #[test]
    fn daemon_runs_without_a_session_bus() {
        let pointing_nowhere = Session::new();
        let missing = format!(
            "unix:path={}",
            bus_dir(&pointing_nowhere).join("no-bus").display()
        );
        let daemon = start_daemon(&pointing_nowhere, &missing, "daemon.log");
        // With no address at all the daemon tries the bus in its runtime directory, which the session never creates.
        let unset = Session::new();
        let fallback = unset.start();
        let runtime_bus = format!("unix:path={}", unset.runtime_dir().join("bus").display());
        let cases = [
            (pointing_nowhere, daemon, missing),
            (unset, fallback, runtime_bus),
        ];
        for (session, mut daemon, address) in cases {
            let status = session.status();
            assert_eq!(status, fs::read_to_string(session.state_file()).unwrap());
            wait_for("the bus line", || {
                daemon.log().contains(NO_BUS_LOG).then_some(())
            });
            assert_eq!(session.send(b"ear:off"), b"");
            assert_ne!(session.status(), status);
            assert_eq!(stop(&mut daemon).code(), Some(0));
            let log = daemon.log();
            assert_eq!(log.lines().count(), 2, "{log}");
            assert_eq!(log.matches(NO_BUS_LOG).count(), 1, "{log}");
            let unreachable = format!("{NO_BUS_LOG} {CONNECT_FAILED} `{address}`:");
            assert!(log.contains(&unreachable), "{log}");
            assert!(log.ends_with("SIGTERM received, stopping\n"), "{log}");
        }
    }
}
