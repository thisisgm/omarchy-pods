//! The dictation microphone stream: Opus frames in ATT notifications, read and built with no I/O.

use crate::att::{AttPdu, HANDLE_VALUE_NOTIFICATION};
use crate::doap::{Handles, STREAM_ID};
use crate::error::{ParseError, Parser};

// The audio, the score, the end and stop reason 6 were seen from one pair of AirPods; the other event codes are read from Apple's host software and were not.

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

/// The pods' answer to a start on the one firmware seen: a score of -1 on its event handle.
pub const STARTED: [u8; 15] = [
    0x1b, 0x10, 0x00, 0x01, 0x00, 0x02, 0x08, 0x00, 0x00, 0x80, 0xbf, 0x00, 0x00, 0x00, 0x00,
];
/// The pods' answer to a stop on the same firmware: the end of the stream on its event handle.
pub const STOPPED: [u8; 7] = [0x1b, 0x10, 0x00, 0x01, 0x00, 0x03, 0x00];

/// Stream identifier, code and data length, the part every event has.
const EVENT_FIXED_LEN: usize = 4;
/// The four bytes of the float a score event opens its data with.
const SCORE_LEN: usize = 4;
/// Stream identifier and reason, the whole of a stop notification's value.
const STOP_VALUE_LEN: usize = 3;
/// Siri was called, by voice or by a tap.
const EVENT_SIRI_ACTIVATION: u8 = 0x01;
/// A score the pods give a start; -1 answers a start the host asked for.
const EVENT_SCORE: u8 = 0x02;
const EVENT_STREAMING_END: u8 = 0x03;
const EVENT_SIRI_STATE: u8 = 0x04;
const EVENT_SIRI_CANCEL: u8 = 0x05;
/// A press or a release of the stem.
const EVENT_BUTTON: u8 = 0x06;

/// Opcode, handle, marker, packet counter, declared length: where the frames begin.
pub const AUDIO_PREFIX_LEN: usize = 8;
/// Flag, level, frame counter and the length byte in front of every frame.
pub const FRAME_PREFIX_LEN: usize = 5;
/// Every frame seen opens with it: Opus configuration 23, CELT only, wideband, 20 ms, mono.
pub const TOC_CELT_WIDEBAND_20MS_MONO: u8 = 0xb8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationPacket<'a> {
    Audio(AudioNotification<'a>),
    Event(Event),
    /// The pods ended or refused the stream, on its stop handle.
    Stopped {
        reason: u8,
    },
    /// A value on the audio, event or stop handle that is not of its layout, kept whole; a short audio one is `TooShort`.
    Unknown {
        handle: u16,
        value: &'a [u8],
    },
}

impl<'a> DictationPacket<'a> {
    /// Sample input: `1b 0d 00 01 00 ...`, a notification on the audio, event or stop handle of `handles`.
    pub fn parse(pdu: &'a [u8], handles: &Handles) -> Result<Option<Self>, ParseError> {
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
        let unknown = Self::Unknown { handle, value };
        if handle == handles.send_data {
            let audio = AudioNotification::parse(value, pdu.len())?;
            Ok(Some(audio.map_or(unknown, Self::Audio)))
        } else if handle == handles.event {
            Ok(Some(Event::parse(value).map_or(unknown, Self::Event)))
        } else if handle == handles.stop {
            Ok(Some(
                stop_reason(value).map_or(unknown, |reason| Self::Stopped { reason }),
            ))
        } else {
            Ok(None)
        }
    }
}

// Sample input: `01 00 06`, the stream and the reason and nothing more; 6 refuses a start.
fn stop_reason(value: &[u8]) -> Option<u8> {
    let [stream_low, stream_high, reason] = <[u8; STOP_VALUE_LEN]>::try_from(value).ok()?;
    (u16::from_le_bytes([stream_low, stream_high]) == STREAM_ID).then_some(reason)
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
pub enum Event {
    /// The four bytes of the float, never computed with.
    Score {
        bits: u32,
    },
    StreamingEnd,
    SiriActivation(u8),
    SiriState(u8),
    SiriCancel,
    Button(u8),
    Other {
        code: u8,
    },
}

impl Event {
    /// The score of -1 that answers a start the host asked for.
    pub const STARTED: Self = Self::Score {
        bits: (-1.0_f32).to_bits(),
    };

    // Sample input: `01 00 02 08 00 00 80 bf 00 00 00 00`, stream 1, code 2, 8 data bytes whose first four are the score.
    fn parse(value: &[u8]) -> Option<Self> {
        let ([stream_low, stream_high, code, len], rest) =
            value.split_first_chunk::<EVENT_FIXED_LEN>()?;
        if u16::from_le_bytes([*stream_low, *stream_high]) != STREAM_ID {
            return None;
        }
        let data = rest.get(..usize::from(*len))?;
        let first = data.first().copied();
        match *code {
            EVENT_SIRI_ACTIVATION => first.map(Self::SiriActivation),
            EVENT_SCORE => {
                let bits = data
                    .first_chunk::<SCORE_LEN>()
                    .copied()
                    .map(u32::from_le_bytes);
                bits.map(|bits| Self::Score { bits })
            }
            EVENT_STREAMING_END => Some(Self::StreamingEnd),
            EVENT_SIRI_STATE => first.map(Self::SiriState),
            EVENT_SIRI_CANCEL => Some(Self::SiriCancel),
            EVENT_BUTTON => first.map(Self::Button),
            code => Some(Self::Other { code }),
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
pub fn encode_audio(
    handles: &Handles,
    packet_counter: u8,
    frames: &[Frame<'_>],
) -> Result<Vec<u8>, EncodeError> {
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
    pdu.extend_from_slice(&handles.send_data.to_le_bytes());
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
