//! Test lookups that read values straight from the C++ daemon sources the Rust crates mirror.
#![cfg_attr(
    test,
    expect(
        dead_code_pub_in_binary,
        reason = "the test build calls only `enumerators`; the crates that depend on this one call the rest"
    )
)]

/// One C++ source file and the path its lookups name when they fail.
#[derive(Debug)]
pub struct CppHeader {
    path: &'static str,
    text: &'static str,
}

pub const PACKETS: CppHeader = CppHeader {
    path: "daemon/airpods_packets.h",
    text: include_str!("../../../daemon/airpods_packets.h"),
};

pub const CONTROL: CppHeader = CppHeader {
    path: "daemon/BasicControlCommand.hpp",
    text: include_str!("../../../daemon/BasicControlCommand.hpp"),
};

pub const BATTERY: CppHeader = CppHeader {
    path: "daemon/battery.hpp",
    text: include_str!("../../../daemon/battery.hpp"),
};

pub const ENUMS: CppHeader = CppHeader {
    path: "daemon/enums.h",
    text: include_str!("../../../daemon/enums.h"),
};

pub const DEVICE_INFO: CppHeader = CppHeader {
    path: "daemon/deviceinfo.hpp",
    text: include_str!("../../../daemon/deviceinfo.hpp"),
};

pub const MEDIA_CONTROLLER: CppHeader = CppHeader {
    path: "daemon/media/mediacontroller.h",
    text: include_str!("../../../daemon/media/mediacontroller.h"),
};

pub const BLE_MANAGER: CppHeader = CppHeader {
    path: "daemon/ble/blemanager.h",
    text: include_str!("../../../daemon/ble/blemanager.h"),
};

