//! The protocol 1 commands against a stub daemon: line reading, reply checks and how `watch` ends.

use std::io::{self, Read};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use proptest::prelude::*;
use serde_json::json;

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

/// The bytes around a padded state reply: `{"id":1,"ok":true,"state":{"p":"` and `"}}`.
const PADDED_REPLY_FRAME_BYTES: usize = 35;

/// Longest stream in the property over short lines.
const SHORT_STREAM_BYTES: usize = 512;

/// Largest read buffer in the property over short lines, so lines are split at many points.
const SHORT_READ_BYTES: usize = 64;

/// How far either side of `MAX_REPLY_BYTES` the long lines reach.
const BOUND_MARGIN_BYTES: usize = 2;

/// Runs of the long line property; each one moves `MAX_REPLY_BYTES` through a socket.
const LONG_LINE_CASES: u32 = 16;

/// How often each member of a generated line is present, so the documented shapes come up often.
const MEMBER_PRESENT: f64 = 0.9;

/// Runs of the properties over generated objects; they touch no socket, and the rare shapes need many runs.
const OBJECT_CASES: u32 = 1024;

/// Lines in the properties over arbitrary bytes are shorter than this, long enough for a small reply object.
const ANY_LINE_BYTES: usize = 256;

/// Any string or object key up to 12 characters, long enough for every word the protocol uses.
const ANY_STRING: &str = ".{0,12}";

/// How deep a generated JSON value nests; the parsers read only the top two levels.
const VALUE_DEPTH: u32 = 3;

/// Nodes proptest aims for in one generated JSON value, a target not a cap, small so runs stay fast.
const VALUE_NODES: u32 = 24;

/// Items proptest expects per generated array or object, a hint not a cap, the same as `VALUE_ITEMS`.
const VALUE_BRANCH: u32 = 4;

/// Generated arrays and objects hold fewer items than this; members are read by name, so more add no paths.
const VALUE_ITEMS: usize = 4;

/// Weight of the documented shapes among a generated member's values, so they come up most.
const DOCUMENTED_WEIGHT: u32 = 3;

/// Weight of a null `id`, the one other `id` a refusal may carry.
const NULL_ID_WEIGHT: u32 = 2;

/// Weight of every other value a generated member may take.
const OTHER_WEIGHT: u32 = 1;

/// Members a reply or an event may hold.
const MEMBERS: [&str; 6] = ["id", "ok", "state", "result", "error", "event"];

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

/// What a line from the daemon comes to, so a result can be compared with the rule.
#[derive(Debug, PartialEq)]
enum Outcome {
    Taken(String),
    Refused(String),
    NotUnderstood,
}

/// The outcome a parser returned; any other failure is a parser bug.
fn outcome(parsed: Result<String, Failure>) -> Outcome {
    match parsed {
        Ok(line) => Outcome::Taken(line),
        Err(Failure::Refused(message)) => Outcome::Refused(message),
        Err(Failure::NotUnderstood) => Outcome::NotUnderstood,
        Err(failure) => panic!("a parser returned {failure:?}"),
    }
}

/// `result_reply` as an outcome, its success taken as an empty line.
fn result_outcome(line: &[u8]) -> Outcome {
    outcome(result_reply(line).map(|()| String::new()))
}

/// The rule for a reply to request 1, stated apart from the client; `success` is what an `ok` reply must carry.
fn expected_reply(value: &Value, success: fn(&Value) -> Option<String>) -> Outcome {
    if !value.is_object() {
        return Outcome::NotUnderstood;
    }
    let id = value.get("id");
    let echoed = id == Some(&json!(1));
    let null_id_allowed =
        id == Some(&Value::Null) && value.pointer("/error/code") == Some(&json!("bad_request"));
    match (value.get("ok"), value.pointer("/error/message")) {
        (Some(Value::Bool(true)), _) if echoed => {
            success(value).map_or(Outcome::NotUnderstood, Outcome::Taken)
        }
        (Some(Value::Bool(false)), Some(Value::String(message))) if echoed || null_id_allowed => {
            Outcome::Refused(message.clone())
        }
        _ => Outcome::NotUnderstood,
    }
}

/// What a successful reply to `get` or `subscribe` carries: a state object, printed as a line.
fn state_success(value: &Value) -> Option<String> {
    value
        .get("state")
        .filter(|state| state.is_object())
        .map(|state| format!("{state}\n"))
}

