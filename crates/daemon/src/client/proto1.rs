//! The protocol 1 commands of the command line: `status`, `watch`, `set` and `do`, one request per connection.

use std::io::{self, BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use pods_core::command::CommandError;
use serde_json::{Map, Value};

use super::{MAX_REPLY_BYTES, REPLY_TIMEOUT, WRITE_TIMEOUT, connect, emit, no_daemon, timed_out};
use crate::protocol::VERSION;

/// The id of the one request each command sends.
const REQUEST_ID: u64 = 1;

/// What a command prints when the daemon closes the connection before the command is done.
const WENT_AWAY: &str = "The daemon went away.\n";

/// The `result` a reply to `set` or `do` carries when the daemon took the command.
const RESULTS: [&str; 2] = ["sent", "confirmed"];

/// Why a command could not finish.
#[derive(Debug)]
enum Failure {
    NoDaemon,
    TimedOut,
    WentAway,
    /// The daemon refused the request, with the message it gave.
    Refused(String),
    /// A line from the daemon that is not the protocol 1 line expected for the request.
    NotUnderstood,
}

/// How a line being read takes one chunk from the socket.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// The whole chunk joined the line, which goes on in the next chunk.
    Continues,
    /// The line ended in the chunk after this many of its bytes, the newline included.
    Ended(usize),
}

/// Prints the state line, as the legacy `status` verb does, and unlike it fails when the line cannot be written.
pub(crate) fn status(socket: &Path, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
    match open(socket, &request("get", &[])).and_then(|(_, reply)| state_reply(&reply)) {
        Ok(line) => match out.write_all(line.as_bytes()).and_then(|()| out.flush()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => unwritten(&error, err),
        },
        Err(failure) => report(failure, "status", socket, err),
    }
}

/// Prints the state line, then one per change until the daemon goes away or nothing reads the output.
pub(crate) fn watch(socket: &Path, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
    let opened = open(socket, &request("subscribe", &[]));
    let (mut reader, mut line) =
        match opened.and_then(|(reader, reply)| Ok((reader, state_reply(&reply)?))) {
            Ok(opened) => opened,
            Err(failure) => return report(failure, "watch", socket, err),
        };
    loop {
        match out.write_all(line.as_bytes()).and_then(|()| out.flush()) {
            Ok(()) => {}
            // Nothing reads the output any more, as under `librepods watch | head -1`, which ends the watch normally.
            Err(error) if error.kind() == ErrorKind::BrokenPipe => return ExitCode::SUCCESS,
            Err(error) => return unwritten(&error, err),
        }
        line = match read_line(&mut reader, None).and_then(|event| event_state_line(&event)) {
            Ok(line) => line,
            Err(failure) => return report(failure, "watch", socket, err),
        };
    }
}

/// Says on stderr that the state line could not be written to standard output, and fails.
fn unwritten(error: &io::Error, err: &mut dyn Write) -> ExitCode {
    emit(
        err,
        format!("Cannot write the state to standard output: {error}\n").as_bytes(),
    );
    ExitCode::FAILURE
}

/// Changes one setting, printing nothing unless the daemon refuses.
pub(crate) fn set(key: &str, value: &str, socket: &Path, err: &mut dyn Write) -> ExitCode {
    let request = request("set", &[("key", key), ("value", value)]);
    match open(socket, &request).and_then(|(_, reply)| result_reply(&reply)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => report(failure, "set", socket, err),
    }
}

/// Asks for one action, printing nothing unless the daemon refuses.
pub(crate) fn act(action: &str, socket: &Path, err: &mut dyn Write) -> ExitCode {
    let request = request("do", &[("action", action)]);
    match open(socket, &request).and_then(|(_, reply)| result_reply(&reply)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => report(failure, "do", socket, err),
    }
}

/// The request line for `op` with string `members`.
fn request(op: &str, members: &[(&str, &str)]) -> String {
    let mut request = Map::new();
    request.insert("v".to_owned(), Value::from(VERSION));
    request.insert("id".to_owned(), Value::from(REQUEST_ID));
    request.insert("op".to_owned(), Value::from(op));
    for &(name, value) in members {
        request.insert(name.to_owned(), Value::from(value));
    }
    format!("{}\n", Value::Object(request))
}

/// Connects, sends `request` and reads its reply line within the reply timeout; the reader is left for any events.
fn open(socket: &Path, request: &str) -> Result<(BufReader<UnixStream>, Vec<u8>), Failure> {
    let stream = connect(socket).ok_or(Failure::NoDaemon)?;
    let mut writer = &stream;
    let sent = stream
        .set_write_timeout(Some(WRITE_TIMEOUT))
        .and_then(|()| writer.write_all(request.as_bytes()));
    // A daemon that accepted but takes no request has closed or stalled, and is gone for this command either way.
    if sent.is_err() {
        return Err(Failure::WentAway);
    }
    let mut reader = BufReader::new(stream);
    let deadline = Instant::now().checked_add(REPLY_TIMEOUT);
    let line = read_line(&mut reader, deadline)?;
    Ok((reader, line))
}

