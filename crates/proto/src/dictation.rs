//! The dictation microphone stream: Opus frames in ATT notifications, read and built with no I/O.

use crate::error::{ParseError, Parser};

// Every byte and layout here was observed between a Mac and one pair of AirPods; none is confirmed from a Linux host.

/// Opcode of an ATT Handle Value Notification.
const NOTIFICATION: u8 = 0x1b;
/// The attribute handle is a little endian u16 right after the opcode.
const HANDLE_LEN: usize = 2;
/// Opcode plus the handle, the shortest notification.
const NOTIFICATION_MIN_LEN: usize = 3;
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

/// Handle `START` is written to: a value of one firmware, not of the protocol, so check the attribute table before writing.
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
    /// A notification on `AUDIO_HANDLE` whose value does not start with the audio marker `01 00`, kept whole so the caller can log or count it.
    Unknown {
        /// Everything after the handle.
        value: &'a [u8],
    },
}

impl<'a> DictationPacket<'a> {
    /// Reads one PDU of the ATT channel; `Ok(None)` is any other opcode or handle, which is ordinary traffic.
    // 1b 0d 00 01 00 ...: notification opcode, handle little endian (0x000d audio, 0x0010 status), then the value.
    pub fn parse(pdu: &'a [u8]) -> Result<Option<Self>, ParseError> {
        let Some(after_opcode) = pdu.strip_prefix(&[NOTIFICATION]) else {
            return Ok(None);
        };
        let Some((handle, value)) = after_opcode.split_first_chunk::<HANDLE_LEN>() else {
            return Err(ParseError::TooShort {
                parser: Parser::Dictation,
                need: NOTIFICATION_MIN_LEN,
                got: pdu.len(),
            });
        };
        match u16::from_le_bytes(*handle) {
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

    /// The frames of this notification, in order.
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
    /// The frames and their prefixes take `len` bytes together, more than the two length bytes can say.
    #[error(
        "{parser} cannot carry {len} bytes of frames: the length field holds at most {max}",
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
    let mut pdu = vec![NOTIFICATION];
    pdu.extend_from_slice(&AUDIO_HANDLE.to_le_bytes());
    pdu.extend_from_slice(&AUDIO_MARKER);
    pdu.push(packet_counter);
    pdu.extend_from_slice(&declared_len.to_le_bytes());
    pdu.extend_from_slice(&body);
    Ok(pdu)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opcode, audio handle, marker, packet counter 0x2a, declared length 8, then one frame: flag 0x80, level 0xd5, counter 0x0104, length 3, three codec bytes.
    const ONE_FRAME_NOTIFICATION: &str = "1b 0d 00 01 00 2a 08 00 80 d5 04 01 03 b8 10 11";
    /// The two values a flag byte is known to take.
    const FLAGS: [u8; 2] = [0x80, 0x00];
    /// Level byte of the first generated frame; later frames count up from it.
    const FIRST_LEVEL: u8 = 0xb9;
    /// Offset of the low handle byte in an ATT PDU; the high byte follows it.
    const HANDLE_AT: usize = 1;
    /// Offset of the packet counter in an audio notification.
    const PACKET_COUNTER_AT: usize = 5;
    /// Offset of the low byte of the declared length in an audio notification.
    const DECLARED_LEN_AT: usize = 6;
    /// How many data bytes of the last frame the truncation test keeps.
    const KEPT_DATA_BYTES: usize = 3;
    /// The longest frame one length byte can say.
    const LONGEST_FRAME: usize = 255;
    /// The most frame bytes the two length bytes can say.
    const LONGEST_BODY: usize = 65535;
    /// How many longest frames, 260 bytes each with the prefix, fit under `LONGEST_BODY` with 15 bytes to spare.
    const FULL_FRAMES: usize = 252;
    /// Data bytes of the last frame that bring the body to exactly `LONGEST_BODY`.
    const LAST_DATA_AT_THE_LIMIT: usize = 10;
    /// Data bytes of the last frame that bring the body one byte past `LONGEST_BODY`.
    const LAST_DATA_PAST_THE_LIMIT: usize = 11;

    // Sample input: "1b 0d 00", hex bytes separated by one space.
    fn hex(text: &str) -> Vec<u8> {
        text.split(' ')
            .map(|byte| u8::from_str_radix(byte, 16).unwrap())
            .collect()
    }

    // Sample output for (0x10, 3): b8 10 11, the table of contents byte then counting bytes that stand in for one coded frame.
    fn codec_bytes(seed: u8, len: usize) -> Vec<u8> {
        let counting = std::iter::successors(Some(seed), |byte| Some(byte.wrapping_add(1)));
        std::iter::once(TOC_CELT_WIDEBAND_20MS_MONO)
            .chain(counting)
            .take(len)
            .collect()
    }

    /// One frame per payload, counted from `first_counter` and wrapping, with alternating flags and rising levels so every prefix differs.
    fn frames(first_counter: u16, payloads: &[Vec<u8>]) -> Vec<Frame<'_>> {
        let counters = std::iter::successors(Some(first_counter), |c| Some(c.wrapping_add(1)));
        payloads
            .iter()
            .zip(counters)
            .zip(FIRST_LEVEL..)
            .zip(FLAGS.into_iter().cycle())
            .map(|(((data, counter), level), flag)| Frame {
                flag,
                level,
                counter,
                data,
            })
            .collect()
    }

    /// The bytes one frame takes on the wire: flag, level, counter, length, data.
    fn frame_record(frame: Frame<'_>) -> Vec<u8> {
        encode_audio(0, &[frame])
            .unwrap()
            .split_off(AUDIO_PREFIX_LEN)
    }

    /// Three whole frames followed by the first `kept` wire bytes of a fourth.
    fn notification_cut_into_the_last_frame(whole: &[Frame<'_>; 4], kept: usize) -> Vec<u8> {
        let [first, second, third, last] = *whole;
        let mut pdu = encode_audio(0, &[first, second, third]).unwrap();
        pdu.extend_from_slice(&frame_record(last)[..kept]);
        pdu
    }

    /// One frame with the flag and level bytes of `ONE_FRAME_NOTIFICATION`.
    fn frame(counter: u16, data: &[u8]) -> Frame<'_> {
        Frame {
            flag: 0x80,
            level: 0xd5,
            counter,
            data,
        }
    }

    fn four_payloads() -> [Vec<u8>; 4] {
        [
            codec_bytes(0x00, 74),
            codec_bytes(0x40, 80),
            codec_bytes(0x80, 81),
            codec_bytes(0xc0, 94),
        ]
    }

    fn audio(pdu: &[u8]) -> AudioNotification<'_> {
        match DictationPacket::parse(pdu) {
            Ok(Some(DictationPacket::Audio(audio))) => audio,
            other => panic!("not an audio notification: {other:?}"),
        }
    }