/// What a successful reply to `set` or `do` carries: a documented result.
fn result_success(value: &Value) -> Option<String> {
    matches!(value.get("result"), Some(Value::String(result)) if result == "sent" || result == "confirmed")
        .then(String::new)
}

/// The rule for an event line, stated apart from the client.
fn expected_event(value: &Value) -> Outcome {
    if value.is_object() && value.get("event") == Some(&json!("state")) {
        state_success(value).map_or(Outcome::NotUnderstood, Outcome::Taken)
    } else {
        Outcome::NotUnderstood
    }
}

/// The JSON a line holds, or none when it is not one JSON value.
fn as_json(line: &[u8]) -> Option<Value> {
    serde_json::from_slice(line).ok()
}

/// The line `value` is written as, and the value read back from it, since a float comes back from text a digit apart.
fn written(value: &Value) -> (Vec<u8>, Value) {
    let line = serde_json::to_vec(value).unwrap();
    let read = as_json(&line).unwrap();
    (line, read)
}

/// Feeds the `chunks` of one line to `take` in order, as `read_line` does, until the line ends or is refused.
fn take_chunks(chunks: &[&[u8]]) -> Result<Vec<u8>, Failure> {
    let mut line = Vec::new();
    for chunk in chunks {
        if let Step::Ended(_) = take(&mut line, chunk)? {
            return Ok(line);
        }
    }
    Err(Failure::WentAway)
}

/// `bytes` cut at each of the ascending offsets `cuts`.
fn split_at<'bytes>(bytes: &'bytes [u8], cuts: &[usize]) -> Vec<&'bytes [u8]> {
    let mut chunks = Vec::new();
    let mut from = 0;
    for &cut in cuts {
        chunks.push(&bytes[from..cut]);
        from = cut;
    }
    chunks.push(&bytes[from..]);
    chunks
}

/// The ways one line of `length` bytes and its newline is cut: whole, around the bound, and one byte at a time.
fn cut_sets(length: usize) -> Vec<Vec<usize>> {
    let bound = MAX_REPLY_BYTES;
    vec![
        vec![],
        vec![1],
        vec![bound - 1],
        vec![bound],
        vec![length],
        vec![1, bound],
        vec![bound - 2, bound - 1, bound, length],
        (1..=length).collect(),
    ]
}

/// The lines `stream` holds by the rule, and whether reading it ends in a refusal rather than at its end.
fn expected_lines(stream: &[u8]) -> (Vec<&[u8]>, bool) {
    let mut parts: Vec<&[u8]> = stream.split(|&byte| byte == b'\n').collect();
    let unfinished = parts.pop().unwrap();
    let mut lines = Vec::new();
    for part in parts {
        if part.len() > MAX_REPLY_BYTES {
            return (lines, true);
        }
        lines.push(part);
    }
    (lines, unfinished.len() > MAX_REPLY_BYTES)
}

/// Reads `stream` through `read_line` with a buffer of `capacity` bytes and checks it against the rule.
fn check_read(stream: Vec<u8>, capacity: usize) {
    let (ours, mut daemon) = UnixStream::pair().unwrap();
    let mut reader = BufReader::with_capacity(capacity, ours);
    let expected = stream.clone();
    let writer = thread::spawn(move || {
        // The reader stops at a refused line and drops its end, so a refused write is expected here.
        drop(daemon.write_all(&stream));
    });
    let (expected_lines, refused) = expected_lines(&expected);
    let mut lines = Vec::new();
    let ending = loop {
        match read_line(&mut reader, None) {
            Ok(line) if lines.len() < expected_lines.len() => lines.push(line),
            // A reader that finds more lines than the rule gives stops here rather than loop.
            Ok(line) => panic!("a line the rule does not give: {line:?}"),
            Err(failure) => break failure,
        }
    };
    drop(reader);
    writer.join().unwrap();
    assert_eq!(lines, expected_lines);
    if refused {
        assert!(matches!(ending, Failure::NotUnderstood), "{ending:?}");
    } else {
        assert!(matches!(ending, Failure::WentAway), "{ending:?}");
    }
}

/// A state reply padded to exactly `length` bytes, newline excluded.
fn padded_state_reply(length: usize) -> Vec<u8> {
    let padding = "p".repeat(length.checked_sub(PADDED_REPLY_FRAME_BYTES).unwrap());
    let reply = format!("{{\"id\":1,\"ok\":true,\"state\":{{\"p\":\"{padding}\"}}}}");
    assert_eq!(reply.len(), length);
    format!("{reply}\n").into_bytes()
}

