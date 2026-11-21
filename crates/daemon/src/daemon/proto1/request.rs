//! Protocol 1 requests: one JSON object per line, read into a typed request or a typed refusal.

use pods_core::command::{Command, CommandError};
use pods_core::settings;
use serde_json::{Map, Value};

use crate::protocol::VERSION;

/// What a valid request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Request {
    Get,
    Subscribe,
    /// A `set` or a `do`, which the hub carries out.
    Submit(Command),
}

/// Why a line was refused, with the id to echo when the line carried a usable one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Refusal {
    pub(super) id: Option<u64>,
    pub(super) error: RequestError,
}

/// The refusals a request line can get.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RequestError {
    Command(CommandError),
    /// `v` is present and is not `VERSION`.
    UnsupportedVersion,
}

/// Reads one request line, newline excluded; only a line with no usable `id` is refused with no id.
// Sample input: `{"v":1,"id":9,"op":"set","key":"noise","value":"anc"}`
pub(super) fn parse(line: &[u8]) -> Result<(u64, Request), Refusal> {
    let bad_request = RequestError::Command(CommandError::BadRequest);
    // The reply names the line as bad; why the JSON did not parse is no use to the client.
    let Ok(Value::Object(members)) = serde_json::from_slice::<Value>(line) else {
        return Err(Refusal {
            id: None,
            error: bad_request,
        });
    };
    let Some(id) = members.get("id").and_then(Value::as_u64) else {
        return Err(Refusal {
            id: None,
            error: bad_request,
        });
    };
    let refuse = |error| Refusal {
        id: Some(id),
        error,
    };
    let Some(version) = members.get("v") else {
        return Err(refuse(bad_request));
    };
    if version.as_u64() != Some(VERSION) {
        return Err(refuse(RequestError::UnsupportedVersion));
    }
    let request = match members.get("op").and_then(Value::as_str) {
        Some("get") => Ok(Request::Get),
        Some("subscribe") => Ok(Request::Subscribe),
        Some("set") => set(&members).map(Request::Submit),
        Some("do") => act(&members).map(Request::Submit),
        Some(_) | None => Err(CommandError::BadRequest),
    };
    request
        .map(|request| (id, request))
        .map_err(|error| refuse(RequestError::Command(error)))
}

/// The command of a `set`, whose `key` and `value` are both strings.
fn set(members: &Map<String, Value>) -> Result<Command, CommandError> {
    let (Some(key), Some(value)) = (text(members, "key"), text(members, "value")) else {
        return Err(CommandError::BadRequest);
    };
    settings::setting(key, value)
}

/// The command of a `do`, whose `action` is a string.
fn act(members: &Map<String, Value>) -> Result<Command, CommandError> {
    let action = text(members, "action").ok_or(CommandError::BadRequest)?;
    settings::action(action)
}

/// The member `name` when it is a string.
fn text<'line>(members: &'line Map<String, Value>, name: &str) -> Option<&'line str> {
    members.get(name).and_then(Value::as_str)
}

#[cfg(test)]
mod tests;
