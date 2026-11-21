//! Unit tests of the command census against a stub daemon socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use pods_core::command::{Command, NoiseTarget};
use pods_core::settings::setting;
use pods_proto::noise::NoiseMode;
use proptest::prelude::*;
use rustix::io::Errno;
use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};

use serde_json::json;

use super::{
    CENSUS, CENSUS_TRIALS, MAX_REPLY_BYTES, NOISE_WORDS, NoWord, Originals, Rounds, WordError,
    census, missing_features, read_word, reply_word, set_over_socket,
};
use crate::hw::report::Check;
use crate::hw::unit::pause;

/// The reply bound the census gets in these tests, in place of the product's 10 s.
const TEST_REPLY_TIMEOUT: Duration = Duration::from_millis(200);
/// The longest a stub daemon serves, so a census that never stops cannot keep it alive.
const SERVER_DEADLINE: Duration = Duration::from_secs(30);
const TIMEOUT_REPLY: &str = r#"{"id":1,"ok":false,"error":{"code":"timeout","message":"m"}}"#;
const CONFIRMED_REPLY: &str = r#"{"id":1,"ok":true,"result":"confirmed"}"#;
const SENT_REPLY: &str = r#"{"id":1,"ok":true,"result":"sent"}"#;
/// The adaptive level the stub's AirPods start at, one of the two the census alternates.
const ORIGINAL_LEVEL: u64 = 40;

/// Settings none of whose first rows the census asks for, with the adaptive level at `level`.
fn originals_at(level: u64) -> Originals {
    Originals {
        noise: "off",
        conversational_awareness: false,
        one_bud_anc: false,
        adaptive_level: level,
    }
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

fn run_census(reply: Option<&'static str>) -> (Rounds, Check, Vec<String>, Duration) {
    run_census_from(Box::new(move |_| reply), &originals_at(ORIGINAL_LEVEL))
}

fn run_census_from(
    answer: Answer,
    originals: &Originals,
) -> (Rounds, Check, Vec<String>, Duration) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("librepods.sock");
    let done = Arc::new(AtomicBool::new(false));
    let server = serve(&socket, answer, Arc::clone(&done));
    let mut check = Check::new("C6 commands");
    let began = Instant::now();
    let rounds = census(&socket, &mut check, TEST_REPLY_TIMEOUT, originals).unwrap();
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
    assert!(!check.passed);
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
    assert!(!check.passed);
}

#[test]
fn a_command_expected_to_answer_sent_fails_on_any_other_word() {
    let (rounds, check, _, _) = run_census(Some(CONFIRMED_REPLY));
    assert_eq!(rounds, Rounds::Answered);
    assert!(!check.passed);
    let failures: Vec<&String> = check
        .lines
        .iter()
        .filter(|line| line.contains("was not"))
        .collect();
    assert_eq!(
        failures,
        ["set adaptive_level 40 and 60 in turn was not sent 10 of 10"]
    );
}

/// Answers `sent` to every adaptive level and `confirmed` to the rest; the bench's daemon stand-in confirms an unchanged level end to end.
fn level_sent() -> Answer {
    Box::new(|line| {
        Some(if line.contains("adaptive_level") {
            SENT_REPLY
        } else {
            CONFIRMED_REPLY
        })
    })
}

#[test]
fn every_adaptive_level_trial_is_a_write_and_only_sent_counts() {
    let (rounds, check, requests, _) = run_census_from(level_sent(), &originals_at(ORIGINAL_LEVEL));
    assert_eq!(rounds, Rounds::Answered);
    assert!(check.passed, "{:?}", check.lines);
    assert!(
        check.lines.iter().any(|line| line
            .starts_with("set adaptive_level 40 and 60 in turn: sent 10 of 10, only sent counts")),
        "{:?}",
        check.lines
    );
    assert_eq!(levels_sent(&requests), alternating("60", "40"));
}