/// Any JSON value, nested a few levels, with the values the protocol uses among the leaves.
fn any_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<f64>().prop_map(Value::from),
        prop_oneof![
            Just("state"),
            Just("sent"),
            Just("confirmed"),
            Just("bad_request"),
            Just("not_connected"),
            Just("message"),
        ]
        .prop_map(Value::from),
        ANY_STRING.prop_map(Value::from),
    ];
    leaf.prop_recursive(VALUE_DEPTH, VALUE_NODES, VALUE_BRANCH, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..VALUE_ITEMS).prop_map(Value::Array),
            proptest::collection::btree_map(ANY_STRING, inner, 0..VALUE_ITEMS)
                .prop_map(|map| Value::Object(map.into_iter().collect())),
        ]
    })
}

/// An `error` member: an object with a code and a message each present or not, or any value.
fn any_error() -> impl Strategy<Value = Value> {
    let code = prop_oneof![
        Just(json!("bad_request")),
        Just(json!("not_connected")),
        any_value()
    ];
    let message = prop_oneof![Just(json!("No.")), any_value()];
    let object = (
        proptest::option::weighted(MEMBER_PRESENT, code),
        proptest::option::weighted(MEMBER_PRESENT, message),
    )
        .prop_map(|(code, message)| {
            let mut error = Map::new();
            if let Some(code) = code {
                error.insert("code".to_owned(), code);
            }
            if let Some(message) = message {
                error.insert("message".to_owned(), message);
            }
            Value::Object(error)
        });
    prop_oneof![DOCUMENTED_WEIGHT => object, OTHER_WEIGHT => any_value()]
}

/// An object of reply and event members, each present or not, near the documented shapes as often as not.
fn any_line_object() -> impl Strategy<Value = Value> {
    let id = prop_oneof![
        DOCUMENTED_WEIGHT => Just(json!(1)),
        OTHER_WEIGHT => Just(json!(2)),
        OTHER_WEIGHT => Just(json!(1.0)),
        NULL_ID_WEIGHT => Just(Value::Null),
        OTHER_WEIGHT => Just(json!("1")),
        OTHER_WEIGHT => any_value(),
    ];
    let ok = prop_oneof![Just(json!(true)), Just(json!(false)), any_value()];
    let state = prop_oneof![
        Just(json!({})),
        Just(json!({"connected": false})),
        any_value()
    ];
    let result = prop_oneof![
        Just(json!("sent")),
        Just(json!("confirmed")),
        Just(json!("Sent")),
        any_value()
    ];
    let event = prop_oneof![Just(json!("state")), Just(json!("other")), any_value()];
    let members = (
        proptest::option::weighted(MEMBER_PRESENT, id),
        proptest::option::weighted(MEMBER_PRESENT, ok),
        proptest::option::weighted(MEMBER_PRESENT, state),
        proptest::option::weighted(MEMBER_PRESENT, result),
        proptest::option::weighted(MEMBER_PRESENT, any_error()),
        proptest::option::weighted(MEMBER_PRESENT, event),
    );
    members.prop_map(|(id, ok, state, result, error, event)| {
        let mut object = Map::new();
        for (name, value) in MEMBERS.iter().zip([id, ok, state, result, error, event]) {
            if let Some(value) = value {
                object.insert((*name).to_owned(), value);
            }
        }
        Value::Object(object)
    })
}

