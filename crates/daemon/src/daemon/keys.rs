//! The one pair of proximity keys kept in `keys.json` beside the state file, owner only.

use std::fmt;
use std::path::{Path, PathBuf};

use pods_core::link::SavedKeys;
use pods_proto::addr::BdAddr;
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

const DEVICE: &str = "device";

const HEX_RADIX: u32 = 16;

const ADDRESS_LEN: usize = 6;

/// Holds the keys and device without reallocating the buffer and leaving a copy behind.
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
pub(super) fn load(path: &Path) -> Option<SavedKeys> {
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
pub(super) fn queue() -> (mpsc::Sender<SavedKeys>, mpsc::Receiver<SavedKeys>) {
    mpsc::channel(QUEUE)
}

/// Queues `keys` without waiting; a full queue drops the pair with a log line.
pub(super) fn offer(pairs: &mpsc::Sender<SavedKeys>, keys: SavedKeys) {
    // A closed queue is a writer that already stopped, which only a stopping daemon has.
    if let Err(TrySendError::Full(_)) = pairs.try_send(keys) {
        log(QUEUE_FULL);
    }
}

/// Writes each pair unlike `stored`, the pair on disk; a failed write keeps the old one, so the next reply tries again.
pub(super) async fn follow(
    mut pairs: mpsc::Receiver<SavedKeys>,
    path: PathBuf,
    mut stored: Option<SavedKeys>,
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

/// A line with two 32-digit keys and, if known, the device as 12 hex digits.
fn render(saved: &SavedKeys) -> Zeroizing<String> {
    let mut text = Zeroizing::new(String::with_capacity(TEXT_CAPACITY));
    text.push('{');
    push_entry(&mut text, IDENTITY, saved.keys.identity.expose());
    text.push(',');
    push_entry(&mut text, PAYLOAD, saved.keys.payload.expose());
    if let Some(device) = saved.device {
        text.push(',');
        push_entry(&mut text, DEVICE, &device.octets());
    }
    text.push_str("}\n");
    text
}

fn push_entry(text: &mut String, name: &str, bytes: &[u8]) {
    text.push('"');
    text.push_str(name);
    text.push_str("\":\"");
    for byte in bytes {
        let byte = u32::from(*byte);
        text.extend(char::from_digit(byte / HEX_RADIX, HEX_RADIX));
        text.extend(char::from_digit(byte % HEX_RADIX, HEX_RADIX));
    }
    text.push('"');
}

// Sample input: `{"identity":"5ac30f9621e87b44d019a63e82f56cb7","payload":"9d0471ea38cf568b12fe67a02db943c8"}`.
fn parse(bytes: &[u8]) -> Result<SavedKeys, FileError> {
    let Ok(Value::Object(mut fields)) = serde_json::from_slice(bytes) else {
        return Err(FileError::NotAnObject);
    };
    // Both taken before either is checked, so a refused identity never drops the payload digits unwiped.
    let identity = hex_field::<KEY_LEN>(fields.remove(IDENTITY));
    let payload = hex_field::<KEY_LEN>(fields.remove(PAYLOAD));
    let device = hex_field::<ADDRESS_LEN>(fields.remove(DEVICE)).map(|bytes| BdAddr::from(*bytes));
    Ok(SavedKeys {
        keys: ProximityKeys {
            identity: Key::from_bytes(*identity.ok_or(FileError::Identity)?),
            payload: Key::from_bytes(*payload.ok_or(FileError::Payload)?),
        },
        device,
    })
}

/// Sample input: `"5ac30f9621e87b44d019a63e82f56cb7"`, exactly two hex digits per byte.
fn hex_field<const LEN: usize>(field: Option<Value>) -> Option<Zeroizing<[u8; LEN]>> {
    let Some(Value::String(digits)) = field else {
        return None;
    };
    let digits = Zeroizing::new(digits);
    let (pairs, extra) = digits.as_bytes().as_chunks::<2>();
    if pairs.len() != LEN || !extra.is_empty() {
        return None;
    }
    let mut bytes = Zeroizing::new([0_u8; LEN]);
    for (byte, &[high, low]) in bytes.iter_mut().zip(pairs) {
        let high = char::from(high).to_digit(HEX_RADIX)?;
        let low = char::from(low).to_digit(HEX_RADIX)?;
        *byte = u8::try_from(high.checked_mul(HEX_RADIX)?.checked_add(low)?).ok()?;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests;
