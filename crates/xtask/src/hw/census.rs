//! C6, the command census: each setting command ten times over protocol 1, the originals put back, then one with no bud in.

use std::io::{self, BufRead, ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};
use serde_json::{Map, Value, json};

use super::lab::{Lab, millis};
use super::report::{Check, Gate};
use super::unit::pause;
use super::{Interrupted, stopped};

/// The reply word of a command that took effect and shows in the state.
const CONFIRMED: &str = "confirmed";
/// The reply word of a command that went to the `AirPods` with no answer yet.
const SENT: &str = "sent";
/// The error code of a command that needs a bud in an ear.
const NOT_WORN: &str = "not_worn";
/// How long the person has to answer a prompt.
const PROMPT_WAIT: Duration = Duration::from_secs(300);
/// How long a `set` over the socket may wait for its reply, longer than the daemon's own confirm timeout.
const SET_REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest reply line read from the daemon, as the command line client bounds it.
const MAX_REPLY_BYTES: usize = 65536;
/// The size of each read of a reply.
const READ_CHUNK_BYTES: usize = 4096;
/// How long a connect waits before it tries a full listen backlog again.
const CONNECT_RETRY: Duration = Duration::from_millis(10);
/// Trials of each census command in C6.
const CENSUS_TRIALS: usize = 10;
/// How often the prompt wait wakes to notice a stop signal, in nanoseconds.
const PROMPT_POLL_NANOS: i64 = 250_000_000;

/// One census command, the word a working link answers it with, and whether it needs adaptive mode first.
struct CensusCommand {
    key: &'static str,
    value: &'static str,
    /// A second value sent on every other trial, for a setting the daemon confirms without a write when it is unchanged.
    alternate: Option<&'static str>,
    expected: &'static str,
    in_adaptive: bool,
}

/// The census of commands C6 sends; a hardware run may correct the expected words.
const CENSUS: [CensusCommand; 7] = [
    CensusCommand {
        key: "noise",
        value: "transparency",
        alternate: None,
        expected: CONFIRMED,
        in_adaptive: false,
    },
    CensusCommand {
        key: "noise",
        value: "anc",
        alternate: None,
        expected: CONFIRMED,
        in_adaptive: false,
    },
    CensusCommand {
        key: "conversational_awareness",
        value: "true",
        alternate: None,
        expected: CONFIRMED,
        in_adaptive: false,
    },
    CensusCommand {
        key: "conversational_awareness",
        value: "false",
        alternate: None,
        expected: CONFIRMED,
        in_adaptive: false,
    },
    CensusCommand {
        key: "one_bud_anc",
        value: "true",
        alternate: None,
        expected: CONFIRMED,
        in_adaptive: false,
    },
    CensusCommand {
        key: "one_bud_anc",
        value: "false",
        alternate: None,
        expected: CONFIRMED,
        in_adaptive: false,
    },
    CensusCommand {
        key: "adaptive_level",
        value: "40",
        alternate: Some("60"),
        expected: SENT,
        in_adaptive: true,
    },
];

/// The status document's capability keys the census needs; a model missing one would answer `not_supported` to its rows.
const NEEDED_FEATURES: [&str; 4] = [
    "supports_noise_control",
    "supports_adaptive",
    "supports_conversational_awareness",
    "supports_one_bud_anc",
];

/// The `set` values of the status document's `noise_mode` numbers, in number order.
const NOISE_WORDS: [&str; 4] = ["off", "anc", "transparency", "adaptive"];

/// Why a `set` gave no reply word.
#[derive(Debug, PartialEq)]
enum NoWord {
    /// Nothing came back within the bound, or the socket failed; the text says which.
    Unanswered(String),
    /// A line came back that is not a protocol 1 reply to the request.
    Unreadable,
}

/// Sends one `set` over protocol 1 and returns the reply word: the `result`, or the error `code`; one deadline covers connect, write and reply.
fn set_over_socket(
    socket: &Path,
    key: &str,
    value: &str,
    reply_timeout: Duration,
) -> Result<String, NoWord> {
    let deadline = Instant::now().checked_add(reply_timeout);
    let unanswered = |error: io::Error| {
        NoWord::Unanswered(if error.kind() == ErrorKind::TimedOut {
            format!("no reply within {} ms", millis(reply_timeout))
        } else {
            error.to_string()
        })
    };
    let mut stream = connect_by(socket, deadline).map_err(unanswered)?;
    let request = json!({"v": 1, "id": 1, "op": "set", "key": key, "value": value});
    write_by(&mut stream, format!("{request}\n").as_bytes(), deadline).map_err(unanswered)?;
    read_word(&mut stream, deadline).map_err(|error| match error {
        WordError::Io(error) => unanswered(error),
        WordError::Word(word) => word,
    })
}

