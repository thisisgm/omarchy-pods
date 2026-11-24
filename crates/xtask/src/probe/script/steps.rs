//! A step file: the packets and waits of one planned sitting, read whole before anything is sent.

use std::fmt;
use std::str::SplitAsciiWhitespace;

use pods_proto::att;
use pods_proto::dictation;
use pods_proto::doap::Handles;
use pods_proto::packet::{OPCODE_CONTROL, opcode_of};
use pods_proto::sdulog::{self, LogError};
use pods_proto::uplink;

use super::{Action, Event, MAX_MINUTES, MS_PER_MINUTE, at};
use crate::probe::dictate;

/// A step file holds at most this many lines, comments and blank lines included.
const MAX_LINES: usize = 256;
/// The longest value or packet one step sends: the longest attribute value (Bluetooth Core Specification 6.1, Vol 3, Part F, 3.2.9).
const MAX_VALUE_BYTES: usize = 512;
/// The longest single wait, chosen here: far past any pause between two packets of a plan.
const MAX_WAIT_MS: u32 = 60_000;
/// The waits of one file add up to at most the longest run `--minutes` asks for.
const MAX_RUN_MS: u32 = MAX_MINUTES.saturating_mul(MS_PER_MINUTE);
/// Bytes in a kibibyte.
const BYTES_PER_KIB: u64 = 1024;
/// The file is refused above this many bytes, chosen here: `MAX_LINES` lines of the longest step fit with room for comments.
pub(crate) const MAX_FILE_BYTES: u64 = 512 * BYTES_PER_KIB;
/// A handle is written `0x` and this many lowercase hex digits, as the marks print it.
const HANDLE_DIGITS: usize = 4;
/// The lowest attribute handle; 0x0000 is reserved (Vol 3, Part F, 3.2.2).
const FIRST_HANDLE: u16 = 0x0001;
/// The most characters the text of a cue holds, chosen here: one line a person reads at a glance.
const MAX_CUE_CHARS: usize = 120;
/// Each packet a guard of the run covers on the control channel, and the action that sends it under that guard.
const GUARDED_CONTROL: [(&[u8], Action); 4] = [
    (&uplink::START, Action::Start),
    (&uplink::STOP, Action::Stop),
    (&dictation::SESSION_ACTIVE, Action::MarkActive),
    (&dictation::SESSION_IDLE, Action::MarkIdle),
];
/// Header, subtype, the declared length 9 and the message 0x0100: the bytes every uplink START begins with.
const START_PREFIX: [u8; 12] = {
    let [prefix @ .., _, _, _, _, _, _, _] = uplink::START;
    prefix
};
/// The same for the attribute channel.
const GUARDED_ATT: [(&[u8], Action); 2] = [
    (&dictate::START, Action::DictationStart),
    (&dictate::STOP, Action::DictationStop),
];
/// Where the run's own start write goes, on the one firmware seen.
const START_HANDLE: u16 = Handles::OBSERVED.start;
/// Where its stop write goes.
const STOP_HANDLE: u16 = Handles::OBSERVED.stop;

/// What a step sends on the attribute channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AttStep {
    WriteRequest { handle: u16, value: Vec<u8> },
    WriteCommand { handle: u16, value: Vec<u8> },
    Read { handle: u16 },
}

impl AttStep {
    /// The PDU as it leaves, as `12 0e 00 01 00` writes `01 00` to 0x000e.
    pub(crate) fn pdu(&self) -> Vec<u8> {
        let (opcode, handle, value) = match self {
            Self::WriteRequest { handle, value } => (att::WRITE_REQUEST, handle, value.as_slice()),
            Self::WriteCommand { handle, value } => (att::WRITE_COMMAND, handle, value.as_slice()),
            Self::Read { handle } => (att::READ_REQUEST, handle, [].as_slice()),
        };
        [&[opcode][..], &handle.to_le_bytes(), value].concat()
    }
}

/// One packet of a step file that no guard covers, sent as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StepPacket {
    Att(AttStep),
    Control(Vec<u8>),
}

