//! A known setting given a value it does not take, through `librepods set`, the control socket, D-Bus and the legacy verbs.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "these tests start the daemon with no fake BlueZ, so its helpers go unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::iter;
    use std::path::Path;

    use pods_core::settings;
    use tokio::runtime::Builder as RuntimeBuilder;

    use super::support::bus::{
        BUS_NAME, CONVERSATION_LIMIT, INTERFACE, OBJECT_PATH, owned_connection, start_bus,
        start_daemon,
    };
    use super::support::{DAEMON_LOG, Proto1, Session, binary, set_request};

    /// The exit code of every refused command, `ExitCode::FAILURE`.
    const REFUSED: i32 = 1;

    const BAD_VALUE: &str =
        r#"{"code":"bad_value","message":"That setting does not take this value."}"#;

    fn error_reply(id: &str, error: &str) -> String {
        format!("{{\"id\":{id},\"ok\":false,\"error\":{error}}}\n")
    }

    #[test]
    fn set_noise_loud_is_bad_value_with_the_list() {
        let session = Session::new();
        let _daemon = session.start();
        for (args, stderr) in [
            (
                ["set", "noise", "loud"],
                "That setting does not take this value.\nValues: off, anc, transparency, adaptive, cycle\n",
            ),
            (
                ["set", "ear_detection", "two"],
                "That setting does not take this value.\nValues: off, one, both\n",
            ),
            (
                ["set", "one_bud_anc", "on"],
                "That setting does not take this value.\nValues: true, false\n",
            ),
            (
                ["set", "adaptive_level", "high"],
                "That setting does not take this value.\n",
            ),
        ] {
            let out = session.run_client(Path::new(binary()), &args);
            assert_eq!(out.status.code(), Some(REFUSED), "{args:?}");
            assert_eq!(out.stdout, b"", "{args:?}");
            let printed = String::from_utf8(out.stderr).unwrap();
            assert_eq!(printed, stderr, "{args:?}");
            for word in settings::values(args[1]).unwrap_or_default() {
                assert!(printed.contains(word), "{args:?} {word}");
            }
        }
    }

    #[test]
    fn the_socket_reply_carries_the_code() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Proto1::connect(&session);
        for (id, key, value) in [
            (1, "noise", "loud"),
            (2, "ear_detection", "two"),
            (3, "conversational_awareness", "on"),
            (4, "adaptive_level", "high"),
        ] {
            let reply = client.ask_line(&set_request(id, key, value));
            assert_eq!(
                reply,
                error_reply(&id.to_string(), BAD_VALUE),
                "{key} {value}"
            );
        }
    }

    #[test]
    fn the_dbus_call_carries_the_code() {
        let session = Session::new();
        let bus = start_bus(&session, "bus");
        let mut daemon = start_daemon(&session, &bus.address, DAEMON_LOG);
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let conversation = async {
            let client = owned_connection(&bus.address, &daemon).await;
            for (method, word) in [("SetNoiseMode", "loud"), ("SetEarDetection", "two")] {
                let reply = client
                    .call_method(
                        Some(BUS_NAME),
                        OBJECT_PATH,
                        Some(INTERFACE),
                        method,
                        &(word,),
                    )
                    .await;
                let Err(zbus::Error::MethodError(name, message, _)) = reply else {
                    panic!("{method}({word}) was not refused: {reply:?}");
                };
                assert_eq!(
                    name.as_str(),
                    "io.github.thisisgm.Omapods1.Error.BadValue",
                    "{method}"
                );
                assert_eq!(
                    message.as_deref(),
                    Some("That setting does not take this value."),
                    "{method}"
                );
            }
        };
        let finished = runtime
            .block_on(async { tokio::time::timeout(CONVERSATION_LIMIT, conversation).await });
        finished.unwrap();
        assert!(daemon.stop().success(), "{}", daemon.log());
    }

    #[test]
    fn a_number_out_of_range_is_still_out_of_range() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Proto1::connect(&session);
        let range = r#"{"code":"out_of_range","message":"The value is outside the range the setting takes."}"#;
        for (id, value) in [(1, "101"), (2, "-1"), (3, "99999999999")] {
            let reply = client.ask_line(&set_request(id, "adaptive_level", value));
            assert_eq!(reply, error_reply(&id.to_string(), range), "{value}");
        }
        let out = session.run_client(Path::new(binary()), &["set", "adaptive_level", "101"]);
        assert_eq!(out.status.code(), Some(REFUSED));
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert_eq!(
            stderr,
            "The value is outside the range the setting takes.\n"
        );
    }

    #[test]
    fn an_unparseable_request_is_still_bad_request() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Proto1::connect(&session);
        let bad = r#"{"code":"bad_request","message":"The daemon could not read the request."}"#;
        assert_eq!(client.ask_line("{garbage"), error_reply("null", bad));
        let no_value = r#"{"v":1,"id":5,"op":"set","key":"noise"}"#;
        assert_eq!(client.ask_line(no_value), error_reply("5", bad));
    }

    #[test]
    fn the_legacy_dialect_answers_as_before() {
        let session = Session::new();
        let daemon = session.start();
        let status = session.status();
        let bad_request = session.send(b"no-such-verb");
        assert_eq!(bad_request, b"");
        let verbs = ["noise:loud", "ear:two", "ca:true", "adaptive:high"];
        for verb in verbs {
            assert_eq!(session.send(verb.as_bytes()), bad_request, "{verb}");
        }
        assert_eq!(session.status(), status);
        let log = daemon.log();
        for verb in iter::once("no-such-verb").chain(verbs) {
            let unknown = format!("control socket: unknown request {verb:?}");
            assert!(log.contains(&unknown), "{unknown}: {log}");
        }
    }
}
