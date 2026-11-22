//! The dictation microphone stream: Opus frames in ATT notifications, read and built with no I/O.

use crate::att::{AttPdu, HANDLE_VALUE_NOTIFICATION, WRITE_COMMAND};
use crate::error::{ParseError, Parser};

// Every byte and layout here was seen between a Mac and one pair of AirPods, none from Linux.

/// Their meaning is unknown.
const AUDIO_MARKER: [u8; 2] = [0x01, 0x00];
/// Marker, packet counter and declared length: the part of the audio prefix in the value.
const AUDIO_VALUE_PREFIX_LEN: usize = 5;

/// Sent when a microphone session opens; that it marks the session active is a guess.
pub const SESSION_ACTIVE: [u8; 11] = [
    0x04, 0x00, 0x04, 0x00, 0x4b, 0x00, 0x03, 0x00, 0x03, 0x01, 0x03,
];
/// Sent at connect and after a microphone session closes.
pub const SESSION_IDLE: [u8; 11] = [
    0x04, 0x00, 0x04, 0x00, 0x4b, 0x00, 0x03, 0x00, 0x03, 0x00, 0x00,
];

/// ATT Write Command to `START_HANDLE` that asks the pods to start the stream.
pub const START: [u8; 5] = [WRITE_COMMAND, 0x07, 0x00, 0x01, 0x00];
/// ATT Write Command to `STOP_HANDLE` that asks the pods to stop the stream.
pub const STOP: [u8; 6] = [WRITE_COMMAND, 0x0a, 0x00, 0x01, 0x00, 0x00];
/// The pods' answer to `START`; nobody can read its value, so it is compared whole.
pub const STARTED: [u8; 15] = [
    0x1b, 0x10, 0x00, 0x01, 0x00, 0x02, 0x08, 0x00, 0x00, 0x80, 0xbf, 0x00, 0x00, 0x00, 0x00,
];
/// The pods' answer to `STOP`, compared whole for the same reason.
pub const STOPPED: [u8; 7] = [0x1b, 0x10, 0x00, 0x01, 0x00, 0x03, 0x00];

/// A value of one firmware, not of the protocol; check the attribute table before writing.
pub const START_HANDLE: u16 = 0x0007;
/// A value of one firmware, like `START_HANDLE`.
pub const STOP_HANDLE: u16 = 0x000a;
/// A value of one firmware, like `START_HANDLE`.
pub const AUDIO_HANDLE: u16 = 0x000d;
/// Where `STARTED` and `STOPPED` arrive; a value of one firmware, like `START_HANDLE`.
pub const STATUS_HANDLE: u16 = 0x0010;

/// Opcode, handle, marker, packet counter, declared length: where the frames begin.
pub const AUDIO_PREFIX_LEN: usize = 8;
/// Flag, level, frame counter and the length byte in front of every frame.
pub const FRAME_PREFIX_LEN: usize = 5;
/// Every frame seen opens with it: Opus configuration 23, CELT only, wideband, 20 ms, mono.
pub const TOC_CELT_WIDEBAND_20MS_MONO: u8 = 0xb8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationPacket<'a> {
    Audio(AudioNotification<'a>),
    Status(Status<'a>),
    /// A value on `AUDIO_HANDLE` without the `01 00` marker, kept whole; a short one is `TooShort`.
    Unknown {
        /// Everything after the handle.
        value: &'a [u8],
    },
}

impl<'a> DictationPacket<'a> {
    /// Sample input: `1b 0d 00 01 00 ...`, a notification on 0x000d (audio) or 0x0010 (status).
    pub fn parse(pdu: &'a [u8]) -> Result<Option<Self>, ParseError> {
        let (handle, value) = match AttPdu::parse(pdu) {
            Ok(AttPdu::Notification { handle, value }) => (handle, value),
            Err(
                cut @ ParseError::TooShort {
                    parser: Parser::AttNotification,
                    ..
                },
            ) => return Err(cut),
            // Any other PDU is not dictation; a malformed one is for its own reader to refuse.
            Ok(_) | Err(_) => return Ok(None),
        };
        match handle {
            AUDIO_HANDLE => match AudioNotification::parse(value, pdu.len())? {
                Some(audio) => Ok(Some(Self::Audio(audio))),
                None => Ok(Some(Self::Unknown { value })),
            },
            STATUS_HANDLE => Ok(Some(Self::Status(Status::parse(pdu, value)))),
            _ => Ok(None),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioNotification<'a> {
    /// Counts notifications and wraps at 256.
    pub packet_counter: u8,
    /// A mismatch with the real length of the frames is not an error.
    pub declared_len: u16,
    frames: &'a [u8],
}

impl<'a> AudioNotification<'a> {
    // `01 00 2a 08 00 ...`: marker, packet counter 0x2a, declared length 8 little endian, frames.
    fn parse(value: &'a [u8], pdu_len: usize) -> Result<Option<Self>, ParseError> {
        let Some(([marker @ .., packet_counter, len_low, len_high], frames)) =
            value.split_first_chunk::<AUDIO_VALUE_PREFIX_LEN>()
        else {
            return Err(ParseError::TooShort {
                parser: Parser::DictationAudio,
                need: AUDIO_PREFIX_LEN,
                got: pdu_len,
            });
        };
        if *marker != AUDIO_MARKER {
            return Ok(None);
        }
        Ok(Some(Self {
            packet_counter: *packet_counter,
            declared_len: u16::from_le_bytes([*len_low, *len_high]),
            frames,
        }))
    }

    /// Walked over the real bytes, whatever the declared length says.
    #[must_use]
    pub fn frames(&self) -> Frames<'a> {
        Frames { rest: self.frames }
    }

    #[must_use]
    pub fn length_matches(&self) -> bool {
        usize::from(self.declared_len) == self.frames.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    /// Handed out raw; that it flags voice activity is a guess.
    pub flag: u8,
    /// Handed out raw; that it is a signal level is a guess.
    pub level: u8,
    /// Counts frames, little endian on the wire.
    pub counter: u16,
    pub data: &'a [u8],
}

impl Frame<'_> {
    /// Whether the data starts with the one table of contents byte seen so far.
    #[must_use]
    pub fn is_expected_codec(&self) -> bool {
        self.data.first() == Some(&TOC_CELT_WIDEBAND_20MS_MONO)
    }
}

/// Stops at the first frame that is cut short.
#[derive(Debug, Clone)]
pub struct Frames<'a> {
    rest: &'a [u8],
}

impl Frames<'_> {
    /// Bytes not yielded yet; after `next` returns `None`, only bytes of no whole frame remain.
    #[must_use]
    pub fn trailing(&self) -> usize {
        self.rest.len()
    }
}

