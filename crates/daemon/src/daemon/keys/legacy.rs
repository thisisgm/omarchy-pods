//! The two proximity keys written by the C++ daemon as `QSettings` byte arrays.

use std::fmt;
use std::iter::Peekable;
use std::path::Path;

use pods_core::link::SavedKeys;
use pods_proto::keys::{KEY_LEN, Key, ProximityKeys};
use zeroize::Zeroizing;

use super::{HEX_RADIX, render};
use crate::daemon::{legacy_conf::LegacyConf, log, owner_file};

const DEVICE_INFO: &str = "[DeviceInfo]";
const IDENTITY: &str = "magicAccIRK";
const PAYLOAD: &str = "magicAccEncKey";

#[derive(Clone, Copy)]
enum Problem {
    IniText,
    Missing(&'static str),
    Malformed(&'static str),
    Length(&'static str),
}

impl fmt::Display for Problem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IniText => formatter.write_str("malformed INI text"),
            Self::Missing(field) => write!(formatter, "missing {field}"),
            Self::Malformed(field) => write!(formatter, "malformed {field}"),
            Self::Length(field) => write!(formatter, "{field} is not {KEY_LEN} bytes"),
        }
    }
}

pub(super) fn import(path: &Path, legacy: &LegacyConf<'_>) -> Option<SavedKeys> {
    let old = legacy.path?;
    let keys = match parse(legacy.read()?) {
        Ok(keys) => keys,
        Err(reason) => {
            log(&format!("keys: cannot import {}: {reason}", old.display()));
            return None;
        }
    };
    if let Err(error) = owner_file::write_new(path, render(&keys).as_bytes()) {
        log(&format!("keys: cannot write {}: {error}", path.display()));
        return None;
    }
    Some(keys)
}

// Sample input: `[DeviceInfo]\nmagicAccIRK=@ByteArray(abcdefghijklmnop)\nmagicAccEncKey=@ByteArray(QRSTUVWXYZ012345)`.
fn parse(bytes: &[u8]) -> Result<SavedKeys, Problem> {
    let text = str::from_utf8(bytes).map_err(|_invalid_utf8| Problem::IniText)?;
    let mut in_device_info = false;
    let mut identity = None;
    let mut payload = None;
    for line in text.trim_start_matches('\u{feff}').lines().map(str::trim) {
        if line.starts_with('[') {
            in_device_info = line == DEVICE_INFO;
            continue;
        }
        if !in_device_info {
            continue;
        }
        let Some((field, value)) = line.split_once('=') else {
            continue;
        };
        let (slot, field) = match field.trim() {
            IDENTITY => (&mut identity, IDENTITY),
            PAYLOAD => (&mut payload, PAYLOAD),
            _ => continue,
        };
        if slot.is_some() {
            return Err(Problem::Malformed(field));
        }
        *slot = Some(decode(field, value.trim())?);
    }
    Ok(SavedKeys {
        keys: ProximityKeys {
            identity: identity.ok_or(Problem::Missing(IDENTITY))?,
            payload: payload.ok_or(Problem::Missing(PAYLOAD))?,
        },
        device: None,
    })
}

// Sample input: `"@ByteArray(\x1\x61\\\";)"`. QSettings quotes the whole value when it contains a semicolon, comma or equals.
fn decode(field: &'static str, value: &str) -> Result<Key, Problem> {
    let malformed = Problem::Malformed(field);
    let quoted = value.starts_with('"');
    let value = if quoted {
        value
            .strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
            .ok_or(malformed)?
    } else {
        value
    };
    let body = value
        .strip_prefix("@ByteArray(")
        .and_then(|rest| rest.strip_suffix(')'))
        .ok_or(malformed)?;
    let mut input = body.bytes().peekable();
    let mut key = Zeroizing::new([0; KEY_LEN]);
    for byte in key.iter_mut() {
        *byte = match input.next().ok_or(Problem::Length(field))? {
            b'\\' => escaped(&mut input).ok_or(malformed)?,
            b'"' => return Err(malformed),
            b';' | b',' | b'=' if !quoted => return Err(malformed),
            plain @ b' '..=b'~' => plain,
            _ => return Err(malformed),
        };
    }
    if input.peek().is_some() {
        return Err(Problem::Length(field));
    }
    Ok(Key::from_bytes(*key))
}

// Sample input after a backslash: `x1\x61` decodes the first escape, leaving the next one for the caller.
fn escaped(input: &mut Peekable<impl Iterator<Item = u8>>) -> Option<u8> {
    Some(match input.next()? {
        b'0' if !input.peek().is_some_and(u8::is_ascii_hexdigit) => 0,
        b'a' => 0x07,
        b'b' => 0x08,
        b'f' => 0x0c,
        b'n' => b'\n',
        b'r' => b'\r',
        b't' => b'\t',
        b'v' => 0x0b,
        byte @ (b'"' | b'\\') => byte,
        b'x' => {
            let high = char::from(input.next()?).to_digit(HEX_RADIX)?;
            if input.peek().is_some_and(u8::is_ascii_hexdigit) {
                let low = char::from(input.next()?).to_digit(HEX_RADIX)?;
                if input.peek().is_some_and(u8::is_ascii_hexdigit) {
                    return None;
                }
                u8::try_from(high.checked_mul(HEX_RADIX)?.checked_add(low)?).ok()?
            } else {
                u8::try_from(high).ok()?
            }
        }
        _ => return None,
    })
}
