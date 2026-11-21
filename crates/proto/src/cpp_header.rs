//! Test lookups that read byte values straight from the C++ daemon headers this crate mirrors.

/// One C++ header and the path its lookups name when they fail.
pub(crate) struct CppHeader {
    path: &'static str,
    text: &'static str,
}

pub(crate) const PACKETS: CppHeader = CppHeader {
    path: "daemon/airpods_packets.h",
    text: include_str!("../../../daemon/airpods_packets.h"),
};

pub(crate) const CONTROL: CppHeader = CppHeader {
    path: "daemon/BasicControlCommand.hpp",
    text: include_str!("../../../daemon/BasicControlCommand.hpp"),
};

pub(crate) const BATTERY: CppHeader = CppHeader {
    path: "daemon/battery.hpp",
    text: include_str!("../../../daemon/battery.hpp"),
};

pub(crate) const ENUMS: CppHeader = CppHeader {
    path: "daemon/enums.h",
    text: include_str!("../../../daemon/enums.h"),
};

pub(crate) const DEVICE_INFO: CppHeader = CppHeader {
    path: "daemon/deviceinfo.hpp",
    text: include_str!("../../../daemon/deviceinfo.hpp"),
};

impl CppHeader {
    /// The text from `scope` (such as `namespace Parse`) to its first closing brace, where every constant read here sits.
    pub(crate) fn scope(&self, scope: &str) -> &'static str {
        let (_, rest) = [" ", "\n", "{"]
            .iter()
            .find_map(|after| self.text.split_once(&format!("{scope}{after}")))
            .unwrap_or_else(|| panic!("{} has no {scope}", self.path));
        let (block, _) = rest
            .split_once('}')
            .unwrap_or_else(|| panic!("{}: {scope} has no closing brace", self.path));
        block
    }

    /// The text after `start` up to the next `end`, inside `scope` or anywhere when `scope` is `None`.
    pub(crate) fn find(&self, scope: Option<&str>, start: &str, end: char) -> &'static str {
        let (text, place) = match scope {
            Some(scope) => (self.scope(scope), scope),
            None => (self.text, "the file"),
        };
        let (_, rest) = text
            .split_once(start)
            .unwrap_or_else(|| panic!("{} has no `{start}` in {place}", self.path));
        let (found, _) = rest
            .split_once(end)
            .unwrap_or_else(|| panic!("{}: `{start}` in {place} has no `{end}`", self.path));
        found.trim()
    }

    /// The text after `start` up to the next `end`, for a block whose entries hold the characters `find` stops at.
    pub(crate) fn between(&self, start: &str, end: &str) -> &'static str {
        let (_, rest) = self
            .text
            .split_once(start)
            .unwrap_or_else(|| panic!("{} has no `{start}`", self.path));
        let (found, _) = rest
            .split_once(end)
            .unwrap_or_else(|| panic!("{}: `{start}` has no `{end}`", self.path));
        found
    }

    /// A byte written as `0x1B` or `3`, found between `start` and `end`.
    // Sample input: `using Type = BasicControlCommand<0x1B>;` read with start `BasicControlCommand<` and end `>`.
    pub(crate) fn byte(&self, scope: Option<&str>, start: &str, end: char) -> u8 {
        parse_byte(self.find(scope, start, end))
    }

    /// The bytes of `symbol = QByteArray::fromHex("...")`.
    // Sample input: static const QByteArray HANDSHAKE = QByteArray::fromHex("00000400010002000000000000000000");
    pub(crate) fn hex(&self, scope: Option<&str>, symbol: &str) -> Vec<u8> {
        let hex = self.find(scope, &format!("{symbol} = QByteArray::fromHex(\""), '"');
        hex.as_bytes()
            .chunks(2)
            .map(|pair| {
                std::str::from_utf8(pair)
                    .ok()
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                    .unwrap_or_else(|| panic!("{}: {symbol} has bad hex `{hex}`", self.path))
            })
            .collect()
    }

    /// The frame `symbol = ControlCommand::createCommand(...)` builds, value bytes the call leaves out read as 0.
    // Sample input: static const QByteArray OFF = ControlCommand::createCommand(0x0D, 0x01);
    pub(crate) fn command(&self, scope: Option<&str>, symbol: &str) -> Vec<u8> {
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
    .unwrap_or_else(|_| panic!("`{text}` is not a byte"))
}