    // Sample input: 52 07 00 01 00, whose handle is the little endian 0x0007 after the opcode.
    fn handle_of(pdu: &[u8]) -> u16 {
        u16::from_le_bytes([pdu[HANDLE_AT], pdu[NOTIFICATION_MIN_LEN - 1]])
    }

    #[test]
    fn start_stop_and_markers_are_the_documented_bytes() {
        assert_eq!(
            SESSION_ACTIVE.to_vec(),
            hex("04 00 04 00 4b 00 03 00 03 01 03")
        );
        assert_eq!(
            SESSION_IDLE.to_vec(),
            hex("04 00 04 00 4b 00 03 00 03 00 00")
        );
        assert_eq!(START.to_vec(), hex("52 07 00 01 00"));
        assert_eq!(STOP.to_vec(), hex("52 0a 00 01 00 00"));
        assert_eq!(
            STARTED.to_vec(),
            hex("1b 10 00 01 00 02 08 00 00 80 bf 00 00 00 00")
        );
        assert_eq!(STOPPED.to_vec(), hex("1b 10 00 01 00 03 00"));
    }

    #[test]
    fn every_pdu_carries_its_handle_constant() {
        let audio = encode_audio(0, &[]).unwrap();
        for (pdu, handle) in [
            (&START[..], START_HANDLE),
            (&STOP[..], STOP_HANDLE),
            (&STARTED[..], STATUS_HANDLE),
            (&STOPPED[..], STATUS_HANDLE),
            (&audio[..], AUDIO_HANDLE),
        ] {
            assert_eq!(handle_of(pdu), handle, "{pdu:02x?}");
        }
    }

