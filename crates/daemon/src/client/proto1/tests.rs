//! The protocol 1 commands against a stub daemon: line reading, reply checks and how `watch` ends.

use std::io::{self, Read};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use proptest::prelude::*;

use super::*;

/// A deadline short enough to keep the timeout test quick.
const SHORT_WAIT: Duration = Duration::from_millis(50);

/// How long the silent stub holds its connection, far past the reply timeout.
const HELD_FOR: Duration = Duration::from_secs(10);

/// How far past the reply bound the oversized line goes.
const READ_PAST_BOUND: usize = 2;

/// A reply line carrying a state object.
const STATE_REPLY: &[u8] = b"{\"id\":1,\"ok\":true,\"state\":{\"connected\":false}}\n";

/// An event line carrying a state object.
const STATE_EVENT: &[u8] = b"{\"event\":\"state\",\"state\":{\"connected\":true}}\n";

/// A connected pair: the reader a command uses, and the daemon's end.
fn pair() -> (BufReader<UnixStream>, UnixStream) {
    let (ours, theirs) = UnixStream::pair().unwrap();
    (BufReader::new(ours), theirs)
}

/// A stub daemon on a socket in `dir` that reads one request line, writes `lines` and closes.
fn stub(dir: &Path, lines: Vec<u8>) -> (PathBuf, thread::JoinHandle<Vec<u8>>) {
    let socket = dir.join("librepods.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let daemon = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        BufReader::new(&stream)
            .read_until(b'\n', &mut request)
            .unwrap();
        // The command may stop reading early, so a refused write is expected here.
        drop((&stream).write_all(&lines));
        request
    });
    (socket, daemon)
}

/// Standard output with nothing reading it.
struct ClosedPipe;

impl Write for ClosedPipe {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn requests_carry_the_version_the_id_and_string_members() {
    assert_eq!(
        request("set", &[("key", "noise"), ("value", "anc")]),
        "{\"id\":1,\"key\":\"noise\",\"op\":\"set\",\"v\":1,\"value\":\"anc\"}\n"
    );
    assert_eq!(request("get", &[]), "{\"id\":1,\"op\":\"get\",\"v\":1}\n");
}

#[test]
fn lines_are_read_one_at_a_time_and_the_end_is_the_daemon_going_away() {
    let (mut reader, mut daemon) = pair();
    daemon.write_all(b"first\nsec").unwrap();
    assert_eq!(read_line(&mut reader, None).unwrap(), b"first");
    daemon.write_all(b"ond\n").unwrap();
    assert_eq!(read_line(&mut reader, None).unwrap(), b"second");
    drop(daemon);
    assert!(matches!(
        read_line(&mut reader, None),
        Err(Failure::WentAway)
    ));
}

#[test]
fn a_line_past_the_bound_is_garbled() {
    let (mut reader, mut daemon) = pair();
    let writer = thread::spawn(move || {
        // The reader stops at the bound and drops its end, so a refused write is expected here.
        drop(daemon.write_all(&vec![b'x'; MAX_REPLY_BYTES + READ_PAST_BOUND]));
    });
    assert!(matches!(
        read_line(&mut reader, None),
        Err(Failure::Garbled(_))
    ));
    drop(reader);
    writer.join().unwrap();
}

#[test]
fn a_reply_that_does_not_come_by_the_deadline_times_out() {
    let (mut reader, _daemon) = pair();
    let started = Instant::now();
    let deadline = started.checked_add(SHORT_WAIT);
    assert!(matches!(
        read_line(&mut reader, deadline),
        Err(Failure::TimedOut)
    ));
    assert!(started.elapsed() >= SHORT_WAIT);
}

#[test]
fn replies_are_taken_refused_or_garbled() {
    assert!(reply(br#"{"id":1,"ok":true,"result":"sent"}"#).is_ok());
    let refused = reply(br#"{"id":1,"ok":false,"error":{"code":"x","message":"No."}}"#);
    assert!(matches!(refused, Err(Failure::Refused(message)) if message == "No."));
    for line in [
        b"not json".as_slice(),
        br#"{"id":1}"#,
        br#"{"id":1,"ok":"true"}"#,
        br#"{"id":1,"ok":false,"error":{"code":"x"}}"#,
        b"[true]",
    ] {
        assert!(matches!(reply(line), Err(Failure::Garbled(_))), "{line:?}");
    }
}

#[test]
fn only_a_state_event_gives_a_state_line() {
    let line = STATE_EVENT.strip_suffix(b"\n").unwrap();
    assert_eq!(event_state_line(line).unwrap(), "{\"connected\":true}\n");
    for line in [
        br#"{"event":"other","state":{}}"#.as_slice(),
        br#"{"event":"state","state":1}"#,
        br#"{"id":1,"ok":true,"state":{}}"#,
    ] {
        assert!(
            matches!(event_state_line(line), Err(Failure::Garbled(_))),
            "{line:?}"
        );
    }
}

#[test]
fn watch_prints_each_state_and_reports_the_daemon_going_away() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, daemon) = stub(dir.path(), [STATE_REPLY, STATE_EVENT].concat());
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(watch(&socket, &mut out, &mut err), ExitCode::FAILURE);
    assert_eq!(out, b"{\"connected\":false}\n{\"connected\":true}\n");
    assert_eq!(err, WENT_AWAY.as_bytes());
    assert_eq!(
        daemon.join().unwrap(),
        request("subscribe", &[]).into_bytes()
    );
}

#[test]
fn watch_with_nothing_reading_its_output_ends_quietly() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, daemon) = stub(dir.path(), STATE_REPLY.to_vec());
    let mut err = Vec::new();
    assert_eq!(watch(&socket, &mut ClosedPipe, &mut err), ExitCode::SUCCESS);
    assert_eq!(err, b"");
    daemon.join().unwrap();
}