impl fmt::Display for StepPacket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Att(AttStep::WriteRequest { handle, .. }) => {
                write!(f, "write-request {handle:#06x}")
            }
            Self::Att(AttStep::WriteCommand { handle, .. }) => {
                write!(f, "write-command {handle:#06x}")
            }
            Self::Att(AttStep::Read { handle }) => write!(f, "read {handle:#06x}"),
            Self::Control(_) => f.write_str("control"),
        }
    }
}

/// An uplink START that asks for its own codec id, frames and last field, sent under the uplink's guard as `uplink::START` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UplinkStart {
    codec: u32,
    frames: u8,
    last: u16,
}

impl UplinkStart {
    // Sample input: `04 00 04 00 58 00 00 00 09 00 00 01 | 01 01 00 00 | 04 | 96 00`, `START_PREFIX` then codec id 257 u32 LE, frames u8 and a u16 LE.
    fn parse(packet: &[u8]) -> Option<Self> {
        let packet: [u8; uplink::START.len()] = packet.try_into().ok()?;
        let [prefix @ .., c0, c1, c2, c3, frames, l0, l1] = packet;
        (prefix == START_PREFIX).then_some(Self {
            codec: u32::from_le_bytes([c0, c1, c2, c3]),
            frames,
            last: u16::from_le_bytes([l0, l1]),
        })
    }

    pub(crate) fn packet(self) -> Vec<u8> {
        let codec = self.codec.to_le_bytes();
        let last = self.last.to_le_bytes();
        [START_PREFIX.as_slice(), &codec, &[self.frames], &last].concat()
    }
}

impl fmt::Display for UplinkStart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "uplink start codec {}", self.codec)
    }
}

/// Why a line was refused; it never repeats the line, whose bytes may identify a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Fault {
    #[error("the file has more than {MAX_LINES} lines")]
    TooManyLines,
    #[error("the line is not UTF-8 text")]
    NotText,
    #[error(
        "the line is no step; the steps are write-request, write-command, read, control, wait and cue with a text of at most {MAX_CUE_CHARS} characters between double quotes, and a comment starts with #"
    )]
    UnknownStep,
    #[error("{step} takes {takes} and nothing more")]
    Fields {
        step: &'static str,
        takes: &'static str,
    },
    #[error("the handle is not 0x and {HANDLE_DIGITS} lowercase hex digits")]
    BadHandle,
    #[error("handle 0x0000 is reserved; handles run from {FIRST_HANDLE:#06x} to 0xffff")]
    HandleOutOfRange,
    #[error("byte {byte} of the hex, counting from 0, is not two lowercase hex digits")]
    BadHex { byte: usize },
    #[error("the value is {len} bytes, and a step sends at most {MAX_VALUE_BYTES}")]
    ValueTooLong { len: usize },
    #[error("the wait is not a whole number of milliseconds")]
    BadWait,
    #[error("the wait is longer than {MAX_WAIT_MS} ms")]
    WaitTooLong,
    #[error("the waits up to this line pass {MAX_RUN_MS} ms in all")]
    RunTooLong,
    #[error(
        "a write to handle {START_HANDLE:#06x} or {STOP_HANDLE:#06x} may only be the stream's own start or stop write, which the run stops itself"
    )]
    GuardedHandle,
    #[error(
        "a control command, opcode {OPCODE_CONTROL:#06x}, changes a device setting the run cannot restore, so no step may send one without --allow-control"
    )]
    SettingCommand,
    #[error(
        "a packet with the opcode of the uplink start or stop or of a session marker may only be one of those exact packets or an uplink start with any codec id, frames and last field, which the run undoes itself"
    )]
    GuardedPacket,
}

/// A refused line of a step file, counting from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {fault}")]
pub(crate) struct StepsError {
    pub(crate) line: usize,
    pub(crate) fault: Fault,
}