    #[test]
    fn the_two_replies_parse_as_started_and_stopped() {
        assert_eq!(
            DictationPacket::parse(&STARTED),
            Ok(Some(DictationPacket::Status(Status::Started)))
        );
        assert_eq!(
            DictationPacket::parse(&STOPPED),
            Ok(Some(DictationPacket::Status(Status::Stopped)))
        );
    }

    #[test]
    fn a_reply_with_one_byte_changed_is_other() {
        let other = |pdu: &[u8]| {
            let expected = Status::Other(&pdu[NOTIFICATION_MIN_LEN..]);
            assert_eq!(
                DictationPacket::parse(pdu),
                Ok(Some(DictationPacket::Status(expected))),
                "{pdu:02x?}"
            );
        };
        for reply in [&STARTED[..], &STOPPED[..]] {
            for at in NOTIFICATION_MIN_LEN..reply.len() {
                let mut changed = reply.to_vec();
                changed[at] ^= 0x01;
                other(&changed);
            }
            other(&reply[..reply.len() - 1]);
            let mut one_byte_more = reply.to_vec();
            one_byte_more.push(0x00);
            other(&one_byte_more);
            other(&reply[..NOTIFICATION_MIN_LEN]);
        }
    }

    #[test]
    fn accepts_an_audio_notification() {
        let data = codec_bytes(0x10, 81);
        let frame = frame(0x0104, &data);
        let pdu = encode_audio(0x2a, &[frame]).unwrap();
        assert_eq!(pdu.len(), 94);
        let audio = audio(&pdu);
        assert_eq!(audio.packet_counter, 0x2a);
        assert_eq!(audio.declared_len, 86);
        assert!(audio.length_matches());
        let mut seen = audio.frames();
        assert_eq!(seen.by_ref().collect::<Vec<_>>(), [frame]);
        assert_eq!(seen.trailing(), 0);
    }

    #[test]
    fn demuxes_every_frame_of_a_burst_packet() {
        let payloads = [
            codec_bytes(0x00, 74),
            codec_bytes(0x40, 80),
            codec_bytes(0x80, 81),
            codec_bytes(0xc0, 94),
            Vec::new(),
        ];
        let whole = frames(4, &payloads);
        assert_eq!(
            whole.iter().map(|f| f.counter).collect::<Vec<_>>(),
            [4, 5, 6, 7, 8]
        );
        let pdu = encode_audio(0x07, &whole).unwrap();
        assert_eq!(
            pdu[PACKET_COUNTER_AT..AUDIO_PREFIX_LEN],
            [0x07, 0x62, 0x01],
            "counter, then 354 little endian"
        );
        let audio = audio(&pdu);
        assert_eq!(audio.declared_len, 354);
        assert!(audio.length_matches());
        let mut seen = audio.frames();
        assert_eq!(seen.by_ref().collect::<Vec<_>>(), whole);
        assert_eq!(seen.trailing(), 0);
    }