/// One line from the daemon, newline excluded, by `deadline` when there is one.
// Sample input: `{"id":1,"ok":true,"result":"confirmed"}\n`
fn read_line(
    reader: &mut BufReader<UnixStream>,
    deadline: Option<Instant>,
) -> Result<Vec<u8>, Failure> {
    let mut line = Vec::new();
    loop {
        let wait = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
        // A zero wait is refused by the socket, and it means the deadline has passed.
        if wait.is_some_and(|wait| wait.is_zero())
            || reader.get_ref().set_read_timeout(wait).is_err()
        {
            return Err(Failure::TimedOut);
        }
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(Failure::TimedOut);
            }
            // A reset is the daemon going away as much as an end of stream is.
            Err(_) => return Err(Failure::WentAway),
        };
        if available.is_empty() {
            return Err(Failure::WentAway);
        }
        let length = available.len();
        match take(&mut line, available)? {
            Step::Continues => reader.consume(length),
            Step::Ended(used) => {
                reader.consume(used);
                return Ok(line);
            }
        }
    }
}

/// Adds the part of `chunk` before its first newline to `line`, refusing a line over `MAX_REPLY_BYTES`, newline excluded.
// Sample input: `line` holds `{"id":1,"ok"` and `chunk` is `:true,"result":"sent"}\n{"event"`.
fn take(line: &mut Vec<u8>, chunk: &[u8]) -> Result<Step, Failure> {
    let newline = chunk.iter().position(|&byte| byte == b'\n');
    let part = chunk
        .get(..newline.unwrap_or(chunk.len()))
        .unwrap_or_default();
    if line.len().saturating_add(part.len()) > MAX_REPLY_BYTES {
        return Err(Failure::NotUnderstood);
    }
    line.extend_from_slice(part);
    Ok(newline.map_or(Step::Continues, |at| Step::Ended(at.saturating_add(1))))
}

/// The members of the reply to the request sent, or the refusal it carries.
// Sample input: `{"id":1,"ok":false,"error":{"code":"not_connected","message":"The AirPods are not connected."}}`
fn reply(line: &[u8]) -> Result<Map<String, Value>, Failure> {
    let Ok(Value::Object(reply)) = serde_json::from_slice::<Value>(line) else {
        return Err(Failure::NotUnderstood);
    };
    let ours = reply.get("id").and_then(Value::as_u64) == Some(REQUEST_ID);
    match reply.get("ok") {
        Some(Value::Bool(true)) if ours => Ok(reply),
        Some(Value::Bool(false)) => Err(refusal(&reply, ours)),
        Some(_) | None => Err(Failure::NotUnderstood),
    }
}

/// The failure a reply with `ok` false gives, where `ours` says its `id` is the request's.
fn refusal(reply: &Map<String, Value>, ours: bool) -> Failure {
    let error = reply.get("error");
    let code = error
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str);
    // The daemon answers with a null `id` only a line it could not take an `id` from, and only with this code.
    let unread =
        reply.get("id") == Some(&Value::Null) && code == Some(CommandError::BadRequest.code());
    match error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
    {
        Some(message) if ours || unread => Failure::Refused(message.to_owned()),
        Some(_) | None => Failure::NotUnderstood,
    }
}

/// The state line of the reply to `get` or `subscribe`.
// Sample input: `{"id":1,"ok":true,"state":{"connected":false}}`
fn state_reply(line: &[u8]) -> Result<String, Failure> {
    state_line(&reply(line)?)
}

/// Checks that the reply to `set` or `do` carries one of the `RESULTS`.
// Sample input: `{"id":1,"ok":true,"result":"confirmed"}`
fn result_reply(line: &[u8]) -> Result<(), Failure> {
    match reply(line)?.get("result").and_then(Value::as_str) {
        Some(result) if RESULTS.contains(&result) => Ok(()),
        Some(_) | None => Err(Failure::NotUnderstood),
    }
}

/// The state line a reply or an event carries in its `state` member.
fn state_line(document: &Map<String, Value>) -> Result<String, Failure> {
    match document.get("state") {
        Some(state @ Value::Object(_)) => Ok(format!("{state}\n")),
        Some(_) | None => Err(Failure::NotUnderstood),
    }
}

/// The state line of one event line.
// Sample input: `{"event":"state","state":{"connected":false}}`
fn event_state_line(line: &[u8]) -> Result<String, Failure> {
    let Ok(Value::Object(event)) = serde_json::from_slice::<Value>(line) else {
        return Err(Failure::NotUnderstood);
    };
    if event.get("event").and_then(Value::as_str) != Some("state") {
        return Err(Failure::NotUnderstood);
    }
    state_line(&event)
}

/// Prints why `command` could not finish and fails.
fn report(failure: Failure, command: &str, socket: &Path, err: &mut dyn Write) -> ExitCode {
    match failure {
        Failure::NoDaemon => return no_daemon(socket, err),
        Failure::TimedOut => return timed_out(command, err),
        Failure::WentAway => emit(err, WENT_AWAY.as_bytes()),
        Failure::Refused(message) => emit(err, format!("{message}\n").as_bytes()),
        Failure::NotUnderstood => emit(
            err,
            format!("The daemon's reply to {command} was not understood.\n").as_bytes(),
        ),
    }
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests;