/// Lines around the bound with or without their newline, then a short line, so the bound and the next line both run.
fn long_stream() -> impl Strategy<Value = Vec<u8>> {
    let length = MAX_REPLY_BYTES - BOUND_MARGIN_BYTES..=MAX_REPLY_BYTES + BOUND_MARGIN_BYTES;
    (length, any::<bool>()).prop_map(|(length, ended)| {
        let mut stream = vec![b'x'; length];
        if ended {
            stream.extend_from_slice(b"\n{}\n");
        }
        stream
    })
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
fn a_line_past_the_bound_with_no_newline_is_not_understood() {
    let (mut reader, mut daemon) = pair();
    let writer = thread::spawn(move || {
        // The reader stops at the bound and drops its end, so a refused write is expected here.
        drop(daemon.write_all(&vec![b'x'; MAX_REPLY_BYTES + READ_PAST_BOUND]));
    });
    assert!(matches!(
        read_line(&mut reader, None),
        Err(Failure::NotUnderstood)
    ));
    drop(reader);
    writer.join().unwrap();
}

#[test]
fn a_line_one_byte_over_the_bound_is_refused_however_it_is_split() {
    let mut line = vec![b'x'; MAX_REPLY_BYTES + 1];
    line.push(b'\n');
    for cuts in cut_sets(MAX_REPLY_BYTES + 1) {
        let taken = take_chunks(&split_at(&line, &cuts));
        assert!(
            matches!(taken, Err(Failure::NotUnderstood)),
            "cuts {:?}",
            cuts.get(..4)
        );
    }
}

#[test]
fn a_line_of_exactly_the_bound_is_taken_however_it_is_split() {
    let mut line = vec![b'x'; MAX_REPLY_BYTES];
    line.push(b'\n');
    for cuts in cut_sets(MAX_REPLY_BYTES) {
        let taken = take_chunks(&split_at(&line, &cuts)).unwrap();
        assert_eq!(taken.len(), MAX_REPLY_BYTES, "cuts {:?}", cuts.get(..4));
    }
}

#[test]
fn two_lines_in_one_chunk_are_taken_one_after_the_other() {
    let chunk = b"one\ntwo\nth";
    let mut first = Vec::new();
    assert_eq!(take(&mut first, chunk).unwrap(), Step::Ended(4));
    assert_eq!(first, b"one");
    let mut second = Vec::new();
    assert_eq!(take(&mut second, &chunk[4..]).unwrap(), Step::Ended(4));
    assert_eq!(second, b"two");
    let mut third = Vec::new();
    assert_eq!(take(&mut third, &chunk[8..]).unwrap(), Step::Continues);
    assert_eq!(third, b"th");
}

#[test]
fn the_bound_holds_on_a_socket_read_one_byte_at_a_time() {
    for (length, taken) in [(MAX_REPLY_BYTES, true), (MAX_REPLY_BYTES + 1, false)] {
        let (ours, mut daemon) = UnixStream::pair().unwrap();
        let mut reader = BufReader::with_capacity(1, ours);
        let writer = thread::spawn(move || {
            // The reader stops at a refused line and drops its end, so a refused write is expected here.
            drop(daemon.write_all(&[vec![b'x'; length], b"\n{}\n".to_vec()].concat()));
        });
        let read = read_line(&mut reader, None);
        if taken {
            assert_eq!(read.unwrap().len(), length);
            assert_eq!(read_line(&mut reader, None).unwrap(), b"{}");
        } else {
            assert!(matches!(read, Err(Failure::NotUnderstood)), "{length}");
        }
        drop(reader);
        writer.join().unwrap();
    }
}

#[test]
fn status_takes_a_reply_of_exactly_the_bound_and_refuses_one_byte_more() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, daemon) = stub(dir.path(), padded_state_reply(MAX_REPLY_BYTES));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(status(&socket, &mut out, &mut err), ExitCode::SUCCESS);
    assert_eq!(err, b"");
    assert!(out.starts_with(b"{\"p\":\"ppp"));
    daemon.join().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (socket, daemon) = stub(dir.path(), padded_state_reply(MAX_REPLY_BYTES + 1));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(status(&socket, &mut out, &mut err), ExitCode::FAILURE);
    assert_eq!(out, b"");
    assert_eq!(err, b"The daemon's reply to status was not understood.\n");
    daemon.join().unwrap();
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
fn a_state_reply_is_taken_only_for_request_1_with_a_state_object() {
    let state = Outcome::Taken("{\"connected\":false}\n".to_owned());
    assert_eq!(
        outcome(state_reply(&STATE_REPLY[..STATE_REPLY.len() - 1])),
        state
    );
    for line in [
        br#"{"id":2,"ok":true,"state":{"connected":false}}"#.as_slice(),
        br#"{"ok":true,"state":{"connected":false}}"#,
        br#"{"id":1.0,"ok":true,"state":{"connected":false}}"#,
        br#"{"id":null,"ok":true,"state":{"connected":false}}"#,
        br#"{"id":1,"ok":true}"#,
        br#"{"id":1,"ok":true,"state":1}"#,
        br#"{"id":1,"ok":true,"result":"sent"}"#,
    ] {
        assert_eq!(
            outcome(state_reply(line)),
            Outcome::NotUnderstood,
            "{line:?}"
        );
    }
}

