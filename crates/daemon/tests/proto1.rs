//! Drives protocol 1 on the built daemon's socket, and the `status`, `watch`, `set` and `do` commands, each run in a temp directory.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod support;

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::process::{Child, ChildStdout, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use rustix::process::{Signal, kill_process};
    use serde_json::Value;

    use super::support::{BINARY, HANG_LIMIT, Session, pause, read_to_close, wait_for};

    /// The longest request line the daemon takes, newline excluded, from `MAX_LINE_BYTES` in `src/daemon/proto1.rs`.
    const MAX_LINE_BYTES: usize = 4096;

    /// Clients the daemon serves at once, from `MAX_CONNECTIONS` in `src/daemon/socket.rs`.
    const MAX_CONNECTIONS: usize = 64;

    /// The changes the slow subscriber test makes, past what one socket buffer and the queue hold.
    const CHANGES: usize = 200;

    /// The bound on a `get` answered while a slow subscriber holds a full queue.
    const GET_WHILE_SLOW_LIMIT: Duration = Duration::from_millis(200);

    /// Half the daemon's 1000 ms first byte deadline: a client closed sooner was closed at accept.
    const CLOSED_AT_ACCEPT_WITHIN: Duration = Duration::from_millis(500);

    /// The legacy request deadline, from `REQUEST_DEADLINE` in `src/daemon/legacy_dialect.rs`.
    const REQUEST_DEADLINE: Duration = Duration::from_millis(1000);

    /// How long the late legacy client waits before its first byte, well inside the deadline.
    const FIRST_BYTE_AFTER: Duration = Duration::from_millis(600);

    /// The deadline plus slack for a shared host, well short of a deadline started again at the first byte.
    const LEGACY_CLOSED_BY: Duration = Duration::from_millis(1400);

    /// Subscriptions the gap test opens while another client changes the state.
    const SUBSCRIPTIONS: usize = 40;

    /// How long a check that nothing more arrives waits.
    const QUIET_FOR: Duration = Duration::from_millis(300);

    const GET: &str = r#"{"v":1,"id":7,"op":"get"}"#;
    const SUBSCRIBE: &str = r#"{"v":1,"id":8,"op":"subscribe"}"#;
    const NOT_CONNECTED: &str =
        r#"{"code":"not_connected","message":"The AirPods are not connected."}"#;
    const BAD_REQUEST: &str =
        r#"{"code":"bad_request","message":"The daemon could not read the request."}"#;

    /// One protocol 1 connection, read a line at a time.
    struct Client {
        reader: BufReader<UnixStream>,
    }

    impl Client {
        fn connect(session: &Session) -> Self {
            let stream = UnixStream::connect(session.socket()).unwrap();
            stream.set_read_timeout(Some(HANG_LIMIT)).unwrap();
            Self {
                reader: BufReader::new(stream),
            }
        }

        fn send(&mut self, bytes: &[u8]) {
            self.reader.get_mut().write_all(bytes).unwrap();
        }

        /// The next line, newline included, or empty once the daemon closed.
        fn line(&mut self) -> String {
            let mut line = String::new();
            self.reader.read_line(&mut line).unwrap();
            line
        }

        /// Sends one request line and reads one line back.
        fn ask(&mut self, request: &str) -> String {
            self.send(format!("{request}\n").as_bytes());
            self.line()
        }

        /// Whether the daemon closed this connection with nothing more to read; unread request bytes make the close a reset.
        fn closed(&mut self) -> bool {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(read) => read == 0,
                Err(error) => error.kind() == ErrorKind::ConnectionReset,
            }
        }
    }

    /// The legacy `status` reply without its newline, which is the state object protocol 1 carries.
    fn legacy_state(session: &Session) -> String {
        session.status().trim_end().to_owned()
    }

    fn state_reply(id: u64, state: &str) -> String {
        format!("{{\"id\":{id},\"ok\":true,\"state\":{state}}}\n")
    }

    fn error_reply(id: &str, error: &str) -> String {
        format!("{{\"id\":{id},\"ok\":false,\"error\":{error}}}\n")
    }

    fn event(state: &str) -> String {
        format!("{{\"event\":\"state\",\"state\":{state}}}\n")
    }

    /// The ear detection change count in a reply or event line, which rises by one per change made here.
    fn ear_changes(line: &str) -> u64 {
        let line: Value = serde_json::from_str(line).unwrap();
        line["state"]["ear_detection_changes_total"]
            .as_u64()
            .unwrap()
    }

    /// The `set` request that moves ear detection to the `n`th of two values, so each one is a change.
    fn ear_change(id: usize) -> String {
        let value = if id.is_multiple_of(2) { "both" } else { "one" };
        format!(r#"{{"v":1,"id":{id},"op":"set","key":"ear_detection","value":"{value}"}}"#)
    }

    /// Raises the flag when dropped, so a helper thread looping on it stops even when the test panics.
    struct StopOnDrop<'flag>(&'flag AtomicBool);

    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    fn confirmed(id: usize) -> String {
        format!("{{\"id\":{id},\"ok\":true,\"result\":\"confirmed\"}}\n")
    }

    /// A `get` request of exactly `length` bytes, padded in a member the daemon ignores.
    fn padded_get(length: usize) -> String {
        let bare = r#"{"v":1,"id":1,"op":"get","pad":""}"#;
        let pad = "x".repeat(length.checked_sub(bare.len()).unwrap());
        let line = format!(r#"{{"v":1,"id":1,"op":"get","pad":"{pad}"}}"#);
        assert_eq!(line.len(), length);
        line
    }

    /// Starts `librepods watch` with its stdout piped back here.
    fn spawn_watch(session: &Session) -> (Child, BufReader<ChildStdout>) {
        let mut child = session
            .command(Path::new(BINARY), &["watch"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        (child, stdout)
    }

    fn read_watch_line(stdout: &mut BufReader<ChildStdout>) -> String {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        line
    }

    /// Waits for `child` to exit and returns its code and stderr.
    fn finish(child: &mut Child) -> (Option<i32>, String) {
        let status = wait_for("the client to exit", || child.try_wait().unwrap());
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        (status.code(), stderr)
    }

    #[test]
    fn get_returns_the_same_object_as_legacy_status() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Client::connect(&session);
        assert_eq!(client.ask(GET), state_reply(7, &legacy_state(&session)));
        assert_eq!(session.send(b"ear:off"), b"");
        assert_eq!(client.ask(GET), state_reply(7, &legacy_state(&session)));
    }

    #[test]
    fn subscribe_sends_current_state_then_one_event_per_change() {
        let session = Session::new();
        let _daemon = session.start();
        let mut subscriber = Client::connect(&session);
        assert_eq!(
            subscriber.ask(SUBSCRIBE),
            state_reply(8, &legacy_state(&session))
        );
        let mut changer = Client::connect(&session);
        let both = r#"{"v":1,"id":2,"op":"set","key":"ear_detection","value":"both"}"#;
        assert_eq!(changer.ask(both), confirmed(2));
        assert_eq!(subscriber.line(), event(&legacy_state(&session)));
        // Setting the value it already has is no change, so the next line is the reply to this get.
        assert_eq!(changer.ask(both), confirmed(2));
        assert_eq!(subscriber.ask(GET), state_reply(7, &legacy_state(&session)));
    }

    #[test]
    fn subscribe_reply_and_events_have_no_gap_or_duplicate() {
        let session = Session::new();
        let _daemon = session.start();
        let done = AtomicBool::new(false);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut changer = Client::connect(&session);
                let mut id = 0;
                while !done.load(Ordering::Relaxed) {
                    assert_eq!(changer.ask(&ear_change(id)), confirmed(id));
                    id += 1;
                }
            });
            let _stop = StopOnDrop(&done);
            for _ in 0..SUBSCRIPTIONS {
                let mut subscriber = Client::connect(&session);
                let reply = subscriber.ask(SUBSCRIBE);
                let first = subscriber.line();
                let second = subscriber.line();
                assert_eq!(
                    ear_changes(&first),
                    ear_changes(&reply) + 1,
                    "{reply}{first}"
                );
                assert_eq!(
                    ear_changes(&second),
                    ear_changes(&first) + 1,
                    "{first}{second}"
                );
            }
        });
    }

    #[test]
    fn set_while_disconnected_is_refused_with_not_connected() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Client::connect(&session);
        let before = legacy_state(&session);
        let anc = r#"{"v":1,"id":9,"op":"set","key":"noise","value":"anc"}"#;
        assert_eq!(client.ask(anc), error_reply("9", NOT_CONNECTED));
        for (id, key, value) in [
            (10, "conversational_awareness", "true"),
            (11, "one_bud_anc", "true"),
            (12, "adaptive_level", "37"),
        ] {
            let request =
                format!(r#"{{"v":1,"id":{id},"op":"set","key":"{key}","value":"{value}"}}"#);
            assert_eq!(
                client.ask(&request),
                error_reply(&id.to_string(), NOT_CONNECTED)
            );
        }
        assert_ne!(
            legacy_state(&session),
            before,
            "the refused noise change still counts"
        );
    }

    #[test]
    fn every_action_is_refused_with_not_connected_and_reopen_is_not_an_action() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Client::connect(&session);
        for action in ["connect", "disconnect", "forget"] {
            let request = format!(r#"{{"v":1,"id":10,"op":"do","action":"{action}"}}"#);
            assert_eq!(
                client.ask(&request),
                error_reply("10", NOT_CONNECTED),
                "{action}"
            );
        }
        let reopen = r#"{"v":1,"id":10,"op":"do","action":"reopen"}"#;
        assert_eq!(client.ask(reopen), error_reply("10", BAD_REQUEST));
    }

    #[test]
    fn set_ear_detection_is_confirmed_and_visible_in_state() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Client::connect(&session);
        let off = r#"{"v":1,"id":11,"op":"set","key":"ear_detection","value":"off"}"#;
        assert_eq!(client.ask(off), confirmed(11));
        let reply = client.ask(GET);
        assert_eq!(reply, state_reply(7, &legacy_state(&session)));
        let state: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(state["state"]["ear_detection_behavior"], 2);
    }

    #[test]
    fn unknown_key_is_unknown_key() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Client::connect(&session);
        let volume = r#"{"v":1,"id":12,"op":"set","key":"volume","value":"3"}"#;
        let unknown = r#"{"code":"unknown_key","message":"There is no setting by that name."}"#;
        assert_eq!(client.ask(volume), error_reply("12", unknown));
    }

    #[test]
    fn adaptive_level_101_is_out_of_range() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Client::connect(&session);
        let level = r#"{"v":1,"id":13,"op":"set","key":"adaptive_level","value":"101"}"#;
        let range = r#"{"code":"out_of_range","message":"The value is outside the range the setting takes."}"#;
        assert_eq!(client.ask(level), error_reply("13", range));
    }

    #[test]
    fn wrong_version_lists_supported_versions() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Client::connect(&session);
        let unsupported = r#"{"code":"unsupported_version","message":"This daemon speaks protocol version 1 only.","supported":[1]}"#;
        for request in [
            r#"{"v":2,"id":3,"op":"get"}"#,
            r#"{"v":"1","id":3,"op":"get"}"#,
            r#"{"v":1.0,"id":3,"op":"get"}"#,
        ] {
            assert_eq!(
                client.ask(request),
                error_reply("3", unsupported),
                "{request}"
            );
        }
    }

    #[test]
    fn garbage_line_is_bad_request_and_connection_stays_usable() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Client::connect(&session);
        let state = legacy_state(&session);
        for (request, id) in [
            ("{garbage", "null"),
            ("", "null"),
            ("[1]", "null"),
            (r#"{"id":4,"op":"get"}"#, "null"),
            (r#"{"v":1,"id":4}"#, "null"),
            (r#"{"v":1,"op":"get"}"#, "null"),
            (r#"{"v":1,"id":"4","op":"get"}"#, "null"),
            (r#"{"v":1,"id":4.5,"op":"get"}"#, "null"),
            (r#"{"v":1,"id":-4,"op":"get"}"#, "null"),
            (r#"{"v":1,"id":18446744073709551616,"op":"get"}"#, "null"),
            (r#"{"v":1,"id":4,"op":"fly"}"#, "4"),
            (r#"{"v":1,"id":4,"op":7}"#, "4"),
            (r#"{"v":1,"id":4,"op":"set","key":"noise"}"#, "4"),
            (
                r#"{"v":1,"id":4,"op":"set","key":"adaptive_level","value":50}"#,
                "4",
            ),
            (
                r#"{"v":1,"id":4,"op":"set","key":"noise","value":"loud"}"#,
                "4",
            ),
            (r#"{"v":1,"id":4,"op":"do"}"#, "4"),
        ] {
            assert_eq!(
                client.ask(request),
                error_reply(id, BAD_REQUEST),
                "{request}"
            );
        }
        client.send(b"{\"v\":1,\"id\":4,\"op\":\"g\xffet\"}\n");
        assert_eq!(client.line(), error_reply("null", BAD_REQUEST));
        let largest = r#"{"v":1,"id":18446744073709551615,"op":"get","extra":[{"a":1}]}"#;
        assert_eq!(client.ask(largest), state_reply(u64::MAX, &state));
        assert_eq!(
            client.ask(r#"{"v":1,"id":4,"id":5,"op":"get"}"#),
            state_reply(5, &state)
        );
    }

    #[test]
    fn line_over_4096_bytes_closes_the_connection() {
        let session = Session::new();
        let _daemon = session.start();
        let state = legacy_state(&session);
        let mut client = Client::connect(&session);
        assert_eq!(
            client.ask(&padded_get(MAX_LINE_BYTES)),
            state_reply(1, &state)
        );
        assert_eq!(client.ask(GET), state_reply(7, &state));
        let mut client = Client::connect(&session);
        client.send(format!("{}\n", padded_get(MAX_LINE_BYTES + 1)).as_bytes());
        assert!(client.closed());
        let mut client = Client::connect(&session);
        client.send(padded_get(MAX_LINE_BYTES + 1).as_bytes());
        assert!(
            client.closed(),
            "a line past the bound closes before its newline"
        );
        assert_eq!(Client::connect(&session).ask(GET), state_reply(7, &state));
    }

    #[test]
    fn slow_subscriber_is_dropped_and_others_keep_receiving() {
        let session = Session::new();
        let _daemon = session.start();
        let mut slow = Client::connect(&session);
        let first_reply = slow.ask(SUBSCRIBE);
        let mut fast = Client::connect(&session);
        fast.ask(SUBSCRIBE);
        let mut changer = Client::connect(&session);
        let mut getter = Client::connect(&session);
        thread::scope(|scope| {
            let reader = scope.spawn(move || (0..CHANGES).map(|_| fast.line()).collect::<Vec<_>>());
            for id in 0..CHANGES {
                assert_eq!(changer.ask(&ear_change(id)), confirmed(id));
                let started = Instant::now();
                getter.ask(GET);
                let waited = started.elapsed();
                assert!(waited < GET_WHILE_SLOW_LIMIT, "get {id} took {waited:?}");
            }
            let events = reader.join().unwrap();
            let mut expected = ear_changes(&first_reply);
            for line in events {
                expected += 1;
                assert_eq!(ear_changes(&line), expected, "{line}");
            }
        });
        let mut expected = ear_changes(&first_reply);
        let mut seen = 0;
        loop {
            let line = slow.line();
            if line.is_empty() {
                break;
            }
            expected += 1;
            seen += 1;
            assert_eq!(
                ear_changes(&line),
                expected,
                "the slow subscriber skipped a change"
            );
        }
        assert!(seen < CHANGES, "the slow subscriber was never dropped");
    }

    #[test]
    fn legacy_verbs_still_work_on_the_same_socket() {
        let session = Session::new();
        let _daemon = session.start();
        let mut subscriber = Client::connect(&session);
        subscriber.ask(SUBSCRIBE);
        assert_eq!(session.send(b"ear:both"), b"");
        assert_eq!(subscriber.line(), event(&legacy_state(&session)));
        let refusal = session.ask(b"reopen");
        assert_eq!(
            refusal,
            b"error: this daemon runs headless and has no window\n"
        );
        assert_eq!(
            subscriber.line(),
            event(&legacy_state(&session)),
            "reopen is counted"
        );
    }

    #[test]
    fn stop_signal_ends_idle_protocol_1_connections() {
        let session = Session::new();
        let mut daemon = session.start();
        let mut subscriber = Client::connect(&session);
        subscriber.ask(SUBSCRIBE);
        let mut idle = Client::connect(&session);
        idle.send(b"{\"v\":1,");
        let mut silent = Client::connect(&session);
        silent.send(b"{");
        kill_process(daemon.pid(), Signal::TERM).unwrap();
        assert!(daemon.wait_exit().success(), "{}", daemon.log());
        for client in [&mut subscriber, &mut idle, &mut silent] {
            assert!(client.closed());
        }
    }

    #[test]
    fn client_limit_counts_subscribers() {
        let session = Session::new();
        let daemon = session.start();
        let state = legacy_state(&session);
        let mut subscribers: Vec<Client> = (0..MAX_CONNECTIONS)
            .map(|_| {
                let mut subscriber = Client::connect(&session);
                assert_eq!(subscriber.ask(SUBSCRIBE), state_reply(8, &state));
                subscriber
            })
            .collect();
        let connected = Instant::now();
        let mut past = UnixStream::connect(session.socket()).unwrap();
        past.set_read_timeout(Some(CLOSED_AT_ACCEPT_WITHIN))
            .unwrap();
        let mut byte = [0_u8; 1];
        assert_eq!(
            past.read(&mut byte).unwrap(),
            0,
            "the client past the limit"
        );
        assert!(connected.elapsed() < CLOSED_AT_ACCEPT_WITHIN);
        assert!(
            daemon.log().contains("clients already connected"),
            "{}",
            daemon.log()
        );
        assert_eq!(subscribers[0].ask(GET), state_reply(7, &state));
        drop(subscribers.pop());
        // The daemon frees the slot only once it has run the departed client's task, so the first try may be closed.
        wait_for("a slot freed by the departed subscriber", || {
            let mut client = UnixStream::connect(session.socket()).ok()?;
            client.write_all(format!("{GET}\n").as_bytes()).ok()?;
            client.set_read_timeout(Some(HANG_LIMIT)).ok()?;
            let mut reply = String::new();
            BufReader::new(client).read_line(&mut reply).ok()?;
            (reply == state_reply(7, &state)).then_some(())
        });
    }

    #[test]
    fn watch_prints_a_line_per_change_and_exits_one_when_daemon_stops() {
        let session = Session::new();
        let mut daemon = session.start();
        let (mut watch, mut stdout) = spawn_watch(&session);
        assert_eq!(read_watch_line(&mut stdout), session.status());
        let set = session.run_client(Path::new(BINARY), &["set", "ear_detection", "both"]);
        assert_eq!(set.status.code(), Some(0));
        assert_eq!(read_watch_line(&mut stdout), session.status());
        assert_eq!(session.send(b"ear:one"), b"");
        assert_eq!(read_watch_line(&mut stdout), session.status());
        kill_process(daemon.pid(), Signal::TERM).unwrap();
        assert!(daemon.wait_exit().success());
        assert_eq!(read_watch_line(&mut stdout), "");
        assert_eq!(
            finish(&mut watch),
            (Some(1), "The daemon went away.\n".to_owned())
        );
    }

    #[test]
    fn watch_ends_quietly_when_nothing_reads_its_output() {
        let session = Session::new();
        let _daemon = session.start();
        let (mut watch, mut stdout) = spawn_watch(&session);
        assert_eq!(read_watch_line(&mut stdout), session.status());
        drop(stdout);
        assert_eq!(session.send(b"ear:both"), b"");
        assert_eq!(finish(&mut watch), (Some(0), String::new()));
    }

    #[test]
    fn set_exit_codes_and_messages() {
        let session = Session::new();
        let mut daemon = session.start();
        let binary = Path::new(BINARY);
        let status = session.run_client(binary, &["status"]);
        assert_eq!(status.status.code(), Some(0));
        assert_eq!(String::from_utf8(status.stdout).unwrap(), session.status());
        assert_eq!(status.stderr, b"");
        for (args, code, stderr) in [
            (&["set", "ear_detection", "off"][..], 0, ""),
            (
                &["set", "noise", "anc"],
                1,
                "The AirPods are not connected.\n",
            ),
            (
                &["set", "volume", "3"],
                1,
                "There is no setting by that name.\n",
            ),
            (
                &["set", "adaptive_level", "101"],
                1,
                "The value is outside the range the setting takes.\n",
            ),
            (
                &["set", "noise", "loud"],
                1,
                "The daemon could not read the request.\n",
            ),
            (&["do", "disconnect"], 1, "The AirPods are not connected.\n"),
            (
                &["do", "reopen"],
                1,
                "The daemon could not read the request.\n",
            ),
        ] {
            let out = session.run_client(binary, args);
            assert_eq!(out.status.code(), Some(code), "{args:?}");
            assert_eq!(out.stdout, b"", "{args:?}");
            assert_eq!(String::from_utf8(out.stderr).unwrap(), stderr, "{args:?}");
        }
        kill_process(daemon.pid(), Signal::TERM).unwrap();
        daemon.wait_exit();
        let refused = format!(
            "Could not connect to OpenPods daemon at {} (is it running?)\n",
            session.socket().display()
        );
        for args in [
            &["status"][..],
            &["watch"],
            &["set", "ear_detection", "off"],
            &["do", "connect"],
        ] {
            let out = session.run_client(binary, args);
            assert_eq!(out.status.code(), Some(1), "{args:?}");
            assert_eq!(out.stdout, b"", "{args:?}");
            assert_eq!(String::from_utf8(out.stderr).unwrap(), refused, "{args:?}");
        }
    }

    #[test]
    fn nothing_arrives_after_the_reply_without_a_subscription() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Client::connect(&session);
        client.ask(GET);
        assert_eq!(session.send(b"ear:both"), b"");
        client
            .reader
            .get_ref()
            .set_read_timeout(Some(QUIET_FOR))
            .unwrap();
        let mut line = String::new();
        let read = client.reader.read_line(&mut line);
        assert!(
            matches!(&read, Err(error) if error.kind() == ErrorKind::WouldBlock),
            "{read:?} {line}"
        );
        let mut stream = client.reader.into_inner();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        assert_eq!(read_to_close(&mut stream), b"");
    }

    #[test]
    fn a_subscriber_that_ends_its_side_gets_its_replies_and_is_closed() {
        let session = Session::new();
        let _daemon = session.start();
        let mut stream = UnixStream::connect(session.socket()).unwrap();
        stream
            .write_all(format!("{SUBSCRIBE}\n{GET}\n{{\"v\":1,").as_bytes())
            .unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let state = legacy_state(&session);
        let expected = format!("{}{}", state_reply(8, &state), state_reply(7, &state));
        assert_eq!(
            String::from_utf8(read_to_close(&mut stream)).unwrap(),
            expected
        );
    }

    #[test]
    fn the_wait_for_the_first_byte_counts_toward_the_legacy_deadline() {
        let session = Session::new();
        let _daemon = session.start();
        let mut stream = UnixStream::connect(session.socket()).unwrap();
        let connected = Instant::now();
        pause(FIRST_BYTE_AFTER);
        stream.write_all(b"s").unwrap();
        assert_eq!(read_to_close(&mut stream), b"");
        let waited = connected.elapsed();
        assert!(waited >= REQUEST_DEADLINE, "{waited:?}");
        assert!(waited < LEGACY_CLOSED_BY, "{waited:?}");
    }
}
