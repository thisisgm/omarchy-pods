//! Drives protocol 1 on the built daemon's socket, and the `status`, `watch`, `set` and `do` commands, each run in a temp directory.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "these tests talk only to the socket, so the state file helper the other suites share goes unused"
)]
mod support;

#[cfg(test)]
mod commands;

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use serde_json::Value;

    use super::support::{GET, HANG_LIMIT, Proto1, Session, pause, read_to_close, wait_for};

    /// The longest request line the daemon takes, newline excluded, from `MAX_LINE_BYTES` in `src/daemon/proto1.rs`.
    const MAX_LINE_BYTES: usize = 4096;

    /// Clients the daemon serves at once, from `MAX_CONNECTIONS` in `src/daemon/mod.rs`.
    const MAX_CONNECTIONS: usize = 64;

    /// The changes the slow subscriber and pipelining tests make, past what one socket buffer and the daemon's queues hold.
    const CHANGES: usize = 200;

    /// Past the 64 changes the hub lets a subscriber leave untaken, within the 16 queued lines plus the ~92 event lines a default 208 KiB socket buffer holds.
    const BURST_CHANGES: usize = 96;

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

    /// How long a client may leave a line unfinished, a reply unread or, unsubscribed, the connection idle, from `STALL_DEADLINE` in `src/daemon/proto1.rs`.
    const STALL_DEADLINE: Duration = Duration::from_millis(2000);

    /// The stall deadline plus slack for a shared host, well short of a deadline started again at each dripped byte.
    const STALL_CLOSED_BY: Duration = Duration::from_millis(3000);

    /// The pause between the bytes of the dripped line, so several bytes land inside one stall deadline.
    const DRIP_PAUSE: Duration = Duration::from_millis(300);

    /// Bytes in the dripped line, so the drip would outlast the stall deadline three times over.
    const DRIP_BYTES: usize = 20;

    /// Requests the non-reading client sends, far past what the socket buffers, the queue and the pending bytes hold.
    const UNREAD_GETS: usize = 5000;

    /// Requests the steady client spaces half a stall deadline apart, more than one deadline in all.
    const STEADY_REQUESTS: usize = 4;

    const SUBSCRIBE: &str = r#"{"v":1,"id":8,"op":"subscribe"}"#;
    const NOT_CONNECTED: &str =
        r#"{"code":"not_connected","message":"The AirPods are not connected."}"#;
    const NO_DEVICE: &str = r#"{"code":"no_device","message":"There are no AirPods to act on."}"#;
    const BAD_REQUEST: &str =
        r#"{"code":"bad_request","message":"The daemon could not read the request."}"#;

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

    /// The count `name` in the state of a reply or event line.
    fn counter(line: &str, name: &str) -> u64 {
        let line: Value = serde_json::from_str(line).unwrap();
        line["state"][name].as_u64().unwrap()
    }

    /// The ear detection change count in a reply or event line, which rises by one per change made here.
    fn ear_changes(line: &str) -> u64 {
        counter(line, "ear_detection_changes_total")
    }

    /// The noise control change count in a reply or event line, which a refused noise set raises too.
    fn noise_changes(line: &str) -> u64 {
        counter(line, "noise_control_changes_total")
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

    #[test]
    fn get_returns_the_same_object_as_legacy_status() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Proto1::connect(&session);
        assert_eq!(
            client.ask_line(GET),
            state_reply(7, &legacy_state(&session))
        );
        assert_eq!(session.send(b"ear:off"), b"");
        assert_eq!(
            client.ask_line(GET),
            state_reply(7, &legacy_state(&session))
        );
    }

    #[test]
    fn subscribe_sends_current_state_then_one_event_per_change() {
        let session = Session::new();
        let _daemon = session.start();
        let mut subscriber = Proto1::connect(&session);
        assert_eq!(
            subscriber.ask_line(SUBSCRIBE),
            state_reply(8, &legacy_state(&session))
        );
        let mut changer = Proto1::connect(&session);
        let both = r#"{"v":1,"id":2,"op":"set","key":"ear_detection","value":"both"}"#;
        assert_eq!(changer.ask_line(both), confirmed(2));
        assert_eq!(subscriber.line(), event(&legacy_state(&session)));
        // Setting the value it already has is no change, so the next line is the reply to this get.
        assert_eq!(changer.ask_line(both), confirmed(2));
        assert_eq!(
            subscriber.ask_line(GET),
            state_reply(7, &legacy_state(&session))
        );
    }

    #[test]
    fn subscribe_reply_and_events_have_no_gap_or_duplicate() {
        let session = Session::new();
        let _daemon = session.start();
        let done = AtomicBool::new(false);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut changer = Proto1::connect(&session);
                let mut id = 0;
                while !done.load(Ordering::Relaxed) {
                    assert_eq!(changer.ask_line(&ear_change(id)), confirmed(id));
                    id += 1;
                }
            });
            let _stop = StopOnDrop(&done);
            for _ in 0..SUBSCRIPTIONS {
                let mut subscriber = Proto1::connect(&session);
                let reply = subscriber.ask_line(SUBSCRIBE);
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
        let mut client = Proto1::connect(&session);
        let before = legacy_state(&session);
        let anc = r#"{"v":1,"id":9,"op":"set","key":"noise","value":"anc"}"#;
        assert_eq!(client.ask_line(anc), error_reply("9", NOT_CONNECTED));
        assert_ne!(
            legacy_state(&session),
            before,
            "the refused noise change still counts"
        );
        for (id, key, value) in [
            (10, "conversational_awareness", "true"),
            (11, "one_bud_anc", "true"),
            (12, "adaptive_level", "37"),
        ] {
            let request =
                format!(r#"{{"v":1,"id":{id},"op":"set","key":"{key}","value":"{value}"}}"#);
            assert_eq!(
                client.ask_line(&request),
                error_reply(&id.to_string(), NOT_CONNECTED)
            );
        }
    }

    #[test]
    fn every_action_with_no_airpods_known_is_no_device_and_reopen_is_not_an_action() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Proto1::connect(&session);
        for action in ["connect", "disconnect", "forget"] {
            let request = format!(r#"{{"v":1,"id":10,"op":"do","action":"{action}"}}"#);
            assert_eq!(
                client.ask_line(&request),
                error_reply("10", NO_DEVICE),
                "{action}"
            );
        }
        let reopen = r#"{"v":1,"id":10,"op":"do","action":"reopen"}"#;
        assert_eq!(client.ask_line(reopen), error_reply("10", BAD_REQUEST));
    }

    #[test]
    fn set_ear_detection_is_confirmed_and_visible_in_state() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Proto1::connect(&session);
        let off = r#"{"v":1,"id":11,"op":"set","key":"ear_detection","value":"off"}"#;
        assert_eq!(client.ask_line(off), confirmed(11));
        let reply = client.ask_line(GET);
        assert_eq!(reply, state_reply(7, &legacy_state(&session)));
        let state: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(state["state"]["ear_detection_behavior"], 2);
    }

    #[test]
    fn unknown_key_is_unknown_key() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Proto1::connect(&session);
        let volume = r#"{"v":1,"id":12,"op":"set","key":"volume","value":"3"}"#;
        let unknown = r#"{"code":"unknown_key","message":"There is no setting by that name."}"#;
        assert_eq!(client.ask_line(volume), error_reply("12", unknown));
    }

    #[test]
    fn adaptive_level_100_is_taken_and_101_is_out_of_range() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Proto1::connect(&session);
        let taken = r#"{"v":1,"id":13,"op":"set","key":"adaptive_level","value":"100"}"#;
        assert_eq!(client.ask_line(taken), error_reply("13", NOT_CONNECTED));
        let level = r#"{"v":1,"id":13,"op":"set","key":"adaptive_level","value":"101"}"#;
        let range = r#"{"code":"out_of_range","message":"The value is outside the range the setting takes."}"#;
        assert_eq!(client.ask_line(level), error_reply("13", range));
    }

    #[test]
    fn wrong_version_lists_supported_versions() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Proto1::connect(&session);
        let unsupported = r#"{"code":"unsupported_version","message":"This daemon speaks protocol version 1 only.","supported":[1]}"#;
        for request in [
            r#"{"v":2,"id":3,"op":"get"}"#,
            r#"{"v":"1","id":3,"op":"get"}"#,
            r#"{"v":1.0,"id":3,"op":"get"}"#,
            r#"{"v":2,"id":3}"#,
        ] {
            assert_eq!(
                client.ask_line(request),
                error_reply("3", unsupported),
                "{request}"
            );
        }
    }

    #[test]
    fn garbage_line_is_bad_request_and_connection_stays_usable() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Proto1::connect(&session);
        let state = legacy_state(&session);
        for (request, id) in [
            ("{garbage", "null"),
            ("", "null"),
            ("[1]", "null"),
            (r#"{"id":4,"op":"get"}"#, "4"),
            (r#"{"v":1,"id":4}"#, "4"),
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
            (r#"{"v":1,"id":4,"op":"do"}"#, "4"),
        ] {
            assert_eq!(
                client.ask_line(request),
                error_reply(id, BAD_REQUEST),
                "{request}"
            );
        }
        client.write(b"{\"v\":1,\"id\":4,\"op\":\"g\xffet\"}\n");
        assert_eq!(client.line(), error_reply("null", BAD_REQUEST));
        let largest = r#"{"v":1,"id":18446744073709551615,"op":"get","extra":[{"a":1}]}"#;
        assert_eq!(client.ask_line(largest), state_reply(u64::MAX, &state));
        assert_eq!(
            client.ask_line(r#"{"v":1,"id":4,"id":5,"op":"get"}"#),
            state_reply(5, &state)
        );
    }

    #[test]
    fn line_over_4096_bytes_closes_the_connection() {
        let session = Session::new();
        let daemon = session.start();
        let state = legacy_state(&session);
        let mut client = Proto1::connect(&session);
        assert_eq!(
            client.ask_line(&padded_get(MAX_LINE_BYTES)),
            state_reply(1, &state)
        );
        assert_eq!(client.ask_line(GET), state_reply(7, &state));
        let mut client = Proto1::connect(&session);
        client.write(format!("{}\n", padded_get(MAX_LINE_BYTES + 1)).as_bytes());
        assert!(client.closed());
        let mut client = Proto1::connect(&session);
        client.write(padded_get(MAX_LINE_BYTES + 1).as_bytes());
        assert!(
            client.closed(),
            "a line past the bound closes before its newline"
        );
        assert_eq!(
            Proto1::connect(&session).ask_line(GET),
            state_reply(7, &state)
        );
        // The daemon logs a close before it makes it, and a stall close would log its own reason instead.
        let log = daemon.log();
        let bound = format!("longer than {MAX_LINE_BYTES} bytes");
        assert_eq!(log.matches(&bound).count(), 2, "{log}");
    }

    #[test]
    fn a_pipelined_burst_of_changes_reaches_a_subscriber_whole() {
        let session = Session::new();
        let _daemon = session.start();
        let mut subscriber = Proto1::connect(&session);
        let mut expected = ear_changes(&subscriber.ask_line(SUBSCRIBE));
        let mut burst = Proto1::connect(&session);
        let lines: String = (0..BURST_CHANGES).map(|id| ear_change(id) + "\n").collect();
        burst.write(lines.as_bytes());
        for id in 0..BURST_CHANGES {
            assert_eq!(burst.line(), confirmed(id));
        }
        // Read only after the burst, so the result never rests on how soon a reader thread is scheduled.
        for seen in 0..BURST_CHANGES {
            let line = subscriber.line();
            assert!(
                !line.is_empty(),
                "the subscriber was closed after {seen} events"
            );
            expected += 1;
            assert_eq!(ear_changes(&line), expected, "{line}");
        }
    }

    #[test]
    fn a_subscriber_pipelining_its_own_changes_gets_every_reply_and_event() {
        let session = Session::new();
        let _daemon = session.start();
        let mut subscriber = Proto1::connect(&session);
        let mut expected = noise_changes(&subscriber.ask_line(SUBSCRIBE));
        // A refused set still counts a change, and lines this short put more changes in one read than the hub holds.
        let refused = r#"{"v":1,"id":0,"op":"set","key":"noise","value":"anc"}"#;
        subscriber.write(format!("{refused}\n").repeat(CHANGES).as_bytes());
        let mut replies = 0;
        let mut events = 0;
        while replies < CHANGES || events < CHANGES {
            let line = subscriber.line();
            if line.starts_with(r#"{"event""#) {
                expected += 1;
                assert_eq!(
                    noise_changes(&line),
                    expected,
                    "after {replies} replies and {events} events"
                );
                events += 1;
            } else {
                assert_eq!(
                    line,
                    error_reply("0", NOT_CONNECTED),
                    "after {replies} replies and {events} events"
                );
                replies += 1;
            }
        }
        // Every change has had its event, so the next line answers this get.
        assert_eq!(
            subscriber.ask_line(GET),
            state_reply(7, &legacy_state(&session))
        );
    }

    #[test]
    fn slow_subscriber_is_dropped_and_others_keep_receiving() {
        let session = Session::new();
        let _daemon = session.start();
        let mut slow = Proto1::connect(&session);
        let first_reply = slow.ask_line(SUBSCRIBE);
        let mut fast = Proto1::connect(&session);
        fast.ask_line(SUBSCRIBE);
        let mut changer = Proto1::connect(&session);
        let mut getter = Proto1::connect(&session);
        thread::scope(|scope| {
            let reader = scope.spawn(move || (0..CHANGES).map(|_| fast.line()).collect::<Vec<_>>());
            let mut changes_made = ear_changes(&first_reply);
            for id in 0..CHANGES {
                assert_eq!(changer.ask_line(&ear_change(id)), confirmed(id));
                let reply = getter.ask_line(GET);
                changes_made += 1;
                assert_eq!(ear_changes(&reply), changes_made, "{reply}");
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
    fn a_subscriber_keeps_16_unsent_lines_and_the_17th_closes_it() {
        let session = Session::new();
        let daemon = session.start();
        let mut subscriber = Proto1::connect(&session);
        subscriber.ask_line(SUBSCRIBE);
        let mut changer = Proto1::connect(&session);
        // The set runs every subscriber's task before its reply, so a close it caused is logged by then.
        let closed_at = (0..CHANGES)
            .position(|id| {
                assert_eq!(changer.ask_line(&ear_change(id)), confirmed(id));
                daemon.log().contains("protocol 1 client closed")
            })
            .expect("the subscriber that reads nothing was never closed");
        let log = daemon.log();
        assert!(log.contains("lines unread"), "{log}");
        let sent = lines_until_closed(&mut subscriber.reader.into_inner());
        assert_eq!(closed_at + 1 - sent, 17, "changes past the socket buffer");
    }

    #[test]
    fn legacy_verbs_still_work_on_the_same_socket() {
        let session = Session::new();
        let _daemon = session.start();
        let mut subscriber = Proto1::connect(&session);
        subscriber.ask_line(SUBSCRIBE);
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

    /// Reads until the daemon closes, which a reset also shows, and returns the lines read; a read that times out fails the test.
    fn lines_until_closed(stream: &mut UnixStream) -> usize {
        stream.set_read_timeout(Some(HANG_LIMIT)).unwrap();
        let mut received = Vec::new();
        match stream.read_to_end(&mut received) {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::ConnectionReset => {}
            Err(error) => panic!("still open after {HANG_LIMIT:?}: {error}"),
        }
        received.as_slice().lines().count()
    }

    #[test]
    fn a_stalled_client_is_closed_and_an_idle_subscriber_is_not() {
        let session = Session::new();
        let daemon = session.start();
        let state = legacy_state(&session);
        let mut subscriber = Proto1::connect(&session);
        subscriber.ask_line(SUBSCRIBE);
        let started = Instant::now();
        let mut steady = Proto1::connect(&session);
        let mut unfinished = Proto1::connect(&session);
        unfinished.write(b"{");
        let mut idle = Proto1::connect(&session);
        idle.ask_line(GET);
        let mut unread = UnixStream::connect(session.socket()).unwrap();
        let mut writer = unread.try_clone().unwrap();
        writer.set_write_timeout(Some(HANG_LIMIT)).unwrap();
        let flood = format!("{SUBSCRIBE}\n{}", format!("{GET}\n").repeat(UNREAD_GETS));
        thread::scope(|scope| {
            // The write fails once the daemon closes the connection, the outcome under test, or once HANG_LIMIT passes.
            scope.spawn(move || drop(writer.write_all(flood.as_bytes())));
            let state = &state;
            // Moved in, so the steady client hangs up when done instead of staying for a stall close of its own.
            scope.spawn(move || {
                for _ in 0..STEADY_REQUESTS {
                    assert_eq!(steady.ask_line(GET), state_reply(7, state));
                    pause(STALL_DEADLINE / 2);
                }
            });
            assert!(unfinished.closed(), "the unfinished line");
            assert!(
                started.elapsed() >= STALL_DEADLINE,
                "closed before its deadline"
            );
            assert!(idle.closed(), "the idle connection");
            pause(STALL_DEADLINE);
            assert!(
                lines_until_closed(&mut unread) < UNREAD_GETS,
                "the unread replies"
            );
        });
        // The daemon names its own deadline, so a drift from the copy here shows.
        let stalled = format!("made no progress for {} ms", STALL_DEADLINE.as_millis());
        let log = daemon.log();
        assert_eq!(log.matches(&stalled).count(), 3, "{log}");
        assert_eq!(subscriber.ask_line(GET), state_reply(7, &state));
    }

    #[test]
    fn a_line_dripped_a_byte_at_a_time_is_closed_by_the_stall_deadline() {
        let session = Session::new();
        let _daemon = session.start();
        let mut stream = UnixStream::connect(session.socket()).unwrap();
        let mut reader = stream.try_clone().unwrap();
        reader.set_read_timeout(Some(HANG_LIMIT)).unwrap();
        // The first byte chooses protocol 1, and no byte ends the line.
        let mut line = [b' '; DRIP_BYTES];
        line[0] = b'{';
        let closed = AtomicBool::new(false);
        let started = Instant::now();
        let (read, waited) = thread::scope(|scope| {
            let watcher = scope.spawn(|| {
                let read = reader.read(&mut [0_u8; 1]);
                let waited = started.elapsed();
                closed.store(true, Ordering::Relaxed);
                (read, waited)
            });
            for byte in line {
                if closed.load(Ordering::Relaxed) || stream.write_all(&[byte]).is_err() {
                    break;
                }
                pause(DRIP_PAUSE);
            }
            watcher.join().unwrap()
        });
        assert!(
            matches!(&read, Ok(0))
                || matches!(&read, Err(error) if error.kind() == ErrorKind::ConnectionReset),
            "{read:?}"
        );
        assert!(
            (STALL_DEADLINE..STALL_CLOSED_BY).contains(&waited),
            "{waited:?}"
        );
    }

    #[test]
    fn stop_signal_ends_idle_protocol_1_connections() {
        let session = Session::new();
        let mut daemon = session.start();
        let mut subscriber = Proto1::connect(&session);
        subscriber.ask_line(SUBSCRIBE);
        let mut idle = Proto1::connect(&session);
        idle.write(b"{\"v\":1,");
        let mut silent = Proto1::connect(&session);
        silent.write(b"{");
        assert!(daemon.stop().success(), "{}", daemon.log());
        // A connection that ended before the stop, by its stall deadline or otherwise, logs its own close.
        let log = daemon.log();
        assert!(!log.contains("protocol 1 client closed"), "{log}");
    }

    #[test]
    fn client_limit_counts_subscribers() {
        let session = Session::new();
        let daemon = session.start();
        let state = legacy_state(&session);
        let mut subscribers: Vec<Proto1> = (0..MAX_CONNECTIONS)
            .map(|_| {
                let mut subscriber = Proto1::connect(&session);
                assert_eq!(subscriber.ask_line(SUBSCRIBE), state_reply(8, &state));
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
        assert_eq!(subscribers[0].ask_line(GET), state_reply(7, &state));
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
    fn nothing_arrives_after_the_reply_without_a_subscription() {
        let session = Session::new();
        let _daemon = session.start();
        let mut client = Proto1::connect(&session);
        client.ask_line(GET);
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
        let daemon = session.start();
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
        // The end of input closes it quietly, and a close by the stall deadline would be logged.
        let log = daemon.log();
        assert!(!log.contains("protocol 1 client closed"), "{log}");
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