impl CppHeader {
    /// The text from `scope` (such as `namespace Parse`) to its first closing brace, where every constant read here sits.
    #[must_use]
    pub fn scope(&self, scope: &str) -> &'static str {
        let (_, rest) = [" ", "\n", "{"]
            .iter()
            .find_map(|after| self.text.split_once(&format!("{scope}{after}")))
            .unwrap_or_else(|| missing(&format!("{} has no {scope}", self.path)));
        let (block, _) = rest
            .split_once('}')
            .unwrap_or_else(|| missing(&format!("{}: {scope} has no closing brace", self.path)));
        block
    }

    /// The text after `start` up to the next `end`, inside `scope` or anywhere when `scope` is `None`.
    #[must_use]
    pub fn find(&self, scope: Option<&str>, start: &str, end: char) -> &'static str {
        let (text, place) = match scope {
            Some(scope) => (self.scope(scope), scope),
            None => (self.text, "the file"),
        };
        let (_, rest) = text
            .split_once(start)
            .unwrap_or_else(|| missing(&format!("{} has no `{start}` in {place}", self.path)));
        let (found, _) = rest.split_once(end).unwrap_or_else(|| {
            missing(&format!(
                "{}: `{start}` in {place} has no `{end}`",
                self.path
            ))
        });
        found.trim()
    }

    /// The text after `start` up to the next `end`, for a block whose entries hold the characters `find` stops at.
    #[must_use]
    pub fn between(&self, start: &str, end: &str) -> &'static str {
        let (_, rest) = self
            .text
            .split_once(start)
            .unwrap_or_else(|| missing(&format!("{} has no `{start}`", self.path)));
        let (found, _) = rest
            .split_once(end)
            .unwrap_or_else(|| missing(&format!("{}: `{start}` has no `{end}`", self.path)));
        found
    }

    /// A byte written as `0x1B` or `3`, found between `start` and `end`.
    // Sample input: `using Type = BasicControlCommand<0x1B>;` read with start `BasicControlCommand<` and end `>`.
    #[must_use]
    pub fn byte(&self, scope: Option<&str>, start: &str, end: char) -> u8 {
        parse_byte(self.find(scope, start, end))
    }

    /// The bytes of `symbol = QByteArray::fromHex("...")`.
    // Sample input: static const QByteArray HANDSHAKE = QByteArray::fromHex("00000400010002000000000000000000");
    #[must_use]
    pub fn hex(&self, scope: Option<&str>, symbol: &str) -> Vec<u8> {
        let hex = self.find(scope, &format!("{symbol} = QByteArray::fromHex(\""), '"');
        hex.as_bytes()
            .chunks(2)
            .map(|pair| {
                std::str::from_utf8(pair)
                    .ok()
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                    .unwrap_or_else(|| {
                        missing(&format!("{}: {symbol} has bad hex `{hex}`", self.path))
                    })
            })
            .collect()
    }

    /// The frame `symbol = ControlCommand::createCommand(...)` builds, value bytes the call leaves out read as 0.
    // Sample input: static const QByteArray OFF = ControlCommand::createCommand(0x0D, 0x01);
    #[must_use]
    pub fn command(&self, scope: Option<&str>, symbol: &str) -> Vec<u8> {
        let args = self.find(
            scope,
            &format!("{symbol} = ControlCommand::createCommand("),
            ')',
        );
        let mut frame = CONTROL.hex(Some("namespace ControlCommand"), "HEADER");
        let mut bytes: Vec<u8> = args.split(',').map(parse_byte).collect();
        bytes.resize(COMMAND_BYTES, 0x00);
        frame.extend(bytes);
        frame
    }

    /// Every enumerator of `scope` in order with its number, counting on from the last explicit value as C++ does.
    // Sample input: `enum class LidState : uint8_t\n{\n OPEN = 0x0,\n CLOSED = 0x1, // note\n UNKNOWN,\n}`, one enumerator per line.
    #[must_use]
    pub fn enumerators(&self, scope: &str) -> Vec<(&'static str, u8)> {
        let (_, body) = self
            .scope(scope)
            .split_once('{')
            .unwrap_or_else(|| missing(&format!("{}: {scope} has no opening brace", self.path)));
        let mut next = Some(0_u8);
        let mut found = Vec::new();
        for line in body.lines() {
            let code = line.split_once("//").map_or(line, |(code, _)| code);
            let entry = code.trim().trim_end_matches(',');
            if entry.is_empty() {
                continue;
            }
            let (name, number) = match entry.split_once('=') {
                Some((name, value)) => (name.trim(), parse_byte(value)),
                None => (
                    entry,
                    next.unwrap_or_else(|| {
                        missing(&format!("{}: {scope}: `{entry}` passes 255", self.path))
                    }),
                ),
            };
            found.push((name, number));
            next = number.checked_add(1);
        }
        found
    }
}

/// The identifier and the four value bytes after `ControlCommand::HEADER`.
const COMMAND_BYTES: usize = 5;

// Sample input: `0x0D` or `0`.
fn parse_byte(text: &str) -> u8 {
    let text = text.trim();
    match text.strip_prefix("0x") {
        Some(hex) => u8::from_str_radix(hex, 16),
        None => text.parse(),
    }
    .unwrap_or_else(|_| missing(&format!("`{text}` is not a byte")))
}

/// Stops the test that asked for a C++ definition the sources no longer hold, naming it.
#[expect(
    clippy::panic,
    reason = "a failed lookup must stop the calling test and name the missing C++ definition"
)]
fn missing(what: &str) -> ! {
    panic!("{what}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerators_skip_the_underlying_type_and_trailing_comments() {
        assert_eq!(
            BLE_MANAGER.enumerators("enum class ConnectionState"),
            [
                ("DISCONNECTED", 0x00),
                ("IDLE", 0x04),
                ("MUSIC", 0x05),
                ("CALL", 0x06),
                ("RINGING", 0x07),
                ("HANGING_UP", 0x09),
                ("UNKNOWN", 0xFF),
            ]
        );
    }
}