#[test]
fn a_result_reply_is_taken_only_for_request_1_with_a_documented_result() {
    for line in [
        br#"{"id":1,"ok":true,"result":"sent"}"#.as_slice(),
        br#"{"id":1,"ok":true,"result":"confirmed"}"#,
    ] {
        assert_eq!(
            result_outcome(line),
            Outcome::Taken(String::new()),
            "{line:?}"
        );
    }
    for line in [
        br#"{"ok":true}"#.as_slice(),
        br#"{"id":1,"ok":true}"#,
        br#"{"id":2,"ok":true,"result":"sent"}"#,
        br#"{"id":1,"ok":true,"result":"done"}"#,
        br#"{"id":1,"ok":true,"result":1}"#,
        br#"{"id":1,"ok":true,"state":{}}"#,
        br#"{"id":1,"ok":"true","result":"sent"}"#,
        b"not json",
        b"[true]",
    ] {
        assert_eq!(result_outcome(line), Outcome::NotUnderstood, "{line:?}");
    }
}

#[test]
fn a_refusal_is_taken_for_request_1_or_an_unread_id_with_a_message() {
    let refused = Outcome::Refused("No.".to_owned());
    for line in [
        br#"{"id":1,"ok":false,"error":{"code":"not_connected","message":"No."}}"#.as_slice(),
        br#"{"id":1,"ok":false,"error":{"message":"No."}}"#,
        br#"{"id":null,"ok":false,"error":{"code":"bad_request","message":"No."}}"#,
    ] {
        assert_eq!(result_outcome(line), refused, "{line:?}");
        assert_eq!(outcome(state_reply(line)), refused, "{line:?}");
    }
    for line in [
        br#"{"id":2,"ok":false,"error":{"code":"not_connected","message":"No."}}"#.as_slice(),
        br#"{"ok":false,"error":{"code":"bad_request","message":"No."}}"#,
        br#"{"id":null,"ok":false,"error":{"code":"not_connected","message":"No."}}"#,
        br#"{"id":1,"ok":false,"error":{"code":"not_connected"}}"#,
        br#"{"id":1,"ok":false,"error":{"message":7}}"#,
        br#"{"id":1,"ok":false,"error":"No."}"#,
        br#"{"id":1,"ok":false}"#,
    ] {
        assert_eq!(result_outcome(line), Outcome::NotUnderstood, "{line:?}");
    }
}

