//! Drives the built daemon's D-Bus interface on a private bus the test starts, and the daemon with no bus at all.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "these tests start the daemon on a private session bus alone, so the fake BlueZ goes unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::future::poll_fn;
    use std::path::Path;
    use std::pin::Pin;

    use pods_core::command::CommandError;
    use tokio::runtime::Builder as RuntimeBuilder;
    use zbus::export::futures_core::Stream;
    use zbus::{Connection, MessageStream, OwnedVariant};

    use super::support::bus::{
        BUS_NAME, CONVERSATION_LIMIT, INTERFACE, Listener, NO_SESSION_LOG, OBJECT_PATH,
        bus_address, changes_rule, owned_connection, start_bus, start_daemon,
    };
    use super::support::players::PROPERTIES;
    use super::support::{HANG_LIMIT, Session, binary, log_lines, wait_for};

    /// How the daemon's line about a taken name starts, from `serve` in `src/daemon/dbus.rs`.
    const NO_BUS_LOG: &str =
        "D-Bus interface off: cannot own io.github.thisisgm.Omapods on the session bus:";

    /// How zbus 6 words a name another connection owns, after `NO_BUS_LOG`.
    const NAME_TAKEN: &str = "name already taken on the bus";

    /// How zbus 6 starts an address it cannot reach, after `NO_BUS_LOG`; the address follows in backquotes.
    const CONNECT_FAILED: &str = "failed to connect to";

    const INTROSPECTABLE_INTERFACE: &str = "org.freedesktop.DBus.Introspectable";

    /// What `librepods <args>` prints, after checking that it exited 0.
    fn client(session: &Session, args: &[&str]) -> String {
        let output = session.run_client(Path::new(binary()), args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    async fn get(client: &Connection, property: &str) -> OwnedVariant {
        client
            .call_method(
                Some(BUS_NAME),
                OBJECT_PATH,
                Some(PROPERTIES),
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

    async fn adaptive_100_is_taken_and_101_is_out_of_range(bus: &Connection) {
        for (level, name_wanted, refusal) in [
            (100_u8, "NotConnected", CommandError::NotConnected),
            (101, "OutOfRange", CommandError::OutOfRange),
        ] {
            let reply = bus
                .call_method(
                    Some(BUS_NAME),
                    OBJECT_PATH,
                    Some(INTERFACE),
                    "SetAdaptiveLevel",
                    &(level,),
                )
                .await;
            let Err(zbus::Error::MethodError(name, message, _)) = reply else {
                panic!("SetAdaptiveLevel({level}) was not refused: {reply:?}");
            };
            assert_eq!(name.as_str(), format!("{INTERFACE}.Error.{name_wanted}"));
            assert_eq!(message.as_deref(), Some(refusal.message()), "{level}");
        }
    }

    /// Whether the daemon of process `pid` answers at its object on a unique name, failing if it has no connection.
    fn answers_at_its_object(asker: &Listener, pid: u32) -> bool {
        let names: Vec<String> = asker.ask_bus("ListNames", &());
        let theirs: Vec<String> = names
            .into_iter()
            .filter(|name| name.starts_with(':'))
            .filter(|name| {
                asker.ask_bus::<u32>("GetConnectionUnixProcessID", &name.as_str()) == pid
            })
            .collect();
        assert!(
            !theirs.is_empty(),
            "process {pid} has no connection on the bus"
        );
        let version = (INTERFACE, "ApiVersion");
        theirs.iter().any(|name| {
            asker
                .call(name, OBJECT_PATH, PROPERTIES, "Get", &version)
                .is_ok()
        })
    }

    fn text(value: &OwnedVariant) -> String {
        String::try_from(value.try_clone().unwrap()).unwrap()
    }

    #[test]
    fn daemon_owns_the_name_and_busctl_style_get_works() {
        let session = Session::new();
        let bus = start_bus(&session, "bus");
        let address = bus.address.clone();
        let mut daemon = start_daemon(&session, &address, "daemon.log");
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let wait = async {
            let client_connection = owned_connection(&address, &daemon).await;
            let bus = &client_connection;
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
            let mut changes = MessageStream::for_match_rule(changes_rule().as_str(), bus, None)
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
            adaptive_100_is_taken_and_101_is_out_of_range(bus).await;
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
        // One connection asks both, so none of the test's own leaves between a listing and its process queries.
        let asker = Listener::subscribe(&address, changes_rule().as_str());
        assert!(
            answers_at_its_object(&asker, daemon.child.id()),
            "{}",
            daemon.log()
        );
        let pid = other.child.id();
        assert!(!answers_at_its_object(&asker, pid), "{}", other.log());
        assert_eq!(other.stop().code(), Some(0));
        let log = other.log();
        assert_eq!(log.matches(NO_BUS_LOG).count(), 1, "{log}");
        assert_eq!(daemon.stop().code(), Some(0));
        assert_eq!(log_lines(&daemon.log()), ["SIGTERM received, stopping"]);
    }

    #[test]
    fn daemon_runs_without_a_session_bus() {
        let pointing_nowhere = Session::new();
        let missing = bus_address(&pointing_nowhere, "no-bus");
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
                daemon.log().contains(NO_SESSION_LOG).then_some(())
            });
            assert_eq!(session.send(b"ear:off"), b"");
            assert_ne!(session.status(), status);
            assert_eq!(daemon.stop().code(), Some(0));
            let log = daemon.log();
            assert_eq!(log_lines(&log).len(), 2, "{log}");
            assert_eq!(log.matches(NO_SESSION_LOG).count(), 1, "{log}");
            let unreachable = format!("{NO_SESSION_LOG} {CONNECT_FAILED} `{address}`:");
            assert!(log.contains(&unreachable), "{log}");
            assert!(log.ends_with("SIGTERM received, stopping\n"), "{log}");
        }
    }
}