// Sample input: "# arm, then start\nwrite-request 0x0010 0100010101\nwait 80\ncontrol 040004004b000300030103\nwrite-command 0x0007 0100\nwait 2000\n"; a step fires at the sum of the waits above it, and the run ends at the last.
pub(crate) fn parse_steps(text: &[u8], allow_control: bool) -> Result<Vec<Event>, StepsError> {
    let text = text.strip_suffix(b"\n").unwrap_or(text);
    let mut at_ms = 0_u32;
    let mut events = Vec::new();
    for (index, line) in text.split(|byte| *byte == b'\n').enumerate() {
        let line_number = index.saturating_add(1);
        let refused = |fault| StepsError {
            line: line_number,
            fault,
        };
        if line_number > MAX_LINES {
            return Err(refused(Fault::TooManyLines));
        }
        let line = std::str::from_utf8(line).map_err(|_not_text| refused(Fault::NotText))?;
        match parse_line(line, line_number, allow_control).map_err(refused)? {
            Line::Blank => {}
            Line::Wait(ms) => {
                at_ms = at_ms
                    .checked_add(ms)
                    .filter(|total| *total <= MAX_RUN_MS)
                    .ok_or(refused(Fault::RunTooLong))?;
            }
            Line::Send(action) => events.push(at(at_ms, action)),
        }
    }
    events.push(at(at_ms, Action::End));
    Ok(events)
}

/// What one line asks for.
enum Line {
    Blank,
    Wait(u32),
    Send(Action),
}

// Sample input: `write-request 0x000e 0100`, a step, then its fields separated by spaces.
fn parse_line(text: &str, line: usize, allow_control: bool) -> Result<Line, Fault> {
    let mut words = text.split_ascii_whitespace();
    let Some(step) = words.next() else {
        return Ok(Line::Blank);
    };
    if step.starts_with('#') {
        return Ok(Line::Blank);
    }
    let action = match step {
        "write-request" => {
            let [handle, value] = take(words).ok_or(fields("write-request", WRITE_TAKES))?;
            let (handle, value) = (parse_handle(handle)?, parse_value(value)?);
            att_action(line, AttStep::WriteRequest { handle, value })?
        }
        "write-command" => {
            let [handle, value] = take(words).ok_or(fields("write-command", WRITE_TAKES))?;
            let (handle, value) = (parse_handle(handle)?, parse_value(value)?);
            att_action(line, AttStep::WriteCommand { handle, value })?
        }
        "read" => {
            let [handle] = take(words).ok_or(fields("read", "a handle"))?;
            let handle = parse_handle(handle)?;
            att_action(line, AttStep::Read { handle })?
        }
        "control" => {
            let [packet] = take(words).ok_or(fields("control", "one packet in hex"))?;
            control_action(line, parse_value(packet)?, allow_control)?
        }
        "wait" => {
            let [ms] = take(words).ok_or(fields("wait", "a number of milliseconds"))?;
            return parse_wait(ms).map(Line::Wait);
        }
        "cue" => Action::Hold(parse_cue(text)?),
        _ => return Err(Fault::UnknownStep),
    };
    Ok(Line::Send(action))
}

/// What the two writes take after their name.
const WRITE_TAKES: &str = "a handle and a hex value";

fn fields(step: &'static str, takes: &'static str) -> Fault {
    Fault::Fields { step, takes }
}

/// Exactly `N` more fields, or none when there are fewer or more.
fn take<const N: usize>(mut words: SplitAsciiWhitespace<'_>) -> Option<[&str; N]> {
    let taken: Vec<&str> = words.by_ref().take(N).collect();
    let fields = <[&str; N]>::try_from(taken).ok()?;
    words.next().is_none().then_some(fields)
}

// Sample input: `0x000e`, read as handle 14; uppercase, a sign and 0x0000 are refused.
fn parse_handle(text: &str) -> Result<u16, Fault> {
    let digits = text.strip_prefix("0x").ok_or(Fault::BadHandle)?;
    if digits.len() != HANDLE_DIGITS {
        return Err(Fault::BadHandle);
    }
    let bytes = sdulog::parse_hex(digits).map_err(|_bad_hex| Fault::BadHandle)?;
    let pair = <[u8; 2]>::try_from(bytes).map_err(|_not_two| Fault::BadHandle)?;
    let handle = u16::from_be_bytes(pair);
    if handle < FIRST_HANDLE {
        return Err(Fault::HandleOutOfRange);
    }
    Ok(handle)
}