#[test]
fn only_a_state_event_gives_a_state_line() {
    let line = STATE_EVENT.strip_suffix(b"\n").unwrap();
    assert_eq!(event_state_line(line).unwrap(), "{\"connected\":true}\n");
    for line in [
        br#"{"event":"other","state":{}}"#.as_slice(),
        br#"{"event":"state","state":1}"#,
        br#"{"event":"state"}"#,
        br#"{"id":1,"ok":true,"state":{}}"#,
        b"[{\"event\":\"state\",\"state\":{}}]",
    ] {
        assert!(
            matches!(event_state_line(line), Err(Failure::NotUnderstood)),
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
fn watch_stops_at_an_event_it_does_not_understand() {
    let dir = tempfile::tempdir().unwrap();
    let lines = [
        STATE_REPLY,
        b"{\"event\":\"other\",\"state\":{}}\n",
        STATE_EVENT,
    ]
    .concat();
    let (socket, daemon) = stub(dir.path(), lines);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(watch(&socket, &mut out, &mut err), ExitCode::FAILURE);
    assert_eq!(out, b"{\"connected\":false}\n");
    assert_eq!(err, b"The daemon's reply to watch was not understood.\n");
    daemon.join().unwrap();
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
fn set_and_do_succeed_only_on_their_own_documented_reply() {
    let cases: [(&[u8], Outcome); 5] = [
        (
            b"{\"id\":1,\"ok\":true,\"result\":\"sent\"}\n",
            Outcome::Taken(String::new()),
        ),
        (b"{\"ok\":true}\n", Outcome::NotUnderstood),
        (
            b"{\"id\":2,\"ok\":true,\"result\":\"sent\"}\n",
            Outcome::NotUnderstood,
        ),
        (b"hello\n", Outcome::NotUnderstood),
        (
            b"{\"id\":1,\"ok\":false,\"error\":{\"code\":\"not_connected\",\"message\":\"No.\"}}\n",
            Outcome::Refused("No.".to_owned()),
        ),
    ];
    for (reply, wanted) in cases {
        for command in ["set", "do"] {
            let dir = tempfile::tempdir().unwrap();
            let (socket, daemon) = stub(dir.path(), reply.to_vec());
            let mut err = Vec::new();
            let (exit, sent) = if command == "set" {
                let exit = set("noise", "anc", &socket, &mut err);
                (exit, request("set", &[("key", "noise"), ("value", "anc")]))
            } else {
                let exit = act("connect", &socket, &mut err);
                (exit, request("do", &[("action", "connect")]))
            };
            let expected = match &wanted {
                Outcome::Taken(_) => (ExitCode::SUCCESS, String::new()),
                Outcome::Refused(message) => (ExitCode::FAILURE, format!("{message}\n")),
                Outcome::NotUnderstood => (
                    ExitCode::FAILURE,
                    format!("The daemon's reply to {command} was not understood.\n"),
                ),
            };
            assert_eq!(
                (exit, String::from_utf8(err).unwrap()),
                expected,
                "{command} {reply:?}"
            );
            assert_eq!(daemon.join().unwrap(), sent.into_bytes(), "{command}");
        }
    }
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
    fn any_reply_bytes_meet_the_reply_rule(line in proptest::collection::vec(any::<u8>(), 0..ANY_LINE_BYTES)) {
        let value = as_json(&line);
        let state = value.as_ref().map_or(Outcome::NotUnderstood, |value| expected_reply(value, state_success));
        let result = value.as_ref().map_or(Outcome::NotUnderstood, |value| expected_reply(value, result_success));
        prop_assert_eq!(outcome(state_reply(&line)), state);
        prop_assert_eq!(result_outcome(&line), result);
    }

    #[test]
    fn any_json_value_meets_the_reply_rule(value in any_value()) {
        let (line, value) = written(&value);
        prop_assert_eq!(outcome(state_reply(&line)), expected_reply(&value, state_success));
        prop_assert_eq!(result_outcome(&line), expected_reply(&value, result_success));
    }

    #[test]
    fn any_event_bytes_meet_the_event_rule(line in proptest::collection::vec(any::<u8>(), 0..ANY_LINE_BYTES)) {
        let expected = as_json(&line).map_or(Outcome::NotUnderstood, |value| expected_event(&value));
        prop_assert_eq!(outcome(event_state_line(&line)), expected);
    }

    #[test]
    fn any_json_value_meets_the_event_rule(value in any_value()) {
        let (line, value) = written(&value);
        prop_assert_eq!(outcome(event_state_line(&line)), expected_event(&value));
    }

    #[test]
    fn any_short_stream_split_anywhere_reads_by_the_line_rule(
        stream in proptest::collection::vec(prop_oneof![Just(b'\n'), any::<u8>()], 0..SHORT_STREAM_BYTES),
        capacity in 1..=SHORT_READ_BYTES,
    ) {
        check_read(stream, capacity);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(OBJECT_CASES))]
    #[test]
    fn any_reply_object_meets_the_reply_rule(value in any_line_object()) {
        let (line, value) = written(&value);
        prop_assert_eq!(outcome(state_reply(&line)), expected_reply(&value, state_success));
        prop_assert_eq!(result_outcome(&line), expected_reply(&value, result_success));
    }

    #[test]
    fn any_event_object_meets_the_event_rule(value in any_line_object()) {
        let (line, value) = written(&value);
        prop_assert_eq!(outcome(event_state_line(&line)), expected_event(&value));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(LONG_LINE_CASES))]
    #[test]
    fn any_long_line_split_anywhere_meets_the_bound(
        stream in long_stream(),
        capacity in 1..=MAX_REPLY_BYTES + BOUND_MARGIN_BYTES,
    ) {
        check_read(stream, capacity);
    }
}

#[test]
fn the_protocol_document_states_the_bound_and_the_sentence() {
    let document = include_str!("../../../../../docs/protocol-1.md");
    let bound = format!("at most {MAX_REPLY_BYTES} bytes, newline excluded");
    assert!(document.contains(&bound), "no {bound}");
    let mut err = Vec::new();
    let socket = Path::new("unused");
    let exit = report(Failure::NotUnderstood, "<command>", socket, &mut err);
    assert_eq!(exit, ExitCode::FAILURE);
    let sentence = format!("`{}`", String::from_utf8(err).unwrap().trim_end());
    assert!(document.contains(&sentence), "no {sentence}");
}
