//! The dictation microphone stream: Opus frames in ATT notifications, read and built with no I/O.

use crate::att::{AttPdu, HANDLE_VALUE_NOTIFICATION};
use crate::error::{ParseError, Parser};

// Every byte and layout here was observed between a Mac and one pair of AirPods; none is confirmed from a Linux host.

/// An audio value starts with these two bytes; their meaning is unknown.
const AUDIO_MARKER: [u8; 2] = [0x01, 0x00];
/// The marker, the packet counter and the declared length: the part of the audio prefix that sits in the value.
const AUDIO_VALUE_PREFIX_LEN: usize = 5;

/// Accessory channel packet the host sends when a microphone session opens; that it marks the session active is a guess.
pub const SESSION_ACTIVE: [u8; 11] = [
    0x04, 0x00, 0x04, 0x00, 0x4b, 0x00, 0x03, 0x00, 0x03, 0x01, 0x03,
];
/// Accessory channel packet the host sends at connect and after a microphone session closes.
pub const SESSION_IDLE: [u8; 11] = [
    0x04, 0x00, 0x04, 0x00, 0x4b, 0x00, 0x03, 0x00, 0x03, 0x00, 0x00,
];

/// ATT Write Command to `START_HANDLE` that asks the pods to start the stream.
pub const START: [u8; 5] = [0x52, 0x07, 0x00, 0x01, 0x00];
/// ATT Write Command to `STOP_HANDLE` that asks the pods to stop the stream.
pub const STOP: [u8; 6] = [0x52, 0x0a, 0x00, 0x01, 0x00, 0x00];
/// The notification the pods answer `START` with; nobody can read its value, so it is compared whole.
pub const STARTED: [u8; 15] = [
    0x1b, 0x10, 0x00, 0x01, 0x00, 0x02, 0x08, 0x00, 0x00, 0x80, 0xbf, 0x00, 0x00, 0x00, 0x00,
];
/// The notification the pods answer `STOP` with, compared whole for the same reason.
pub const STOPPED: [u8; 7] = [0x1b, 0x10, 0x00, 0x01, 0x00, 0x03, 0x00];

/// Handle `START` is written to: a value of one firmware, not of the protocol; check the pods' attribute table before writing, and where it differs nothing in this module applies.
pub const START_HANDLE: u16 = 0x0007;
/// Handle `STOP` is written to, a value of one firmware like `START_HANDLE`.
pub const STOP_HANDLE: u16 = 0x000a;
/// Handle the audio notifications arrive on, a value of one firmware like `START_HANDLE`.
pub const AUDIO_HANDLE: u16 = 0x000d;
/// Handle the replies to `START` and `STOP` arrive on, a value of one firmware like `START_HANDLE`.
pub const STATUS_HANDLE: u16 = 0x0010;

/// Opcode, handle, marker, packet counter and declared length: where the frames of an audio notification begin.
pub const AUDIO_PREFIX_LEN: usize = 8;
/// Flag, level, frame counter and the length byte in front of every frame.
pub const FRAME_PREFIX_LEN: usize = 5;
/// The Opus table of contents byte of every frame seen: configuration 23, CELT only, wideband, 20 ms, mono.
pub const TOC_CELT_WIDEBAND_20MS_MONO: u8 = 0xb8;

