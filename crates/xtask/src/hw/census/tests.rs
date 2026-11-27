//! Unit tests of the command census against a stub daemon socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use pods_core::command::{AdaptiveLevel, Command, NoiseTarget};
use pods_core::settings::setting;
use pods_core::state::State;
use pods_core::status::to_status_object;
use pods_proto::noise::NoiseMode;
use proptest::prelude::*;
use rustix::io::Errno;
use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};

use serde_json::{Map, Value, json};

use super::{
    CENSUS, CENSUS_TRIALS, MAX_REPLY_BYTES, NOISE_WORDS, NoWord, Rounds, SET_REPLY_TIMEOUT,
    WordError, census, missing_features, read_back, read_word, reply_word, set_over_socket,
    settings,
};
use crate::hw::report::Check;
use crate::hw::unit::pause;

/// In place of the product's 10 s.
const TEST_REPLY_TIMEOUT: Duration = Duration::from_millis(200);
/// So a census that never stops cannot keep the stub alive.
const SERVER_DEADLINE: Duration = Duration::from_secs(30);
const TIMEOUT_REPLY: &str = r#"{"id":1,"ok":false,"error":{"code":"timeout","message":"m"}}"#;
const CONFIRMED_REPLY: &str = r#"{"id":1,"ok":true,"result":"confirmed"}"#;
const SENT_REPLY: &str = r#"{"id":1,"ok":true,"result":"sent"}"#;

/// The AirPods state behind the status document the stub serves.
type Shown = Arc<Mutex<State>>;

/// The stub's AirPods before the census: noise off, both switches off, the level at 50.
fn original_status() -> Shown {
    let state = State {
        adaptive_noise_level: AdaptiveLevel::new(50).unwrap(),
        ..State::default()
    };
    Arc::new(Mutex::new(state))
}

/// Shows the request `line` in `shown`, as the daemon stores a written value; sample input `{"v":1,"id":1,"op":"set","key":"noise","value":"anc"}`.
fn show(shown: &Shown, line: &str) {
    let Ok(request) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let (Some(key), Some(value)) = (request["key"].as_str(), request["value"].as_str()) else {
        return;
    };
    let mut state = shown.lock().unwrap();
    match setting(key, value) {
        Ok(Command::SetNoise(NoiseTarget::Mode(mode))) => state.noise_mode = mode,
        Ok(Command::SetConversationalAwareness(on)) => state.conversational_awareness = on,
        Ok(Command::SetOneBudAnc(on)) => state.one_bud_anc_mode = on,
        Ok(Command::SetAdaptiveLevel(level)) => state.adaptive_noise_level = level,
        _ => {}
    }
}

/// The status document of `state`, as the daemon serves it.
fn status_of(state: &State) -> Map<String, Value> {
    to_status_object(state).as_object().unwrap().clone()
}

/// Gives the reply line to one request line, or `None` to never answer it.
type Answer = Box<dyn FnMut(&str) -> Option<&'static str> + Send>;