/// The adaptive levels the census asked for, in order.
fn levels_sent(requests: &[String]) -> Vec<String> {
    requests
        .iter()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|request| request["key"] == "adaptive_level")
        .filter_map(|request| request["value"].as_str().map(str::to_owned))
        .collect()
}

/// `CENSUS_TRIALS` values, `first` then `second` in turn.
fn alternating(first: &str, second: &str) -> Vec<String> {
    (0..CENSUS_TRIALS)
        .map(|trial| if trial % 2 == 0 { first } else { second }.to_owned())
        .collect()
}

#[test]
fn an_original_level_outside_the_two_starts_on_the_first() {
    let (_, check, requests, _) = run_census_from(level_sent(), &originals_at(50));
    assert!(check.passed, "{:?}", check.lines);
    assert_eq!(levels_sent(&requests), alternating("40", "60"));
}

#[test]
fn no_request_asks_for_the_value_already_set() {
    let originals = Originals {
        noise: "transparency",
        conversational_awareness: true,
        one_bud_anc: true,
        adaptive_level: ORIGINAL_LEVEL,
    };
    let (rounds, check, requests, _) = run_census_from(level_sent(), &originals);
    assert_eq!(rounds, Rounds::Answered);
    assert!(check.passed, "{:?}", check.lines);
    let mut current = std::collections::HashMap::from([
        ("noise".to_owned(), "transparency".to_owned()),
        ("conversational_awareness".to_owned(), "true".to_owned()),
        ("one_bud_anc".to_owned(), "true".to_owned()),
        ("adaptive_level".to_owned(), ORIGINAL_LEVEL.to_string()),
    ]);
    for (index, line) in requests.iter().enumerate() {
        let request: serde_json::Value = serde_json::from_str(line).unwrap();
        let key = request["key"].as_str().unwrap().to_owned();
        let value = request["value"].as_str().unwrap().to_owned();
        let before = current.insert(key.clone(), value.clone());
        assert_ne!(
            before.as_ref(),
            Some(&value),
            "request {index}: set {key} {value}"
        );
    }
}

/// Settings whose noise is at the census's first noise row, so C6 sends `noise anc` before it.
fn originals_at_transparency() -> Originals {
    Originals {
        noise: "transparency",
        ..originals_at(ORIGINAL_LEVEL)
    }
}

#[test]
fn an_unconfirmed_lead_in_fails_c6() {
    let mut first = true;
    let answer: Answer = Box::new(move |line| {
        Some(
            if std::mem::take(&mut first) || line.contains("adaptive_level") {
                SENT_REPLY
            } else {
                CONFIRMED_REPLY
            },
        )
    });
    let (rounds, check, requests, _) = run_census_from(answer, &originals_at_transparency());
    assert_eq!(rounds, Rounds::Answered);
    assert!(
        requests
            .first()
            .is_some_and(|line| line.contains(r#""value":"anc""#)),
        "{requests:?}"
    );
    assert!(!check.passed);
    let failures: Vec<&String> = check
        .lines
        .iter()
        .filter(|line| line.contains("before the first trial"))
        .collect();
    assert_eq!(
        failures,
        ["set noise anc before the first trial was not confirmed"]
    );
}

#[test]
fn an_unanswered_lead_in_ends_the_census() {
    let (rounds, check, requests, _) =
        run_census_from(Box::new(|_| None), &originals_at_transparency());
    assert_eq!(rounds, Rounds::Unanswered);
    assert_eq!(requests.len(), 1);
    assert!(!check.passed);
    assert!(
        check
            .lines
            .iter()
            .any(|line| line == "set noise anc: no reply within 200 ms; the census stopped there"),
        "{:?}",
        check.lines
    );
}

#[test]
fn an_unconfirmed_adaptive_setup_is_named() {
    let (_, check, _, _) = run_census(Some(SENT_REPLY));
    assert!(
        check
            .lines
            .iter()
            .any(|line| line
                == "set noise adaptive was not confirmed before adaptive_level in 10 of 10"),
        "{:?}",
        check.lines
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
    let path = dir.path().join("librepods.sock");
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
