//! C6, the command census: each setting command ten times over protocol 1 and shown in the status at once, the originals put back and read back, then one with no bud in.

use std::io::{self, ErrorKind, Read, Write};
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

/// The reply word of a command carried out with no write, which no census command answers.
const CONFIRMED: &str = "confirmed";
/// The reply word of a setting command whose write is on the link.
const SENT: &str = "sent";
/// The error code of a command that needs a bud in an ear.
const NOT_WORN: &str = "not_worn";
/// How long the person has to answer a prompt.
const PROMPT_WAIT: Duration = Duration::from_secs(300);
/// How long a `set` over the socket may wait for its reply, far past the write it answers.
const SET_REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest reply line read from the daemon, as the command line client bounds it.
const MAX_REPLY_BYTES: usize = 65536;
const READ_CHUNK_BYTES: usize = 4096;
/// How long a connect waits before it tries a full listen backlog again.
const CONNECT_RETRY: Duration = Duration::from_millis(10);
/// Trials of each census command in C6.
const CENSUS_TRIALS: usize = 10;
/// How often the prompt wait wakes to notice a stop signal, in nanoseconds.
const PROMPT_POLL_NANOS: i64 = 250_000_000;

/// One census command, and whether it needs adaptive mode first.
struct CensusCommand {
    key: &'static str,
    value: &'static str,
    in_adaptive: bool,
}

/// The census of commands C6 sends, each answered `sent`; each key's rows alternate, so a status stuck at one value fails the other row.
const CENSUS: [CensusCommand; 8] = [
    CensusCommand {
        key: "noise",
        value: "transparency",
        in_adaptive: false,
    },
    CensusCommand {
        key: "noise",
        value: "anc",
        in_adaptive: false,
    },
    CensusCommand {
        key: "conversational_awareness",
        value: "true",
        in_adaptive: false,
    },
    CensusCommand {
        key: "conversational_awareness",
        value: "false",
        in_adaptive: false,
    },
    CensusCommand {
        key: "one_bud_anc",
        value: "true",
        in_adaptive: false,
    },
    CensusCommand {
        key: "one_bud_anc",
        value: "false",
        in_adaptive: false,
    },
    CensusCommand {
        key: "adaptive_level",
        value: "40",
        in_adaptive: true,
    },
    CensusCommand {
        key: "adaptive_level",
        value: "60",
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

/// Indexed by the status document's `noise_mode` number.
const NOISE_WORDS: [&str; 4] = ["off", "anc", "transparency", "adaptive"];

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

enum WordError {
    Io(io::Error),
    Word(NoWord),
}

/// Sample input `{"id":1,"ok":false,"error":{"code":"not_worn","message":"m"}}`.
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

/// Retried because a full listen backlog answers `EAGAIN` rather than waiting.
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

/// The `set` keys of the settings C6 changes.
const SETTING_KEYS: [&str; 4] = [
    "noise",
    "conversational_awareness",
    "one_bud_anc",
    "adaptive_level",
];

/// The settings C6 changes, as a status document holds them.
struct Settings {
    noise: &'static str,
    conversational_awareness: bool,
    one_bud_anc: bool,
    adaptive_level: u64,
}

impl Settings {
    /// The `set` value of `key`.
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

/// The capability keys in `NEEDED_FEATURES` not true in the status line; sample input `{"supports_adaptive":true,"supports_one_bud_anc":false}` among its other keys.
fn missing_features(state: &Map<String, Value>) -> Vec<&'static str> {
    NEEDED_FEATURES
        .into_iter()
        .filter(|key| state.get(*key).and_then(Value::as_bool) != Some(true))
        .collect()
}

/// The settings in the status line; sample input `{"adaptive_noise_level":50,"conversational_awareness":false,"noise_mode":0,"one_bud_anc_mode":false}` among its other keys.
fn settings(state: &Map<String, Value>) -> Result<Settings, String> {
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
    Ok(Settings {
        noise,
        conversational_awareness: flag("conversational_awareness")?,
        one_bud_anc: flag("one_bud_anc_mode")?,
        adaptive_level: state
            .get("adaptive_noise_level")
            .and_then(Value::as_u64)
            .ok_or_else(|| missing("adaptive_noise_level"))?,
    })
}

/// C6: each census command ten times, the originals put back on every way out and read back, then `not_worn` with both buds out.
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
    let originals = match settings(&state) {
        Ok(originals) => originals,
        Err(error) => {
            check.fail(error);
            return Ok(check);
        }
    };
    let rounds = census(&lab.socket(), &mut check, SET_REPLY_TIMEOUT, &|| {
        lab.status()
    });
    let restored =
        restore_settings(lab, &originals).and_then(|()| read_back(&|| lab.status(), &originals));
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
        let put_back = put_back.is_ok_and(|word| word == SENT);
        check.note(format!("noise put back: {put_back}"));
        if !put_back {
            check.fail("could not put back noise");
        }
    }
    Ok(check)
}

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