    #[test]
    fn stops_on_a_truncated_frame() {
        let payloads = four_payloads();
        let whole: [Frame<'_>; 4] = frames(0, &payloads).try_into().unwrap();
        let pdu = notification_cut_into_the_last_frame(&whole, FRAME_PREFIX_LEN + KEPT_DATA_BYTES);
        let seen: Vec<_> = audio(&pdu).frames().collect();
        assert_eq!(seen, whole[..3]);
    }

    #[test]
    fn trailing_bytes_are_counted() {
        let payloads = four_payloads();
        let whole: [Frame<'_>; 4] = frames(0, &payloads).try_into().unwrap();
        let last_record_len = frame_record(whole[3]).len();
        for kept in 0..last_record_len {
            let pdu = notification_cut_into_the_last_frame(&whole, kept);
            let mut seen = audio(&pdu).frames();
            assert_eq!(seen.by_ref().count(), 3, "kept {kept}");
            assert_eq!(seen.trailing(), kept, "kept {kept}");
        }
        let mut hostile = hex(ONE_FRAME_NOTIFICATION);
        hostile.extend(hex("80 d5 05 01 ff b8 10"));
        let mut seen = audio(&hostile).frames();
        assert_eq!(seen.by_ref().count(), 1);
        assert_eq!(seen.trailing(), 7);
    }

    #[test]
    fn trailing_before_the_end_counts_the_frames_not_yet_yielded() {
        let payloads = four_payloads();
        let whole = frames(0, &payloads);
        let record_lens: Vec<usize> = whole.iter().map(|f| frame_record(*f).len()).collect();
        let pdu = encode_audio(0, &whole).unwrap();
        let mut seen = audio(&pdu).frames();
        for yielded in 0..=whole.len() {
            let not_yielded: usize = record_lens[yielded..].iter().sum();
            assert_eq!(seen.trailing(), not_yielded, "after {yielded} frames");
            assert_eq!(seen.next(), whole.get(yielded).copied());
        }
        assert_eq!(seen.trailing(), 0);
    }

    #[test]
    fn emits_nothing_from_a_prefix_only_notification() {
        let pdu = encode_audio(0x2a, &[]).unwrap();
        assert_eq!(pdu, hex("1b 0d 00 01 00 2a 00 00"));
        assert_eq!(pdu.len(), AUDIO_PREFIX_LEN);
        let audio = audio(&pdu);
        assert_eq!(audio.declared_len, 0);
        assert!(audio.length_matches());
        let mut seen = audio.frames();
        assert_eq!(seen.next(), None);
        assert_eq!(seen.trailing(), 0);
    }

    #[test]
    fn declared_length_is_kept_beside_the_real_one() {
        let pdu = hex(ONE_FRAME_NOTIFICATION);
        let whole = audio(&pdu);
        assert_eq!(whole.declared_len, 8);
        assert!(whole.length_matches());
        let one_byte_lost = audio(&pdu[..pdu.len() - 1]);
        assert_eq!(one_byte_lost.packet_counter, 0x2a);
        assert_eq!(one_byte_lost.declared_len, 8);
        assert!(!one_byte_lost.length_matches());
        let mut frames = one_byte_lost.frames();
        assert_eq!(frames.next(), None);
        assert_eq!(frames.trailing(), 7);
        let length_only = hex("1b 0d 00 01 00 2a 02 01");
        let empty = audio(&length_only);
        assert_eq!(empty.declared_len, 0x0102);
        assert!(!empty.length_matches());
    }

    #[test]
    fn a_notification_on_another_handle_is_not_dictation() {
        let audio = hex(ONE_FRAME_NOTIFICATION);
        for handle in [
            START_HANDLE,
            STOP_HANDLE,
            0x000c,
            0x000e,
            0x0d00,
            0x010d,
            0x1000,
            0x0110,
        ] {
            let mut pdu = audio.clone();
            pdu[HANDLE_AT..NOTIFICATION_MIN_LEN].copy_from_slice(&handle.to_le_bytes());
            assert_eq!(DictationPacket::parse(&pdu), Ok(None), "{handle:#06x}");
        }
        assert_eq!(DictationPacket::parse(&[]), Ok(None));
    }

    #[test]
    fn a_write_command_is_not_dictation() {
        let mut write_to_the_audio_handle = hex(ONE_FRAME_NOTIFICATION);
        write_to_the_audio_handle[0] = 0x52;
        let mut indication_on_the_status_handle = STARTED.to_vec();
        indication_on_the_status_handle[0] = 0x1d;
        for pdu in [
            &START[..],
            &STOP[..],
            &SESSION_ACTIVE,
            &SESSION_IDLE,
            &write_to_the_audio_handle,
            &indication_on_the_status_handle,
            &[0x52],
        ] {
            assert_eq!(DictationPacket::parse(pdu), Ok(None), "{pdu:02x?}");
        }
    }

    #[test]
    fn a_short_audio_notification_is_too_short() {
        let audio = hex(ONE_FRAME_NOTIFICATION);
        let unknown_marker = hex("1b 0d 00 02 00 2a 08");
        for pdu in [
            &audio[..3],
            &audio[..4],
            &audio[..5],
            &audio[..6],
            &audio[..7],
            &unknown_marker,
        ] {
            let error = DictationPacket::parse(pdu).unwrap_err();
            let expected = ParseError::TooShort {
                parser: Parser::DictationAudio,
                need: 8,
                got: pdu.len(),
            };
            assert_eq!(error, expected, "{pdu:02x?}");
        }
        assert_eq!(
            DictationPacket::parse(&audio[..7]).unwrap_err().to_string(),
            "dictation audio notification too short: need 8 bytes, got 7"
        );
    }

    #[test]
    fn a_notification_cut_before_its_handle_is_too_short() {
        for pdu in [&[0x1b][..], &[0x1b, 0x0d], &[0x1b, 0x10]] {
            let error = DictationPacket::parse(pdu).unwrap_err();
            let expected = ParseError::TooShort {
                parser: Parser::Dictation,
                need: 3,
                got: pdu.len(),
            };
            assert_eq!(error, expected, "{pdu:02x?}");
        }
        assert_eq!(
            DictationPacket::parse(&[0x1b, 0x0d])
                .unwrap_err()
                .to_string(),
            "dictation packet too short: need 3 bytes, got 2"
        );
    }

    #[test]
    fn an_audio_handle_value_with_another_prefix_is_unknown() {
        for pdu in [
            hex("1b 0d 00 02 00 2a 00 00"),
            hex("1b 0d 00 01 01 2a 00 00"),
            hex("1b 0d 00 00 01 2a 08 00 80 d5 04 01 03 b8 10 11"),
        ] {
            let expected = DictationPacket::Unknown {
                value: &pdu[NOTIFICATION_MIN_LEN..],
            };
            assert_eq!(
                DictationPacket::parse(&pdu),
                Ok(Some(expected)),
                "{pdu:02x?}"
            );
        }
    }

    #[test]
    fn is_expected_codec_reads_the_first_data_byte() {
        assert!(frame(0, &[0xb8]).is_expected_codec());
        assert!(frame(0, &[0xb8, 0x00]).is_expected_codec());
        assert!(!frame(0, &[0xb9]).is_expected_codec());
        assert!(!frame(0, &[0x00, 0xb8]).is_expected_codec());
        assert!(!frame(0, &[]).is_expected_codec());
    }

    #[test]
    fn packet_counter_wraps_at_256_in_the_encoder_input() {
        let data = codec_bytes(0x10, 3);
        let last_before_the_wrap = u8::MAX;
        let first_after_the_wrap = last_before_the_wrap.wrapping_add(1);
        assert_eq!(first_after_the_wrap, 0);
        for (packet_counter, frame_counter) in
            [(last_before_the_wrap, u16::MAX), (first_after_the_wrap, 0)]
        {
            let frame = frame(frame_counter, &data);
            let pdu = encode_audio(packet_counter, &[frame]).unwrap();
            assert_eq!(pdu[PACKET_COUNTER_AT], packet_counter);
            let audio = audio(&pdu);
            assert_eq!(audio.packet_counter, packet_counter);
            assert_eq!(audio.frames().collect::<Vec<_>>(), [frame]);
        }
    }

    #[test]
    fn encode_then_parse_round_trips() {
        let data = codec_bytes(0x10, 3);
        let one_frame = frame(0x0104, &data);
        assert_eq!(
            encode_audio(0x2a, &[one_frame]),
            Ok(hex(ONE_FRAME_NOTIFICATION))
        );
        let literal = hex(ONE_FRAME_NOTIFICATION);
        let parsed = audio(&literal);
        assert_eq!(parsed.packet_counter, 0x2a);
        assert_eq!(parsed.frames().collect::<Vec<_>>(), [one_frame]);

        let payloads = [
            codec_bytes(0x00, 1),
            codec_bytes(0x07, LONGEST_FRAME),
            Vec::new(),
        ];
        let whole = frames(u16::MAX - 1, &payloads);
        assert_eq!(
            whole.iter().map(|f| f.counter).collect::<Vec<_>>(),
            [u16::MAX - 1, u16::MAX, 0]
        );
        let pdu = encode_audio(u8::MAX, &whole).unwrap();
        assert_eq!(
            pdu[DECLARED_LEN_AT..AUDIO_PREFIX_LEN],
            [0x0f, 0x01],
            "271 little endian"
        );
        let parsed = audio(&pdu);
        assert_eq!(parsed.packet_counter, u8::MAX);
        assert_eq!(parsed.declared_len, 271);
        assert!(parsed.length_matches());
        let mut seen = parsed.frames();
        assert_eq!(seen.by_ref().collect::<Vec<_>>(), whole);
        assert_eq!(seen.trailing(), 0);
    }

    #[test]
    fn a_256_byte_frame_cannot_be_encoded() {
        let longest = codec_bytes(0x00, LONGEST_FRAME);
        let too_long = codec_bytes(0x00, LONGEST_FRAME + 1);
        assert!(encode_audio(0, &[frame(0, &longest)]).is_ok());
        let error = encode_audio(0, &[frame(0, &longest), frame(1, &too_long)]).unwrap_err();
        assert_eq!(error, EncodeError::FrameTooLong { index: 1, len: 256 });
        assert_eq!(
            error.to_string(),
            "dictation audio notification cannot carry frame 1: it is 256 bytes, the length byte holds at most 255"
        );
    }

    #[test]
    fn a_packet_over_65535_bytes_cannot_be_encoded() {
        let longest = codec_bytes(0x00, LONGEST_FRAME);
        let at_the_limit = codec_bytes(0x00, LAST_DATA_AT_THE_LIMIT);
        let past_the_limit = codec_bytes(0x00, LAST_DATA_PAST_THE_LIMIT);
        let mut frames = vec![frame(0, &longest); FULL_FRAMES];
        frames.push(frame(0, &at_the_limit));
        let pdu = encode_audio(0, &frames).unwrap();
        assert_eq!(pdu.len(), AUDIO_PREFIX_LEN + LONGEST_BODY);
        let largest = audio(&pdu);
        assert_eq!(largest.declared_len, u16::MAX);
        assert!(largest.length_matches());
        assert_eq!(largest.frames().collect::<Vec<_>>(), frames);

        frames.pop();
        frames.push(frame(0, &past_the_limit));
        let error = encode_audio(0, &frames).unwrap_err();
        assert_eq!(error, EncodeError::PacketTooLong { len: 65536 });
        assert_eq!(
            error.to_string(),
            "dictation audio notification cannot carry 65536 bytes of frames: the length field holds at most 65535"
        );
    }
}