// Sample input: `0100`, two lowercase hex digits for each of 1 to MAX_VALUE_BYTES bytes, as the channel logs write them.
fn parse_value(text: &str) -> Result<Vec<u8>, Fault> {
    let value = sdulog::parse_hex(text).map_err(|error| match error {
        LogError::BadHex { byte } => Fault::BadHex { byte },
        LogError::TooLong { len } => Fault::ValueTooLong { len },
        LogError::Empty | LogError::MissingField | LogError::BadStamp | LogError::BadDirection => {
            Fault::BadHex { byte: 0 }
        }
    })?;
    if value.len() > MAX_VALUE_BYTES {
        return Err(Fault::ValueTooLong { len: value.len() });
    }
    Ok(value)
}

// Sample input: `80`, a decimal number of milliseconds up to MAX_WAIT_MS; a sign is refused.
fn parse_wait(text: &str) -> Result<u32, Fault> {
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Fault::BadWait);
    }
    let ms: u32 = text.parse().map_err(|_overflow| Fault::WaitTooLong)?;
    if ms > MAX_WAIT_MS {
        return Err(Fault::WaitTooLong);
    }
    Ok(ms)
}

// Sample input: `cue "controller: restore now"`, the whole line: one space, then up to MAX_CUE_CHARS characters between double quotes, none of them a quote or a control character.
fn parse_cue(line: &str) -> Result<String, Fault> {
    let quoted = line.trim_ascii().strip_prefix("cue \"");
    let text = quoted.and_then(|rest| rest.strip_suffix('"'));
    let text = text.ok_or(Fault::UnknownStep)?;
    let plain = !text.chars().any(|c| c == '"' || c.is_control());
    if plain && text.chars().count() <= MAX_CUE_CHARS {
        Ok(text.to_owned())
    } else {
        Err(Fault::UnknownStep)
    }
}

/// A packet a guard covers goes out under that guard; any other write to the stream's two handles is refused.
fn att_action(line: usize, step: AttStep) -> Result<Action, Fault> {
    let pdu = step.pdu();
    let guarded = GUARDED_ATT
        .into_iter()
        .find(|(packet, _)| *packet == pdu.as_slice());
    if let Some((_, action)) = guarded {
        return Ok(action);
    }
    match &step {
        AttStep::WriteRequest { handle, .. } | AttStep::WriteCommand { handle, .. }
            if [START_HANDLE, STOP_HANDLE].contains(handle) =>
        {
            Err(Fault::GuardedHandle)
        }
        AttStep::WriteRequest { .. } | AttStep::WriteCommand { .. } | AttStep::Read { .. } => {
            Ok(Action::Step {
                line,
                packet: StepPacket::Att(step),
            })
        }
    }
}

/// The same on the control channel, where a control command is refused unless `allow_control`, and any other uplink START goes out under the uplink's guard.
fn control_action(line: usize, packet: Vec<u8>, allow_control: bool) -> Result<Action, Fault> {
    let guarded = GUARDED_CONTROL
        .into_iter()
        .find(|(known, _)| *known == packet.as_slice());
    if let Some((_, action)) = guarded {
        return Ok(action);
    }
    let opcode = opcode_of(&packet);
    if opcode == Some(OPCODE_CONTROL) && !allow_control {
        return Err(Fault::SettingCommand);
    }
    if let Some(start) = UplinkStart::parse(&packet) {
        return Ok(Action::StartCodec(start));
    }
    let mut known = GUARDED_CONTROL.iter();
    if opcode.is_some() && known.any(|(known, _)| opcode_of(known) == opcode) {
        return Err(Fault::GuardedPacket);
    }
    let packet = StepPacket::Control(packet);
    Ok(Action::Step { line, packet })
}

#[cfg(test)]
mod tests;