impl<'a> Iterator for Frames<'a> {
    type Item = Frame<'a>;

    // `80 d5 04 01 03 b8 10 11`: flag, level, counter 0x0104 little endian, length 3, codec bytes.
    fn next(&mut self) -> Option<Frame<'a>> {
        let ([flag, level, counter @ .., len], after_prefix) =
            self.rest.split_first_chunk::<FRAME_PREFIX_LEN>()?;
        let (data, rest) = after_prefix.split_at_checked(usize::from(*len))?;
        self.rest = rest;
        Some(Frame {
            flag: *flag,
            level: *level,
            counter: u16::from_le_bytes(*counter),
            data,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status<'a> {
    /// The whole notification equals `STARTED`.
    Started,
    /// The whole notification equals `STOPPED`.
    Stopped,
    /// Everything after the handle.
    Other(&'a [u8]),
}

impl<'a> Status<'a> {
    // `1b 10 00 01 00 03 00`: compared whole with the known replies; `value` follows the handle.
    fn parse(pdu: &[u8], value: &'a [u8]) -> Self {
        if pdu == STARTED {
            Self::Started
        } else if pdu == STOPPED {
            Self::Stopped
        } else {
            Self::Other(value)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    #[error(
        "{parser} cannot carry frame {index}: it is {len} bytes, the length byte holds at most {max}",
        parser = Parser::DictationAudio,
        max = u8::MAX
    )]
    FrameTooLong { index: usize, len: usize },
    #[error(
        "{parser} cannot carry {len} bytes of frames: the length field counts the frames and their prefixes, not the whole notification, and holds at most {max}",
        parser = Parser::DictationAudio,
        max = u16::MAX
    )]
    PacketTooLong { len: usize },
}

fn frame_len(index: usize, frame: &Frame<'_>) -> Result<u8, EncodeError> {
    let len = frame.data.len();
    u8::try_from(len).or(Err(EncodeError::FrameTooLong { index, len }))
}

/// Both limits are checked on the lengths before a byte is copied.
pub fn encode_audio(packet_counter: u8, frames: &[Frame<'_>]) -> Result<Vec<u8>, EncodeError> {
    let mut body_len = 0_usize;
    for (index, frame) in frames.iter().enumerate() {
        let data_len = usize::from(frame_len(index, frame)?);
        // Saturating, because a sum past `usize::MAX` is past the limit of the length field too.
        body_len = body_len
            .saturating_add(FRAME_PREFIX_LEN)
            .saturating_add(data_len);
    }
    let Ok(declared_len) = u16::try_from(body_len) else {
        return Err(EncodeError::PacketTooLong { len: body_len });
    };
    let mut pdu = Vec::with_capacity(AUDIO_PREFIX_LEN.saturating_add(body_len));
    pdu.push(HANDLE_VALUE_NOTIFICATION);
    pdu.extend_from_slice(&AUDIO_HANDLE.to_le_bytes());
    pdu.extend_from_slice(&AUDIO_MARKER);
    pdu.push(packet_counter);
    pdu.extend_from_slice(&declared_len.to_le_bytes());
    for (index, frame) in frames.iter().enumerate() {
        pdu.extend_from_slice(&[frame.flag, frame.level]);
        pdu.extend_from_slice(&frame.counter.to_le_bytes());
        pdu.push(frame_len(index, frame)?);
        pdu.extend_from_slice(frame.data);
    }
    Ok(pdu)
}

#[cfg(test)]
mod tests;
