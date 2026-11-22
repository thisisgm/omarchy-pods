//! The one pair of proximity keys kept in `keys.json` beside the state file, owner only.

use std::fmt;
use std::path::{Path, PathBuf};

use pods_proto::keys::{KEY_LEN, Key, ProximityKeys};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::spawn_blocking;
use zeroize::Zeroizing;

use super::{log, owner_file};

pub(super) const FILE_NAME: &str = "keys.json";

/// The link machine has one active device and asks once per link, so a second pair waits only while the first is written.
const QUEUE: usize = 2;

const QUEUE_FULL: &str = "keys: the writer is behind; this pair is not stored";

const IDENTITY: &str = "identity";

const PAYLOAD: &str = "payload";

const HEX_RADIX: u32 = 16;

const HEX_LEN: usize = KEY_LEN * 2;

/// Past the 93 bytes of the file, so the text never moves to a larger buffer and leaves a copy behind.
const TEXT_CAPACITY: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileError {
    NotAnObject,
    Identity,
    Payload,
}

impl fmt::Display for FileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotAnObject => "not a JSON object",
            Self::Identity => "no identity key of 32 hex digits",
            Self::Payload => "no payload key of 32 hex digits",
        })
    }
}

/// Runs once before the runtime starts; the line for a file it cannot use names the fault, never the content.
pub(super) fn load(path: &Path) -> Option<ProximityKeys> {
    let reason = match owner_file::read_private(path) {
        Ok(None) => return None,
        Ok(Some(bytes)) => match parse(&Zeroizing::new(bytes)) {
            Ok(keys) => return Some(keys),
            Err(error) => error.to_string(),
        },
        Err(error) => error.to_string(),
    };
    log(&format!("keys: cannot read {}: {reason}", path.display()));
    None
}

/// The one place the queue's bound is set.
pub(super) fn queue() -> (mpsc::Sender<ProximityKeys>, mpsc::Receiver<ProximityKeys>) {
    mpsc::channel(QUEUE)
}

/// Queues `keys` without waiting; a full queue drops the pair with a log line.
pub(super) fn offer(pairs: &mpsc::Sender<ProximityKeys>, keys: ProximityKeys) {
    // A closed queue is a writer that already stopped, which only a stopping daemon has.
    if let Err(TrySendError::Full(_)) = pairs.try_send(keys) {
        log(QUEUE_FULL);
    }
}

/// Writes each pair unlike `stored`, the pair on disk; a failed write keeps the old one, so the next reply tries again.
pub(super) async fn follow(
    mut pairs: mpsc::Receiver<ProximityKeys>,
    path: PathBuf,
    mut stored: Option<ProximityKeys>,
) {
    while let Some(keys) = pairs.recv().await {
        if stored.as_ref() == Some(&keys) {
            continue;
        }
        let target = path.clone();
        let written = spawn_blocking(move || {
            let result = owner_file::write(&target, render(&keys).as_bytes());
            (keys, result)
        });
        match written.await {
            Ok((keys, Ok(()))) => stored = Some(keys),
            Ok((_, Err(error))) => log(&format!("keys: cannot write {}: {error}", path.display())),
            Err(error) => log(&format!("keys: the write did not finish: {error}")),
        }
    }
}

/// `{"identity":"<32 hex digits>","payload":"<32 hex digits>"}` and a newline.
fn render(keys: &ProximityKeys) -> Zeroizing<String> {
    let mut text = Zeroizing::new(String::with_capacity(TEXT_CAPACITY));
    text.push('{');
    push_entry(&mut text, IDENTITY, &keys.identity);
    text.push(',');
    push_entry(&mut text, PAYLOAD, &keys.payload);
    text.push_str("}\n");
    text
}

fn push_entry(text: &mut String, name: &str, key: &Key) {
    text.push('"');
    text.push_str(name);
    text.push_str("\":\"");
    for byte in key.expose() {
        let byte = u32::from(*byte);
        text.extend(char::from_digit(byte / HEX_RADIX, HEX_RADIX));
        text.extend(char::from_digit(byte % HEX_RADIX, HEX_RADIX));
    }
    text.push('"');
}

// Sample input: `{"identity":"5ac30f9621e87b44d019a63e82f56cb7","payload":"9d0471ea38cf568b12fe67a02db943c8"}`.
fn parse(bytes: &[u8]) -> Result<ProximityKeys, FileError> {
    let Ok(Value::Object(mut fields)) = serde_json::from_slice(bytes) else {
        return Err(FileError::NotAnObject);
    };
    // Both taken before either is checked, so a refused identity never drops the payload digits unwiped.
    let identity = key(fields.remove(IDENTITY));
    let payload = key(fields.remove(PAYLOAD));
    Ok(ProximityKeys {
        identity: identity.ok_or(FileError::Identity)?,
        payload: payload.ok_or(FileError::Payload)?,
    })
}

/// Sample input: `"5ac30f9621e87b44d019a63e82f56cb7"`, exactly 32 hex digits in either case.
fn key(field: Option<Value>) -> Option<Key> {
    let Some(Value::String(digits)) = field else {
        return None;
    };
    let digits = Zeroizing::new(digits);
    if digits.len() != HEX_LEN {
        return None;
    }
    let mut bytes = Zeroizing::new([0_u8; KEY_LEN]);
    let (pairs, _) = digits.as_bytes().as_chunks::<2>();
    for (byte, &[high, low]) in bytes.iter_mut().zip(pairs) {
        let high = char::from(high).to_digit(HEX_RADIX)?;
        let low = char::from(low).to_digit(HEX_RADIX)?;
        *byte = u8::try_from(high.checked_mul(HEX_RADIX)?.checked_add(low)?).ok()?;
    }
    Some(Key::from_bytes(*bytes))
}

#[cfg(test)]
mod tests;