#[test]
fn a_garbled_reply_is_reported_with_the_line() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, daemon) = stub(dir.path(), b"hello\n".to_vec());
    let mut err = Vec::new();
    assert_eq!(set("noise", "anc", &socket, &mut err), ExitCode::FAILURE);
    let err = String::from_utf8(err).unwrap();
    assert!(
        err.starts_with("The daemon sent a line that is not protocol 1, "),
        "{err}"
    );
    assert!(err.contains("hello"), "{err}");
    let mut request = daemon.join().unwrap();
    assert_eq!(request.pop(), Some(b'\n'));
    let mut sent = String::new();
    request.as_slice().read_to_string(&mut sent).unwrap();
    assert!(sent.contains("\"value\":\"anc\""), "{sent}");
}

#[test]
fn a_daemon_that_accepts_but_never_replies_times_out() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("librepods.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let held = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        BufReader::new(&stream)
            .read_until(b'\n', &mut request)
            .unwrap();
        // Held open past the reply timeout, so only the deadline ends the wait; a client with no deadline sees the close.
        stream.set_read_timeout(Some(HELD_FOR)).unwrap();
        let mut rest = Vec::new();
        drop(BufReader::new(&stream).read_to_end(&mut rest));
    });
    let started = Instant::now();
    let mut err = Vec::new();
    assert_eq!(act("connect", &socket, &mut err), ExitCode::FAILURE);
    assert!(started.elapsed() >= REPLY_TIMEOUT);
    assert_eq!(err, b"Timed out waiting for a reply to do\n");
    held.join().unwrap();
}

proptest! {
    #[test]
    fn any_reply_bytes_are_taken_refused_or_garbled(line in proptest::collection::vec(any::<u8>(), 0..256)) {
        let taken = reply(&line).is_ok();
        let ok = serde_json::from_slice::<Value>(&line).ok().and_then(|reply| reply.get("ok")?.as_bool());
        prop_assert_eq!(taken, ok == Some(true));
    }
}
