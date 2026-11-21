//! The protocol 1 commands of the command line: `status`, `watch`, `set` and `do`, one request per connection.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use serde_json::{Map, Value};

use super::{MAX_REPLY_BYTES, REPLY_TIMEOUT, WRITE_TIMEOUT, connect, emit, no_daemon, timed_out};
use crate::protocol::VERSION;

/// The id of the one request each command sends.
const REQUEST_ID: u64 = 1;

/// What a command prints when the daemon closes the connection before the command is done.
const WENT_AWAY: &str = "The daemon went away.\n";

/// Why a command could not finish.
#[derive(Debug)]
enum Failure {
    NoDaemon,
    TimedOut,
    WentAway,
    /// The daemon refused the request, with the message it gave.
    Refused(String),
    /// A line from the daemon that is not the protocol 1 line expected, with what is wrong with it.
    Garbled(String),
}

/// Prints the state line, as the legacy `status` verb does.
pub(crate) fn status(socket: &Path, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
    match open(socket, &request("get", &[])).and_then(|(_, reply)| state_line(&reply)) {
        Ok(line) => {
            emit(out, line.as_bytes());
            ExitCode::SUCCESS
        }
        Err(failure) => report(failure, "status", socket, err),
    }
}

/// Prints the state line, then one per change until the daemon goes away or nothing reads the output.
pub(crate) fn watch(socket: &Path, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
    let opened = open(socket, &request("subscribe", &[]));
    let (mut reader, mut line) =
        match opened.and_then(|(reader, reply)| Ok((reader, state_line(&reply)?))) {
            Ok(opened) => opened,
            Err(failure) => return report(failure, "watch", socket, err),
        };
    loop {
        match out.write_all(line.as_bytes()).and_then(|()| out.flush()) {
            Ok(()) => {}
            // Nothing reads the output any more, as under `librepods watch | head -1`, which ends the watch normally.
            Err(error) if error.kind() == ErrorKind::BrokenPipe => return ExitCode::SUCCESS,
            Err(error) => {
                emit(
                    err,
                    format!("Cannot write the state to standard output: {error}\n").as_bytes(),
                );
                return ExitCode::FAILURE;
            }
        }
        line = match read_line(&mut reader, None).and_then(|event| event_state_line(&event)) {
            Ok(line) => line,
            Err(failure) => return report(failure, "watch", socket, err),
        };
    }
}

/// Changes one setting, printing nothing unless the daemon refuses.
pub(crate) fn set(key: &str, value: &str, socket: &Path, err: &mut dyn Write) -> ExitCode {
    let request = request("set", &[("key", key), ("value", value)]);
    match open(socket, &request) {
        Ok(_) => ExitCode::SUCCESS,
        Err(failure) => report(failure, "set", socket, err),
    }
}

/// Asks for one action, printing nothing unless the daemon refuses.
pub(crate) fn act(action: &str, socket: &Path, err: &mut dyn Write) -> ExitCode {
    match open(socket, &request("do", &[("action", action)])) {
        Ok(_) => ExitCode::SUCCESS,
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

/// Connects, sends `request` and reads its reply within the reply timeout; the reader is left for any events.
fn open(socket: &Path, request: &str) -> Result<(BufReader<UnixStream>, Value), Failure> {
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
    let reply = reply(&line)?;
    Ok((reader, reply))
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
        let newline = available.iter().position(|&byte| byte == b'\n');
        let end = newline.unwrap_or(available.len());
        line.extend_from_slice(available.get(..end).unwrap_or_default());
        reader.consume(newline.map_or(end, |at| at.saturating_add(1)));
        if newline.is_some() {
            return Ok(line);
        }
        if line.len() > MAX_REPLY_BYTES {
            return Err(Failure::Garbled(format!(
                "a line longer than {MAX_REPLY_BYTES} bytes"
            )));
        }
    }
}

/// The reply in `line`, or the refusal it carries.
// Sample input: `{"id":1,"ok":false,"error":{"code":"not_connected","message":"The AirPods are not connected."}}`
fn reply(line: &[u8]) -> Result<Value, Failure> {
    let reply: Value = serde_json::from_slice(line).map_err(|error| {
        Failure::Garbled(format!("{error} in {}", String::from_utf8_lossy(line)))
    })?;
    match reply.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(reply),
        Some(false) => match reply.pointer("/error/message").and_then(Value::as_str) {
            Some(message) => Err(Failure::Refused(message.to_owned())),
            None => Err(garbled("a refusal with no error message", line)),
        },
        None => Err(garbled("a reply with no ok member", line)),
    }
}

/// The state line a reply or an event carries in its `state` member.
fn state_line(document: &Value) -> Result<String, Failure> {
    match document.get("state") {
        Some(state @ Value::Object(_)) => Ok(format!("{state}\n")),
        Some(_) | None => Err(Failure::Garbled(format!("no state object in {document}"))),
    }
}

/// The state line of one event line.
// Sample input: `{"event":"state","state":{"connected":false}}`
fn event_state_line(line: &[u8]) -> Result<String, Failure> {
    let event: Value = serde_json::from_slice(line).map_err(|error| {
        Failure::Garbled(format!("{error} in {}", String::from_utf8_lossy(line)))
    })?;
    if event.get("event").and_then(Value::as_str) != Some("state") {
        return Err(garbled("a line that is not a state event", line));
    }
    state_line(&event)
}

fn garbled(what: &str, line: &[u8]) -> Failure {
    Failure::Garbled(format!("{what}: {}", String::from_utf8_lossy(line)))
}

/// Prints why `command` could not finish and fails.
fn report(failure: Failure, command: &str, socket: &Path, err: &mut dyn Write) -> ExitCode {
    match failure {
        Failure::NoDaemon => return no_daemon(socket, err),
        Failure::TimedOut => return timed_out(command, err),
        Failure::WentAway => emit(err, WENT_AWAY.as_bytes()),
        Failure::Refused(message) => emit(err, format!("{message}\n").as_bytes()),
        Failure::Garbled(detail) => emit(
            err,
            format!("The daemon sent a line that is not protocol 1, {detail}\n").as_bytes(),
        ),
    }
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests;