/// One notification of the dictation stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationPacket<'a> {
    Audio(AudioNotification<'a>),
    Status(Status<'a>),
    /// A notification on `AUDIO_HANDLE` of at least `AUDIO_PREFIX_LEN` bytes whose value does not start with the audio marker `01 00`, kept whole so the caller can log or count it; a shorter one is `TooShort` whatever its marker.
    Unknown {
        /// Everything after the handle.
        value: &'a [u8],
    },
}

impl<'a> DictationPacket<'a> {
    /// Reads one PDU of the ATT channel; `Ok(None)` is anything but a notification on `AUDIO_HANDLE` or `STATUS_HANDLE`, which is ordinary traffic.
    // 1b 0d 00 01 00 ...: a notification as `AttPdu::parse` reads it, on handle 0x000d (audio) or 0x0010 (status), then the value.
    pub fn parse(pdu: &'a [u8]) -> Result<Option<Self>, ParseError> {
        let (handle, value) = match AttPdu::parse(pdu) {
            Ok(AttPdu::Notification { handle, value }) => (handle, value),
            Err(
                cut @ ParseError::TooShort {
                    parser: Parser::AttNotification,
                    ..
                },
            ) => return Err(cut),
            // A PDU of any other kind is not dictation, and a malformed one is for its own reader to refuse.
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

/// An audio notification: a packet counter, a declared length, then the frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioNotification<'a> {
    /// Counts notifications and wraps at 256.
    pub packet_counter: u8,
    /// The length the packet declares for its frames; a mismatch with the real length is not an error.
    pub declared_len: u16,
    frames: &'a [u8],
}

impl<'a> AudioNotification<'a> {
    // 01 00 2a 08 00 ...: the marker, packet counter 0x2a, declared length 8 little endian, then the frames; `None` is another marker.
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

    /// The frames of this notification, in order, walked over the real bytes whatever the declared length says.
    #[must_use]
    pub fn frames(&self) -> Frames<'a> {
        Frames { rest: self.frames }
    }

    /// Whether the frames take as many bytes as the packet declares.
    #[must_use]
    pub fn length_matches(&self) -> bool {
        usize::from(self.declared_len) == self.frames.len()
    }
}

/// One coded audio frame and the four prefix bytes sent in front of its length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    /// The first prefix byte, handed out raw; that it flags voice activity is a guess.
    pub flag: u8,
    /// The second prefix byte, handed out raw; that it is a signal level is a guess.
    pub level: u8,
    /// Counts frames, little endian on the wire.
    pub counter: u16,
    pub data: &'a [u8],
}

impl Frame<'_> {
    /// Whether the data starts with the one table of contents byte seen so far; an empty frame does not.
    #[must_use]
    pub fn is_expected_codec(&self) -> bool {
        self.data.first() == Some(&TOC_CELT_WIDEBAND_20MS_MONO)
    }
}

/// Walks the frames of one audio notification and stops at the first one that is cut short.
#[derive(Debug, Clone)]
pub struct Frames<'a> {
    rest: &'a [u8],
}

impl Frames<'_> {
    /// The bytes not yielded yet, whole frames included; only once `next` returns `None` is it the bytes that hold no whole frame.
    #[must_use]
    pub fn trailing(&self) -> usize {
        self.rest.len()
    }
}

impl<'a> Iterator for Frames<'a> {
    type Item = Frame<'a>;

    // 80 d5 04 01 03 b8 10 11: flag 0x80, level 0xd5, counter 0x0104 little endian, length 3, then that many codec bytes.
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

/// What the pods said on the status handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status<'a> {
    /// The whole notification equals `STARTED`.
    Started,
    /// The whole notification equals `STOPPED`.
    Stopped,
    /// Any other value on the status handle: everything after the handle.
    Other(&'a [u8]),
}

impl<'a> Status<'a> {
    // 1b 10 00 01 00 03 00: the whole notification is compared with the two known replies; `value` is what follows the handle.
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

/// Why an audio notification could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    /// The frame at `index` is longer than its one length byte can say.
    #[error(
        "{parser} cannot carry frame {index}: it is {len} bytes, the length byte holds at most {max}",
        parser = Parser::DictationAudio,
        max = u8::MAX
    )]
    FrameTooLong { index: usize, len: usize },
    /// `len` counts what the 16 bit length field must hold, the frames and their prefixes, not the whole notification.
    #[error(
        "{parser} cannot carry {len} bytes of frames: the length field counts the frames and their prefixes, not the whole notification, and holds at most {max}",
        parser = Parser::DictationAudio,
        max = u16::MAX
    )]
    PacketTooLong { len: usize },
}

/// Builds the audio notification the pods would send for these frames.
pub fn encode_audio(packet_counter: u8, frames: &[Frame<'_>]) -> Result<Vec<u8>, EncodeError> {
    let mut body = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
        let Ok(len) = u8::try_from(frame.data.len()) else {
            return Err(EncodeError::FrameTooLong {
                index,
                len: frame.data.len(),
            });
        };
        body.extend_from_slice(&[frame.flag, frame.level]);
        body.extend_from_slice(&frame.counter.to_le_bytes());
        body.push(len);
        body.extend_from_slice(frame.data);
    }
    let Ok(declared_len) = u16::try_from(body.len()) else {
        return Err(EncodeError::PacketTooLong { len: body.len() });
    };
    let mut pdu = vec![HANDLE_VALUE_NOTIFICATION];
    pdu.extend_from_slice(&AUDIO_HANDLE.to_le_bytes());
    pdu.extend_from_slice(&AUDIO_MARKER);
    pdu.push(packet_counter);
    pdu.extend_from_slice(&declared_len.to_le_bytes());
    pdu.extend_from_slice(&body);
    Ok(pdu)
}

#[cfg(test)]
mod tests;