/// A stub daemon on `socket` that reads each request and gives it `answer`'s reply; it returns the requests it read.
fn serve(socket: &Path, mut answer: Answer, done: Arc<AtomicBool>) -> JoinHandle<Vec<String>> {
    let listener = UnixListener::bind(socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    thread::spawn(move || {
        let deadline = Instant::now().checked_add(SERVER_DEADLINE).unwrap();
        let mut open: Vec<UnixStream> = Vec::new();
        let mut requests = Vec::new();
        while !done.load(Ordering::SeqCst) && Instant::now() < deadline {
            let Ok((mut stream, _)) = listener.accept() else {
                pause(Duration::from_millis(5));
                continue;
            };
            stream.set_nonblocking(false).unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            match answer(&line) {
                Some(reply) => stream.write_all(format!("{reply}\n").as_bytes()).unwrap(),
                None => open.push(stream),
            }
            requests.push(line);
        }
        requests
    })
}

/// Whether the stub's status shows one request line.
type Follows = Box<dyn FnMut(&str) -> bool + Send>;

/// Reads the stub's status as the census reads the daemon's.
type Read = fn(&Shown) -> Result<Map<String, Value>, String>;

fn read_shown(shown: &Shown) -> Result<Map<String, Value>, String> {
    let state = shown.lock().map_err(|error| error.to_string())?;
    Ok(status_of(&state))
}

fn run_census(reply: Option<&'static str>) -> (Rounds, Check, Vec<String>, Duration) {
    run_census_from(Box::new(move |_| reply), Box::new(|_| true), read_shown)
}

/// The census against a stub answering with `answer`, whose status shows each request `follows` takes, read by `read`.
fn run_census_from(
    answer: Answer,
    mut follows: Follows,
    read: Read,
) -> (Rounds, Check, Vec<String>, Duration) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("pods.sock");
    let done = Arc::new(AtomicBool::new(false));
    let shown = original_status();
    let mut answer = answer;
    let stub_shown = Arc::clone(&shown);
    let answer: Answer = Box::new(move |line| {
        if follows(line) {
            show(&stub_shown, line);
        }
        answer(line)
    });
    let server = serve(&socket, answer, Arc::clone(&done));
    let mut check = Check::new("C6 commands");
    let began = Instant::now();
    let status = || read(&shown);
    let rounds = census(&socket, &mut check, TEST_REPLY_TIMEOUT, &status).unwrap();
    let took = began.elapsed();
    done.store(true, Ordering::SeqCst);
    (rounds, check, server.join().unwrap(), took)
}

#[test]
fn the_first_request_with_no_reply_ends_the_census_and_is_named() {
    let (rounds, check, requests, took) = run_census(None);
    assert_eq!(rounds, Rounds::Unanswered);
    assert_eq!(requests.len(), 1);
    assert!(took >= TEST_REPLY_TIMEOUT, "{took:?}");
    assert!(
        took < TEST_REPLY_TIMEOUT.checked_mul(5).unwrap(),
        "{took:?}"
    );
    assert!(check.failed());
    assert!(
        check
            .lines
            .iter()
            .any(|line| line.contains("set noise transparency: no reply within 200 ms")),
        "{:?}",
        check.lines
    );
}

#[test]
fn a_timeout_reply_is_an_answer_and_the_census_goes_on() {
    let (rounds, check, requests, _) = run_census(Some(TIMEOUT_REPLY));
    assert_eq!(rounds, Rounds::Answered);
    let adaptive_setups = CENSUS.iter().filter(|command| command.in_adaptive).count();
    assert_eq!(
        requests.len(),
        CENSUS_TRIALS * (CENSUS.len() + adaptive_setups)
    );
    assert!(check.failed());
}

#[test]
fn every_command_answered_sent_and_shown_passes() {
    let (rounds, check, _, _) = run_census(Some(SENT_REPLY));
    assert_eq!(rounds, Rounds::Answered);
    assert!(!check.failed(), "{:?}", check.lines);
    assert!(
        check
            .lines
            .iter()
            .any(|line| line
                .starts_with("set adaptive_level 40: sent 10 of 10, shown at once 10 of 10")),
        "{:?}",
        check.lines
    );
}

#[test]
fn a_command_answered_with_any_other_word_fails() {
    let (rounds, check, _, _) = run_census(Some(CONFIRMED_REPLY));
    assert_eq!(rounds, Rounds::Answered);
    assert!(check.failed());
    let failures: Vec<&String> = check
        .lines
        .iter()
        .filter(|line| line.contains("was not"))
        .collect();
    assert_eq!(failures.len(), CENSUS.len() + 1, "{failures:?}");
    assert!(
        failures.contains(
            &&"set noise adaptive was not sent before adaptive_level in 20 of 20".to_owned()
        ),
        "{failures:?}"
    );
}

#[test]
fn a_status_that_keeps_the_old_values_fails_every_row_asking_another() {
    let (rounds, check, _, _) = run_census_from(
        Box::new(|_| Some(SENT_REPLY)),
        Box::new(|_| false),
        read_shown,
    );
    assert_eq!(rounds, Rounds::Answered);
    assert!(check.failed());
    let unshown: Vec<&String> = check
        .lines
        .iter()
        .filter(|line| line.contains("was not shown at once"))
        .collect();
    assert_eq!(
        unshown,
        [
            "set noise transparency was not shown at once 10 of 10",
            "set noise anc was not shown at once 10 of 10",
            "set conversational_awareness true was not shown at once 10 of 10",
            "set one_bud_anc true was not shown at once 10 of 10",
            "set adaptive_level 40 was not shown at once 10 of 10",
            "set adaptive_level 60 was not shown at once 10 of 10",
        ]
    );
}