/// Reads the status document the daemon serves.
type StatusReader<'a> = &'a dyn Fn() -> Result<Map<String, Value>, String>;

/// Whether the status `status` reads shows `key` at the `set` value `value`.
fn shown(status: StatusReader<'_>, key: &str, value: &str) -> Result<bool, String> {
    let shows = status().and_then(|state| settings(&state))?;
    Ok(shows.value_of(key).as_deref() == Some(value))
}

fn census(
    socket: &Path,
    check: &mut Check,
    reply_timeout: Duration,
    status: StatusReader<'_>,
) -> Result<Rounds, Interrupted> {
    let mut words: Vec<Vec<String>> = CENSUS.iter().map(|_| Vec::new()).collect();
    let mut times: Vec<Vec<u64>> = CENSUS.iter().map(|_| Vec::new()).collect();
    let mut shown_at_once = vec![0_usize; CENSUS.len()];
    let mut setups_unsent = 0_usize;
    let mut unread_status = None;
    for _ in 0..CENSUS_TRIALS {
        for (index, command) in CENSUS.iter().enumerate() {
            if stopped() {
                return Err(Interrupted);
            }
            if command.in_adaptive {
                let Some(setup) = census_set(socket, ("noise", "adaptive"), reply_timeout, check)
                else {
                    return Ok(Rounds::Unanswered);
                };
                if setup.as_deref() != Ok(SENT) {
                    setups_unsent = setups_unsent.saturating_add(1);
                }
            }
            let began = Instant::now();
            let Some(reply) =
                census_set(socket, (command.key, command.value), reply_timeout, check)
            else {
                return Ok(Rounds::Unanswered);
            };
            let elapsed = millis(began.elapsed());
            match shown(status, command.key, command.value) {
                Ok(true) => {
                    if let Some(count) = shown_at_once.get_mut(index) {
                        *count = count.saturating_add(1);
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    unread_status.get_or_insert(error);
                }
            }
            if let (Some(words), Some(times)) = (words.get_mut(index), times.get_mut(index)) {
                times.push(elapsed);
                words.push(reply.unwrap_or_default());
            }
        }
    }
    if let Some(error) = unread_status {
        check.fail(format!("the status could not be read after a set: {error}"));
    }
    if setups_unsent > 0 {
        let setups =
            CENSUS_TRIALS.saturating_mul(CENSUS.iter().filter(|row| row.in_adaptive).count());
        check.fail(format!(
            "set noise adaptive was not {SENT} before adaptive_level in {setups_unsent} of {setups}"
        ));
    }
    let mut all_ms = Vec::new();
    for (((command, words), times), shown) in
        CENSUS.iter().zip(&words).zip(&times).zip(shown_at_once)
    {
        let sent = words.iter().filter(|word| word.as_str() == SENT).count();
        let others: Vec<&str> = words
            .iter()
            .map(|word| printable(word))
            .filter(|word| *word != SENT)
            .collect();
        let label = format!("{} {}", command.key, command.value);
        check.note(format!(
            "set {label}: {SENT} {sent} of {}, shown at once {shown} of {}, {}{}",
            words.len(),
            words.len(),
            super::report::range_text(times),
            if others.is_empty() {
                String::new()
            } else {
                format!(", also {}", others.join(" "))
            }
        ));
        all_ms.extend_from_slice(times);
        if sent != CENSUS_TRIALS {
            check.fail(format!(
                "set {label} was not {SENT} {CENSUS_TRIALS} of {CENSUS_TRIALS}"
            ));
        }
        if shown != CENSUS_TRIALS {
            check.fail(format!(
                "set {label} was not shown at once {CENSUS_TRIALS} of {CENSUS_TRIALS}"
            ));
        }
    }
    check.gates.push(Gate {
        name: "G2b",
        samples_ms: all_ms,
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

fn restore_settings(lab: &Lab<'_>, originals: &Settings) -> Result<(), String> {
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
            Ok(word) if word == SENT => {}
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

/// Reads the status back and compares its settings with the originals.
fn read_back(status: StatusReader<'_>, originals: &Settings) -> Result<(), String> {
    let back = status().and_then(|state| settings(&state))?;
    let changed: Vec<&str> = SETTING_KEYS
        .into_iter()
        .filter(|key| back.value_of(key) != originals.value_of(key))
        .collect();
    if changed.is_empty() {
        Ok(())
    } else {
        Err(format!("{} read back changed", changed.join(", ")))
    }
}

/// Writes a prompt for the person at the machine.
fn say(text: &str) {
    crate::report(text);
}

/// A byte at a time, so an unfinished line cannot hold up a stop signal.
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
            let mut byte = [0_u8; 1];
            match rustix::io::read(&stdin, &mut byte) {
                Ok(0) => return Ok(false),
                Ok(_) if byte == *b"\n" => return Ok(true),
                Ok(_) | Err(Errno::INTR | Errno::AGAIN) => {}
                Err(_) => return Ok(false),
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests;