/// Why `read_word` gave no word: the read failed, or what came back gave none.
enum WordError {
    Io(io::Error),
    Word(NoWord),
}

/// Reads the reply line from `stream` before `deadline` and returns its word; sample input `{"id":1,"ok":false,"error":{"code":"not_worn","message":"m"}}`.
fn read_word(stream: &mut UnixStream, deadline: Option<Instant>) -> Result<String, WordError> {
    let Some(reply) = read_reply(stream, deadline).map_err(WordError::Io)? else {
        return Err(WordError::Word(NoWord::Unanswered(
            "the daemon closed the socket".to_owned(),
        )));
    };
    std::str::from_utf8(&reply)
        .ok()
        .and_then(reply_word)
        .ok_or(WordError::Word(NoWord::Unreadable))
}

/// A non-blocking connect retried until `deadline`, since a full listen backlog answers `EAGAIN` rather than waiting.
fn connect_by(socket: &Path, deadline: Option<Instant>) -> io::Result<UnixStream> {
    let flags = SocketFlags::CLOEXEC | SocketFlags::NONBLOCK;
    let fd = rustix::net::socket_with(AddressFamily::UNIX, SocketType::STREAM, flags, None)?;
    let address = SocketAddrUnix::new(socket)?;
    loop {
        match rustix::net::connect(&fd, &address) {
            Ok(()) => return Ok(UnixStream::from(fd)),
            Err(Errno::AGAIN | Errno::INTR) => {
                if left(deadline).is_zero() {
                    return Err(ErrorKind::TimedOut.into());
                }
                pause(CONNECT_RETRY.min(left(deadline)));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Writes all of `bytes` to the non-blocking `stream` before `deadline`.
fn write_by(
    stream: &mut UnixStream,
    mut bytes: &[u8],
    deadline: Option<Instant>,
) -> io::Result<()> {
    while !bytes.is_empty() {
        wait_ready(stream, PollFlags::OUT, deadline)?;
        match stream.write(bytes) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(written) => bytes = bytes.get(written..).unwrap_or_default(),
            Err(error)
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Reads one reply line from `stream` before `deadline`, without its newline and cut at `MAX_REPLY_BYTES`; `None` when the daemon closed before a byte; sample input `{"id":1,"ok":true,"result":"sent"}` and a newline.
fn read_reply(stream: &mut UnixStream, deadline: Option<Instant>) -> io::Result<Option<Vec<u8>>> {
    let mut reply = Vec::new();
    let mut chunk = [0_u8; READ_CHUNK_BYTES];
    loop {
        if let Some(end) = reply.iter().position(|byte| *byte == b'\n') {
            return Ok(reply.get(..end).map(<[u8]>::to_vec));
        }
        let room = MAX_REPLY_BYTES.saturating_sub(reply.len());
        // corner: a line past the bound is cut there, as the command line client cuts it, and parses only if its first bytes are a whole reply.
        if room == 0 {
            return Ok(Some(reply));
        }
        wait_ready(stream, PollFlags::IN, deadline)?;
        let buffer = chunk
            .get_mut(..room.min(READ_CHUNK_BYTES))
            .unwrap_or_default();
        match stream.read(buffer) {
            Ok(0) if reply.is_empty() => return Ok(None),
            // corner: a last line with no newline before the close is still the reply.
            Ok(0) => return Ok(Some(reply)),
            Ok(read) => reply.extend_from_slice(buffer.get(..read).unwrap_or_default()),
            Err(error)
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
            Err(error) => return Err(error),
        }
    }
}

/// The time left before `deadline`; none is left when the deadline could not be computed.
fn left(deadline: Option<Instant>) -> Duration {
    deadline.map_or(Duration::ZERO, |deadline| {
        deadline.saturating_duration_since(Instant::now())
    })
}

/// Waits until `stream` is ready for `flags`, or fails with `TimedOut` at `deadline`.
fn wait_ready(stream: &UnixStream, flags: PollFlags, deadline: Option<Instant>) -> io::Result<()> {
    loop {
        let left = left(deadline);
        if left.is_zero() {
            return Err(ErrorKind::TimedOut.into());
        }
        let timeout = Timespec {
            tv_sec: i64::try_from(left.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(left.subsec_nanos()),
        };
        let mut fds = [PollFd::new(stream, flags)];
        match poll(&mut fds, Some(&timeout)) {
            Ok(0) | Err(Errno::INTR) => {}
            Ok(_) => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
}

/// The word of a protocol 1 reply to request 1; sample input `{"id":1,"ok":true,"result":"confirmed"}` or `{"id":1,"ok":false,"error":{"code":"not_worn","message":"..."}}`.
fn reply_word(line: &str) -> Option<String> {
    let reply: Value = serde_json::from_str(line.trim()).ok()?;
    if reply.get("id")?.as_u64()? != 1 {
        return None;
    }
    let word = if reply.get("ok")?.as_bool()? {
        reply.get("result")?
    } else {
        reply.get("error")?.get("code")?
    };
    word.as_str().map(str::to_owned)
}

/// The settings C6 changes, as the status document held them before it began.
struct Originals {
    noise: &'static str,
    conversational_awareness: bool,
    one_bud_anc: bool,
    adaptive_level: u64,
}

impl Originals {
    /// The `set` value of `key` as it was before C6 began.
    fn value_of(&self, key: &str) -> Option<String> {
        match key {
            "noise" => Some(self.noise.to_owned()),
            "conversational_awareness" => Some(self.conversational_awareness.to_string()),
            "one_bud_anc" => Some(self.one_bud_anc.to_string()),
            "adaptive_level" => Some(self.adaptive_level.to_string()),
            _ => None,
        }
    }
}

/// The value sent once before row `index`'s first trial when it is its key's first row and asks for the value already set: the key's other row value, so that trial is a write too.
fn lead_in(index: usize, originals: &Originals) -> Option<&'static str> {
    let command = CENSUS.get(index)?;
    let first_of_key = CENSUS.iter().position(|row| row.key == command.key) == Some(index);
    if !first_of_key || originals.value_of(command.key).as_deref() != Some(command.value) {
        return None;
    }
    CENSUS
        .iter()
        .find(|row| row.key == command.key && row.value != command.value)
        .map(|row| row.value)
}

/// The capability keys in `NEEDED_FEATURES` not true in the status line; sample input `{"supports_adaptive":true,"supports_one_bud_anc":false}` among its other keys.
fn missing_features(state: &Map<String, Value>) -> Vec<&'static str> {
    NEEDED_FEATURES
        .into_iter()
        .filter(|key| state.get(*key).and_then(Value::as_bool) != Some(true))
        .collect()
}

/// The settings in the status line; sample input `{"adaptive_noise_level":50,"conversational_awareness":false,"noise_mode":0,"one_bud_anc_mode":false}` among its other keys.
fn originals(state: &Map<String, Value>) -> Result<Originals, String> {
    let missing = |key: &str| format!("the state has no usable {key}");
    let noise = state
        .get("noise_mode")
        .and_then(Value::as_u64)
        .and_then(|mode| usize::try_from(mode).ok())
        .and_then(|mode| NOISE_WORDS.get(mode).copied())
        .ok_or_else(|| missing("noise_mode"))?;
    let flag = |key: &str| {
        state
            .get(key)
            .and_then(Value::as_bool)
            .ok_or_else(|| missing(key))
    };
    Ok(Originals {
        noise,
        conversational_awareness: flag("conversational_awareness")?,
        one_bud_anc: flag("one_bud_anc_mode")?,
        adaptive_level: state
            .get("adaptive_noise_level")
            .and_then(Value::as_u64)
            .ok_or_else(|| missing("adaptive_noise_level"))?,
    })
}

/// C6: each census command ten times, the originals put back on every way out, then `not_worn` with both buds out.
pub(crate) fn commands(lab: &Lab<'_>) -> Result<Check, Interrupted> {
    let mut check = Check::new("C6 commands");
    let state = match lab.status() {
        Ok(state) => state,
        Err(error) => {
            check.fail(error);
            return Ok(check);
        }
    };
    let missing = missing_features(&state);
    if !missing.is_empty() {
        check.fail(format!(
            "the AirPods lack {}, so nothing was set",
            missing.join(", ")
        ));
        return Ok(check);
    }
    let originals = match originals(&state) {
        Ok(originals) => originals,
        Err(error) => {
            check.fail(error);
            return Ok(check);
        }
    };
    let rounds = census(&lab.socket(), &mut check, SET_REPLY_TIMEOUT, &originals);
    let restored = restore_settings(lab, &originals);
    check.note(format!("settings put back: {}", restored.is_ok()));
    if let Err(error) = restored {
        // A stop signal below drops this check, so a setting left changed is reported now.
        say(&format!("C6: {error}"));
        check.fail(error);
    }
    if rounds? == Rounds::Unanswered {
        return Ok(check);
    }
    say("C6: take both buds out of your ears, then press Enter.");
    if !answered()? {
        check.fail("no answer to the prompt");
        return Ok(check);
    }
    let probe = if originals.noise == "anc" {
        "transparency"
    } else {
        "anc"
    };
    let word = match set_over_socket(&lab.socket(), "noise", probe, SET_REPLY_TIMEOUT) {
        Ok(word) => {
            check.note(format!(
                "with no bud in, set noise {probe} answered {}",
                printable(&word)
            ));
            word
        }
        Err(NoWord::Unanswered(reason)) => {
            check.note(format!("with no bud in, set noise {probe}: {reason}"));
            String::new()
        }
        Err(NoWord::Unreadable) => {
            check.note(format!(
                "with no bud in, set noise {probe} gave an unreadable reply"
            ));
            String::new()
        }
    };
    if word != NOT_WORN {
        check.fail(format!("expected {NOT_WORN} with no bud in"));
        let put_back = set_over_socket(&lab.socket(), "noise", originals.noise, SET_REPLY_TIMEOUT);
        let put_back = put_back.is_ok_and(|word| took_effect(&word));
        check.note(format!("noise put back: {put_back}"));
        if !put_back {
            check.fail("could not put back noise");
        }
    }
    Ok(check)
}

/// How the census rounds ended.
#[derive(Debug, PartialEq)]
enum Rounds {
    Answered,
    /// A request got no reply at all, so the rest, which would wait as long, were not sent.
    Unanswered,
}

/// Sends one census request; `None` when it went unanswered, which `check` then names.
fn census_set(
    socket: &Path,
    (key, value): (&str, &str),
    reply_timeout: Duration,
    check: &mut Check,
) -> Option<Result<String, NoWord>> {
    match set_over_socket(socket, key, value, reply_timeout) {
        Err(NoWord::Unanswered(reason)) => {
            check.fail(format!(
                "set {key} {value}: {reason}; the census stopped there"
            ));
            None
        }
        reply => Some(reply),
    }
}

impl CensusCommand {
    /// The value of trial `trial`; an alternating row starts on the value that differs from `original`, so every trial is a write.
    fn value_at(&self, trial: usize, original: &str) -> &'static str {
        let Some(alternate) = self.alternate else {
            return self.value;
        };
        let starts_on_alternate = original == self.value;
        if (trial % 2 == 1) == starts_on_alternate {
            self.value
        } else {
            alternate
        }
    }

    /// The row's name in the report: `key value`, or `key value and alternate in turn`.
    fn label(&self) -> String {
        match self.alternate {
            Some(alternate) => format!("{} {} and {alternate} in turn", self.key, self.value),
            None => format!("{} {}", self.key, self.value),
        }
    }
}

fn census(
    socket: &Path,
    check: &mut Check,
    reply_timeout: Duration,
    originals: &Originals,
) -> Result<Rounds, Interrupted> {
    let original_level = originals.adaptive_level.to_string();
    let mut words: Vec<Vec<String>> = CENSUS.iter().map(|_| Vec::new()).collect();
    let mut times: Vec<Vec<u64>> = CENSUS.iter().map(|_| Vec::new()).collect();
    let mut setups_unconfirmed = 0_usize;
    for trial in 0..CENSUS_TRIALS {
        for (index, command) in CENSUS.iter().enumerate() {
            if stopped() {
                return Err(Interrupted);
            }
            if trial == 0
                && let Some(other) = lead_in(index, originals)
            {
                let Some(reply) = census_set(socket, (command.key, other), reply_timeout, check)
                else {
                    return Ok(Rounds::Unanswered);
                };
                if reply.as_deref() != Ok(CONFIRMED) {
                    check.fail(format!(
                        "set {} {other} before the first trial was not {CONFIRMED}",
                        command.key
                    ));
                }
            }
            if command.in_adaptive {
                let Some(setup) = census_set(socket, ("noise", "adaptive"), reply_timeout, check)
                else {
                    return Ok(Rounds::Unanswered);
                };
                if setup.as_deref() != Ok(CONFIRMED) {
                    setups_unconfirmed = setups_unconfirmed.saturating_add(1);
                }
            }
            let value = command.value_at(trial, &original_level);
            let began = Instant::now();
            let Some(reply) = census_set(socket, (command.key, value), reply_timeout, check) else {
                return Ok(Rounds::Unanswered);
            };
            let word = reply.unwrap_or_default();
            if let (Some(words), Some(times)) = (words.get_mut(index), times.get_mut(index)) {
                times.push(millis(began.elapsed()));
                words.push(word);
            }
        }
    }
    if setups_unconfirmed > 0 {
        check.fail(format!(
            "set noise adaptive was not {CONFIRMED} before adaptive_level in {setups_unconfirmed} of {CENSUS_TRIALS}"
        ));
    }
    let mut confirmable_ms = Vec::new();
    for ((command, words), times) in CENSUS.iter().zip(&words).zip(&times) {
        let expected = words
            .iter()
            .filter(|word| word.as_str() == command.expected)
            .count();
        let others: Vec<&str> = words
            .iter()
            .map(|word| printable(word))
            .filter(|word| *word != command.expected)
            .collect();
        check.note(format!(
            "set {}: {} {expected} of {}, only {} counts, {}{}",
            command.label(),
            command.expected,
            words.len(),
            command.expected,
            super::report::range_text(times),
            if others.is_empty() {
                String::new()
            } else {
                format!(", also {}", others.join(" "))
            }
        ));
        if command.expected == CONFIRMED {
            confirmable_ms.extend_from_slice(times);
        }
        if expected != CENSUS_TRIALS {
            check.fail(format!(
                "set {} was not {} {CENSUS_TRIALS} of {CENSUS_TRIALS}",
                command.label(),
                command.expected
            ));
        }
    }
    check.note("G2b is held by the counts above: a trial past the daemon's confirm timeout answers timeout");
    check.gates.push(Gate {
        name: "G2b",
        samples_ms: confirmable_ms,
        bound: None,
    });
    Ok(Rounds::Answered)
}

/// A reply word as it may be printed: one of the protocol's words, or `error` for anything else.
fn printable(word: &str) -> &str {
    const KNOWN: [&str; 7] = [
        CONFIRMED,
        SENT,
        NOT_WORN,
        "timeout",
        "not_connected",
        "not_supported",
        "bad_request",
    ];
    KNOWN
        .iter()
        .find(|known| **known == word)
        .copied()
        .unwrap_or("error")
}

fn restore_settings(lab: &Lab<'_>, originals: &Originals) -> Result<(), String> {
    let level = originals.adaptive_level.to_string();
    let settings = [
        ("noise", "adaptive"),
        ("adaptive_level", level.as_str()),
        ("noise", originals.noise),
        (
            "conversational_awareness",
            if originals.conversational_awareness {
                "true"
            } else {
                "false"
            },
        ),
        (
            "one_bud_anc",
            if originals.one_bud_anc {
                "true"
            } else {
                "false"
            },
        ),
    ];
    let mut failed = Vec::new();
    let mut unanswered = None;
    for (index, (key, value)) in settings.iter().enumerate() {
        match set_over_socket(&lab.socket(), key, value, SET_REPLY_TIMEOUT) {
            Ok(word) if took_effect(&word) => {}
            // A daemon that gave no reply would make each later set wait as long, so they are not sent.
            Err(NoWord::Unanswered(reason)) => {
                failed.extend(settings.iter().skip(index).map(|(key, _)| *key));
                unanswered = Some(reason);
                break;
            }
            _ => failed.push(key),
        }
    }
    match (failed.is_empty(), unanswered) {
        (true, _) => Ok(()),
        (false, None) => Err(format!("could not put back {}", failed.join(", "))),
        (false, Some(reason)) => Err(format!(
            "could not put back {}: {reason}",
            failed.join(", ")
        )),
    }
}

/// True for a reply word that means the setting went to the `AirPods`.
fn took_effect(word: &str) -> bool {
    word == CONFIRMED || word == SENT
}

/// Writes a prompt for the person at the machine.
fn say(text: &str) {
    crate::report(text);
}

/// Waits for the person to press Enter, waking to notice a stop signal.
fn answered() -> Result<bool, Interrupted> {
    let stdin = io::stdin();
    let tick = Timespec {
        tv_sec: 0,
        tv_nsec: PROMPT_POLL_NANOS,
    };
    let deadline = Instant::now().checked_add(PROMPT_WAIT);
    while deadline.is_some_and(|deadline| Instant::now() < deadline) {
        if stopped() {
            return Err(Interrupted);
        }
        let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
        if poll(&mut fds, Some(&tick)).is_ok_and(|ready| ready > 0) {
            let mut line = String::new();
            return Ok(stdin.lock().read_line(&mut line).is_ok_and(|read| read > 0));
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests;