#[test]
fn a_level_the_status_shows_only_once_fails_though_it_holds_a_value_asked_for() {
    let mut level_shown = false;
    let follows: Follows = Box::new(move |line| {
        if !line.contains("adaptive_level") {
            return true;
        }
        !std::mem::replace(&mut level_shown, true)
    });
    let (_, check, _, _) = run_census_from(Box::new(|_| Some(SENT_REPLY)), follows, read_shown);
    assert!(check.failed());
    assert!(
        check
            .lines
            .iter()
            .any(|line| line == "set adaptive_level 60 was not shown at once 10 of 10"),
        "{:?}",
        check.lines
    );
}

#[test]
fn a_status_that_cannot_be_read_fails_naming_why() {
    let (_, check, _, _) =
        run_census_from(Box::new(|_| Some(SENT_REPLY)), Box::new(|_| true), |_| {
            Err("pods status exited with exit status: 1".to_owned())
        });
    assert!(check.failed());
    assert!(
        check.lines.iter().any(|line| line
            == "the status could not be read after a set: pods status exited with exit status: 1"),
        "{:?}",
        check.lines
    );
}

#[test]
fn a_setting_that_reads_back_changed_is_named() {
    let state = original_status().lock().unwrap().clone();
    let original = status_of(&state);
    let originals = settings(&original).unwrap();
    assert_eq!(read_back(&|| Ok(original.clone()), &originals), Ok(()));
    let changed = status_of(&State {
        one_bud_anc_mode: true,
        ..state
    });
    assert_eq!(
        read_back(&|| Ok(changed.clone()), &originals),
        Err("one_bud_anc read back changed".to_owned())
    );
}

#[test]
fn each_noise_word_is_the_daemon_word_for_its_number() {
    for (number, word) in (0_u8..).zip(NOISE_WORDS) {
        let mode = NoiseMode::try_from(number).unwrap();
        assert_eq!(
            setting("noise", word),
            Ok(Command::SetNoise(NoiseTarget::Mode(mode))),
            "{number} {word}"
        );
    }
}

#[test]
fn a_feature_that_is_false_or_absent_is_missing() {
    let state = json!({
        "supports_noise_control": true,
        "supports_adaptive": false,
        "supports_conversational_awareness": true,
    });
    let state = state.as_object().unwrap();
    assert_eq!(
        missing_features(state),
        ["supports_adaptive", "supports_one_bud_anc"]
    );
}

#[test]
fn a_reply_gives_its_result_or_its_error_code() {
    assert_eq!(
        reply_word(r#"{"id":1,"ok":true,"result":"confirmed"}"#).as_deref(),
        Some("confirmed")
    );
    assert_eq!(
        reply_word(r#"{"id":1,"ok":false,"error":{"code":"not_worn","message":"m"}}"#).as_deref(),
        Some("not_worn")
    );
    assert_eq!(reply_word(r#"{"id":2,"ok":true,"result":"sent"}"#), None);
}

/// More pending connections than a listen backlog of 0 admits, so the next connect finds it full.
const BACKLOG_FILL: usize = 16;
/// How long a bounded exchange may take: the reply bound and a second for scheduling.
const BOUNDED: Duration = TEST_REPLY_TIMEOUT.saturating_add(Duration::from_secs(1));

#[test]
fn a_full_listen_backlog_is_unanswered_within_the_bound() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pods.sock");
    let listener = rustix::net::socket(AddressFamily::UNIX, SocketType::STREAM, None).unwrap();
    let address = SocketAddrUnix::new(&path).unwrap();
    rustix::net::bind(&listener, &address).unwrap();
    rustix::net::listen(&listener, 0).unwrap();
    let flags = SocketFlags::CLOEXEC | SocketFlags::NONBLOCK;
    let mut pending = Vec::new();
    let mut full = false;
    for _ in 0..BACKLOG_FILL {
        let fd =
            rustix::net::socket_with(AddressFamily::UNIX, SocketType::STREAM, flags, None).unwrap();
        match rustix::net::connect(&fd, &address) {
            Ok(()) => pending.push(fd),
            Err(Errno::AGAIN) => {
                full = true;
                break;
            }
            Err(error) => panic!("{error}"),
        }
    }
    assert!(full, "the backlog never filled");
    let began = Instant::now();
    let word = set_over_socket(&path, "noise", "anc", TEST_REPLY_TIMEOUT);
    let took = began.elapsed();
    assert_eq!(
        word,
        Err(NoWord::Unanswered("no reply within 200 ms".to_owned()))
    );
    assert!(took >= TEST_REPLY_TIMEOUT, "{took:?}");
    assert!(took < BOUNDED, "{took:?}");
}

/// The word `read_word` gives for `bytes` written by a peer that then closes, or why it gave none.
fn word_of(bytes: Vec<u8>) -> String {
    let (mut ours, mut theirs) = UnixStream::pair().unwrap();
    let writer = thread::spawn(move || {
        drop(theirs.write_all(&bytes));
    });
    let deadline = Instant::now().checked_add(TEST_REPLY_TIMEOUT);
    let word = read_word(&mut ours, deadline);
    drop(ours);
    writer.join().unwrap();
    match word {
        Ok(word) => word,
        Err(WordError::Io(error)) => error.to_string(),
        Err(WordError::Word(NoWord::Unanswered(reason))) => reason,
        Err(WordError::Word(NoWord::Unreadable)) => "unreadable".to_owned(),
    }
}

#[test]
fn a_reply_that_is_not_utf8_or_not_a_reply_is_unreadable() {
    assert_eq!(word_of(b"\xff\xfe\n".to_vec()), "unreadable");
    assert_eq!(word_of(b"{not json\n".to_vec()), "unreadable");
    assert_eq!(word_of(b"\n".to_vec()), "unreadable");
    assert_eq!(word_of(Vec::new()), "the daemon closed the socket");
    assert_eq!(word_of(CONFIRMED_REPLY.as_bytes().to_vec()), "confirmed");
}

#[test]
fn a_reply_is_read_up_to_the_size_limit_and_cut_there_as_the_client_cuts_it() {
    let padded = |length: usize, tail: &[u8]| {
        let mut line = CONFIRMED_REPLY.as_bytes().to_vec();
        line.resize(length, b' ');
        line.extend_from_slice(tail);
        line
    };
    assert_eq!(word_of(padded(MAX_REPLY_BYTES, b"\n")), "confirmed");
    assert_eq!(word_of(padded(MAX_REPLY_BYTES, b"x\n")), "confirmed");
    let mut cut_inside = vec![b' '; MAX_REPLY_BYTES - 5];
    cut_inside.extend_from_slice(CONFIRMED_REPLY.as_bytes());
    cut_inside.push(b'\n');
    assert_eq!(word_of(cut_inside), "unreadable");
}

proptest! {
    #[test]
    fn any_reply_bytes_end_within_the_bound(
        bytes in proptest::collection::vec(any::<u8>(), 0..=MAX_REPLY_BYTES + 64)
    ) {
        let began = Instant::now();
        let word = word_of(bytes);
        prop_assert!(began.elapsed() < BOUNDED);
        prop_assert!(word.len() <= MAX_REPLY_BYTES);
    }
}

#[test]
fn wait_ready_on_keeps_one_deadline_across_an_interrupted_poll() {
    use super::wait_ready_on;
    use crate::hw::clock::{Clock, PausedClock};

    let clock = PausedClock::default();
    let deadline = clock.deadline(SET_REPLY_TIMEOUT);
    clock.pause(Duration::from_secs(2));
    let mut budgets = Vec::new();
    let error = wait_ready_on(&clock, deadline, |left| {
        budgets.push(left);
        if budgets.len() == 1 {
            clock.pause(Duration::from_secs(1));
            return Err(Errno::INTR);
        }
        clock.pause(left);
        Ok(0)
    })
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(budgets, [Duration::from_secs(8), Duration::from_secs(7)]);
    assert_eq!(clock.elapsed(), Duration::from_secs(10));
}
