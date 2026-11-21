//! The microphone uplink: opcode 0x58 packets on the control channel, read and built with no I/O.

use crate::control::{ControlCommand, ControlId};
use crate::error::{HEADER_LEN, ParseError, Parser};

// From librepods-org/librepods pull request 655 (aacp.rs, aacp_audio.rs); the C++ has no uplink.

const HEADER: [u8; HEADER_LEN] = [0x04, 0x00, 0x04, 0x00, 0x58, 0x00];
/// A little endian u16 right after the header.
const SUBTYPE_LEN: usize = 2;
/// Header plus the subtype, the shortest uplink packet.
const MIN_LEN: usize = 8;
/// Subtype of START and STOP.
const SUBTYPE_CONTROL: u16 = 0x0000;
const SUBTYPE_AUDIO: u16 = 0x0001;
/// A little endian u16 right after the subtype of a control packet.
const DECLARED_LEN_BYTES: usize = 2;
/// Header, subtype and the declared length.
const CONTROL_MIN_LEN: usize = 10;

/// Header, subtype and the opaque bytes: where the access units of an audio SDU begin.
pub const AUDIO_HEADER_LEN: usize = 22;
/// The stamp and the length byte in front of every access unit.
pub const UNIT_PREFIX_LEN: usize = 5;
/// Bytes 8 to 21 of an audio SDU, which no known code reads.
pub const OPAQUE_LEN: usize = 14;

pub const START: [u8; 19] = [
    0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x01, 0x82, 0x00, 0x00, 0x00,
    0x04, 0x96, 0x00,
];

pub const STOP: [u8; 12] = [
    0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x00, 0x00, 0x02, 0x00, 0x03, 0x01,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UplinkPacket<'a> {
    Audio(AudioSdu<'a>),
    Control(UplinkControl<'a>),
    /// An undocumented subtype, kept whole so the caller can log or count it.
    Unknown {
        subtype: u16,
        /// Everything after the subtype.
        body: &'a [u8],
    },
}

impl<'a> UplinkPacket<'a> {
    /// Sample input: `04 00 04 00 58 00 01 00 ...`, subtype 1 audio or 0 control, then the body.
    pub fn parse(sdu: &'a [u8]) -> Result<Option<Self>, ParseError> {
        let Some(after_header) = sdu.strip_prefix(&HEADER) else {
            return Ok(None);
        };
        let Some((subtype, body)) = after_header.split_first_chunk::<SUBTYPE_LEN>() else {
            return Err(ParseError::TooShort {
                parser: Parser::Uplink,
                need: MIN_LEN,
                got: sdu.len(),
            });
        };
        match u16::from_le_bytes(*subtype) {
            SUBTYPE_AUDIO => AudioSdu::parse(body, sdu.len()).map(|audio| Some(Self::Audio(audio))),
            SUBTYPE_CONTROL => {
                UplinkControl::parse(body, sdu.len()).map(|control| Some(Self::Control(control)))
            }
            subtype => Ok(Some(Self::Unknown { subtype, body })),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioSdu<'a> {
    /// Bytes 8 to 21, handed out raw.
    pub opaque: &'a [u8; OPAQUE_LEN],
    units: &'a [u8],
}

impl<'a> AudioSdu<'a> {
    // `a0 a1 a2 ... ad 10 20 30 40 02 00 00`: 14 opaque bytes, then the access units.
    fn parse(body: &'a [u8], sdu_len: usize) -> Result<Self, ParseError> {
        let Some((opaque, units)) = body.split_first_chunk::<OPAQUE_LEN>() else {
            return Err(ParseError::TooShort {
                parser: Parser::UplinkAudio,
                need: AUDIO_HEADER_LEN,
                got: sdu_len,
            });
        };
        Ok(Self { opaque, units })
    }

    #[must_use]
    pub fn units(&self) -> Units<'a> {
        Units { rest: self.units }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessUnit<'a> {
    /// Little endian on the wire; its unit is unknown.
    pub stamp: u32,
    pub data: &'a [u8],
}

/// Stops at the first access unit that is cut short.
#[derive(Debug, Clone)]
pub struct Units<'a> {
    rest: &'a [u8],
}

impl Units<'_> {
    /// The bytes not yielded yet; once `next` returns `None`, the bytes that hold no whole unit.
    #[must_use]
    pub fn trailing(&self) -> usize {
        self.rest.len()
    }
}

impl<'a> Iterator for Units<'a> {
    type Item = AccessUnit<'a>;

    // `10 20 30 40 02 00 00`: stamp 0x40302010 little endian, length 2, then the unit bytes.
    fn next(&mut self) -> Option<AccessUnit<'a>> {
        let ([stamp @ .., len], after_prefix) = self.rest.split_first_chunk::<UNIT_PREFIX_LEN>()?;
        let (data, rest) = after_prefix.split_at_checked(usize::from(*len))?;
        self.rest = rest;
        Some(AccessUnit {
            stamp: u32::from_le_bytes(*stamp),
            data,
        })
    }
}

/// The shape of START, STOP and the reply the pods are reported to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UplinkControl<'a> {
    /// Bytes 8 and 9; reading them as the payload length is a guess, so a mismatch is not an error.
    pub declared_len: u16,
    /// Everything after the declared length, whatever that length says.
    pub payload: &'a [u8],
}

impl<'a> UplinkControl<'a> {
    // `03 00 01 01 01`: declared length 3 little endian, then the payload.
    fn parse(body: &'a [u8], sdu_len: usize) -> Result<Self, ParseError> {
        let Some((declared_len, payload)) = body.split_first_chunk::<DECLARED_LEN_BYTES>() else {
            return Err(ParseError::TooShort {
                parser: Parser::UplinkControl,
                need: CONTROL_MIN_LEN,
                got: sdu_len,
            });
        };
        Ok(Self {
            declared_len: u16::from_le_bytes(*declared_len),
            payload,
        })
    }

    #[must_use]
    pub fn length_matches(&self) -> bool {
        usize::from(self.declared_len) == self.payload.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    #[error(
        "{parser} cannot carry access unit {index}: it is {len} bytes, the length byte holds at most {max}",
        parser = Parser::UplinkAudio,
        max = u8::MAX
    )]
    UnitTooLong { index: usize, len: usize },
}

pub fn encode_audio(
    opaque: &[u8; OPAQUE_LEN],
    units: &[AccessUnit<'_>],
) -> Result<Vec<u8>, EncodeError> {
    let mut sdu = HEADER.to_vec();
    sdu.extend_from_slice(&SUBTYPE_AUDIO.to_le_bytes());
    sdu.extend_from_slice(opaque);
    for (index, unit) in units.iter().enumerate() {
        let Ok(len) = u8::try_from(unit.data.len()) else {
            return Err(EncodeError::UnitTooLong {
                index,
                len: unit.data.len(),
            });
        };
        sdu.extend_from_slice(&unit.stamp.to_le_bytes());
        sdu.push(len);
        sdu.extend_from_slice(unit.data);
    }
    Ok(sdu)
}

/// Numbered as the control command carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MicSide {
    Automatic = 0,
    Right = 1,
    Left = 2,
}

impl MicSide {
    #[must_use]
    pub fn command(self) -> ControlCommand {
        let wire = match self {
            Self::Automatic => 0x00,
            Self::Right => 0x01,
            Self::Left => 0x02,
        };
        ControlCommand {
            id: ControlId::MIC_SIDE,
            value: [wire, 0, 0, 0],
        }
    }
}

impl TryFrom<u8> for MicSide {
    type Error = ParseError;

    // 0 Automatic, 1 Right, 2 Left: the first value byte of a MIC_SIDE control command.
    fn try_from(value: u8) -> Result<Self, ParseError> {
        match value {
            0 => Ok(Self::Automatic),
            1 => Ok(Self::Right),
            2 => Ok(Self::Left),
            _ => Err(ParseError::UnknownValue {
                parser: Parser::Control,
                field: "microphone side",
                value,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::ControlId;
    use crate::fixture::hex;

    /// Stands in for header bytes 8 to 21, whose real meaning nobody knows.
    const OPAQUE: [u8; OPAQUE_LEN] = [
        0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab, 0xac, 0xad,
    ];
    /// The shortest coded unit, which decodes to silence.
    const SILENT_UNIT: [u8; 2] = [0x00, 0x00];
    /// The eight byte gate, `OPAQUE`, then one unit: stamp 0x40302010, length 2, the silent unit.
    const ONE_UNIT_SDU: &str =
        "04 00 04 00 58 00 01 00 a0 a1 a2 a3 a4 a5 a6 a7 a8 a9 aa ab ac ad 10 20 30 40 02 00 00";
    /// Offset of the low subtype byte, 1 in an audio SDU and 0 in a control packet.
    const SUBTYPE_AT: usize = 6;
    const KEPT_DATA_BYTES: usize = 3;

    // Sample output for (0x10, 3): `10 11 12`, counting bytes that stand in for one coded unit.
    fn unit_bytes(first: u8, len: usize) -> Vec<u8> {
        std::iter::successors(Some(first), |byte| Some(byte.wrapping_add(1)))
            .take(len)
            .collect()
    }

    /// Stamped from 0x01020304 upward so every stamp byte differs.
    fn units(payloads: &[Vec<u8>]) -> Vec<AccessUnit<'_>> {
        (0x0102_0304_u32..)
            .zip(payloads)
            .map(|(stamp, data)| AccessUnit { stamp, data })
            .collect()
    }

    /// The bytes one unit takes on the wire: stamp, length, data.
    fn unit_record(unit: AccessUnit<'_>) -> Vec<u8> {
        encode_audio(&OPAQUE, &[unit])
            .unwrap()
            .split_off(AUDIO_HEADER_LEN)
    }

    fn sdu_cut_into_the_last_unit(whole: &[AccessUnit<'_>; 4], kept: usize) -> Vec<u8> {
        let [first, second, third, last] = *whole;
        let mut sdu = encode_audio(&OPAQUE, &[first, second, third]).unwrap();
        sdu.extend_from_slice(&unit_record(last)[..kept]);
        sdu
    }

    fn four_payloads() -> [Vec<u8>; 4] {
        [
            unit_bytes(0x00, 68),
            unit_bytes(0x40, 74),
            unit_bytes(0x80, 78),
            unit_bytes(0xc0, 70),
        ]
    }

    fn audio(sdu: &[u8]) -> AudioSdu<'_> {
        match UplinkPacket::parse(sdu) {
            Ok(Some(UplinkPacket::Audio(audio))) => audio,
            other => panic!("not an audio SDU: {other:?}"),
        }
    }

    fn control(sdu: &[u8]) -> UplinkControl<'_> {
        match UplinkPacket::parse(sdu) {
            Ok(Some(UplinkPacket::Control(control))) => control,
            other => panic!("not an uplink control packet: {other:?}"),
        }
    }

    #[test]
    fn start_and_stop_are_the_documented_bytes() {
        assert_eq!(
            START.to_vec(),
            hex("04 00 04 00 58 00 00 00 09 00 00 01 82 00 00 00 04 96 00")
        );
        assert_eq!(STOP.to_vec(), hex("04 00 04 00 58 00 00 00 02 00 03 01"));
    }

    #[test]
    fn accepts_an_audio_sdu() {
        let sdu = hex(ONE_UNIT_SDU);
        let audio = audio(&sdu);
        assert_eq!(audio.opaque, &OPAQUE);
        let seen: Vec<_> = audio.units().collect();
        let expected = AccessUnit {
            stamp: 0x4030_2010,
            data: &SILENT_UNIT,
        };
        assert_eq!(seen, [expected]);
    }

    #[test]
    fn rejects_the_control_subtype_as_audio() {
        let mut sdu = hex(ONE_UNIT_SDU);
        sdu[SUBTYPE_AT] = 0x00;
        let expected = UplinkControl {
            declared_len: 0xa1a0,
            payload: &sdu[CONTROL_MIN_LEN..],
        };
        assert_eq!(
            UplinkPacket::parse(&sdu),
            Ok(Some(UplinkPacket::Control(expected)))
        );
    }

    #[test]
    fn foreign_and_short_frames_are_not_uplink() {
        let ear = hex("04 00 04 00 06 00 00 01");
        let odd_second_byte = hex("04 01 04 00 58 00 01 00");
        let odd_opcode_high_byte = hex("04 00 04 00 58 01 01 00");
        for foreign in [
            &[][..],
            &HEADER[..5],
            &ear,
            &odd_second_byte,
            &odd_opcode_high_byte,
        ] {
            assert_eq!(UplinkPacket::parse(foreign), Ok(None), "{foreign:02x?}");
        }
        let too_short = |parser, need, got| Err(ParseError::TooShort { parser, need, got });
        let audio = hex(ONE_UNIT_SDU);
        for (sdu, expected) in [
            (&HEADER[..], too_short(Parser::Uplink, 8, 6)),
            (&audio[..7], too_short(Parser::Uplink, 8, 7)),
            (&audio[..8], too_short(Parser::UplinkAudio, 22, 8)),
            (&audio[..21], too_short(Parser::UplinkAudio, 22, 21)),
            (&START[..8], too_short(Parser::UplinkControl, 10, 8)),
            (&START[..9], too_short(Parser::UplinkControl, 10, 9)),
        ] {
            assert_eq!(UplinkPacket::parse(sdu), expected, "{sdu:02x?}");
        }
    }

    #[test]
    fn demuxes_every_access_unit() {
        let payloads = [
            unit_bytes(0x00, 68),
            unit_bytes(0x40, 74),
            unit_bytes(0x80, 78),
            Vec::new(),
        ];
        let whole = units(&payloads);
        let sdu = encode_audio(&OPAQUE, &whole).unwrap();
        let mut seen = audio(&sdu).units();
        assert_eq!(seen.by_ref().collect::<Vec<_>>(), whole);
        assert_eq!(seen.trailing(), 0);
    }

    #[test]
    fn stops_on_a_truncated_access_unit() {
        let payloads = four_payloads();
        let whole: [AccessUnit<'_>; 4] = units(&payloads).try_into().unwrap();
        let sdu = sdu_cut_into_the_last_unit(&whole, UNIT_PREFIX_LEN + KEPT_DATA_BYTES);
        let seen: Vec<_> = audio(&sdu).units().collect();
        assert_eq!(seen, whole[..3]);
    }

    #[test]
    fn trailing_bytes_are_counted() {
        let payloads = four_payloads();
        let whole: [AccessUnit<'_>; 4] = units(&payloads).try_into().unwrap();
        let last_record_len = unit_record(whole[3]).len();
        for kept in 0..last_record_len {
            let sdu = sdu_cut_into_the_last_unit(&whole, kept);
            let mut seen = audio(&sdu).units();
            assert_eq!(seen.by_ref().count(), 3, "kept {kept}");
            assert_eq!(seen.trailing(), kept, "kept {kept}");
        }
        let mut hostile = hex(ONE_UNIT_SDU);
        hostile.extend(hex("00 00 00 00 ff 01 02"));
        let mut seen = audio(&hostile).units();
        assert_eq!(seen.by_ref().count(), 1);
        assert_eq!(seen.trailing(), 7);
    }

    #[test]
    fn emits_nothing_from_a_header_only_sdu() {
        let sdu = encode_audio(&OPAQUE, &[]).unwrap();
        assert_eq!(sdu.len(), AUDIO_HEADER_LEN);
        let mut seen = audio(&sdu).units();
        assert_eq!(seen.next(), None);
        assert_eq!(seen.trailing(), 0);
    }

    #[test]
    fn control_reply_keeps_declared_and_real_length() {
        let reply = hex("04 00 04 00 58 00 00 00 03 00 01 01 01");
        let whole = control(&reply);
        assert_eq!(whole.declared_len, 3);
        assert_eq!(whole.payload, [0x01, 0x01, 0x01]);
        assert!(whole.length_matches());
        let one_byte_lost = control(&reply[..12]);
        assert_eq!(one_byte_lost.declared_len, 3);
        assert_eq!(one_byte_lost.payload, [0x01, 0x01]);
        assert!(!one_byte_lost.length_matches());
        let length_only = hex("04 00 04 00 58 00 00 00 02 01");
        let empty = control(&length_only);
        assert_eq!(empty.declared_len, 0x0102);
        assert_eq!(empty.payload, [0_u8; 0]);
        assert!(!empty.length_matches());
    }

    #[test]
    fn start_parses_as_a_control_packet_with_matching_length() {
        let start = control(&START);
        assert_eq!(start.declared_len, 9);
        assert_eq!(start.payload, &START[CONTROL_MIN_LEN..]);
        assert!(start.length_matches());
    }

    #[test]
    fn stop_parses_as_a_control_packet_with_matching_length() {
        let stop = control(&STOP);
        assert_eq!(stop.declared_len, 2);
        assert_eq!(stop.payload, &STOP[CONTROL_MIN_LEN..]);
        assert!(stop.length_matches());
    }

    #[test]
    fn unknown_subtype_is_a_value_not_an_error() {
        let with_body = hex("04 00 04 00 58 00 02 01 aa bb");
        assert_eq!(
            UplinkPacket::parse(&with_body),
            Ok(Some(UplinkPacket::Unknown {
                subtype: 0x0102,
                body: &[0xaa, 0xbb],
            }))
        );
        let bare = hex("04 00 04 00 58 00 02 00");
        assert_eq!(
            UplinkPacket::parse(&bare),
            Ok(Some(UplinkPacket::Unknown {
                subtype: 0x0002,
                body: &[],
            }))
        );
    }

    #[test]
    fn encode_then_parse_round_trips() {
        let one_unit = AccessUnit {
            stamp: 0x4030_2010,
            data: &SILENT_UNIT,
        };
        assert_eq!(encode_audio(&OPAQUE, &[one_unit]), Ok(hex(ONE_UNIT_SDU)));
        let payloads = [SILENT_UNIT.to_vec(), unit_bytes(0x07, 255), Vec::new()];
        let whole = units(&payloads);
        let sdu = encode_audio(&OPAQUE, &whole).unwrap();
        let parsed = audio(&sdu);
        assert_eq!(parsed.opaque, &OPAQUE);
        let mut seen = parsed.units();
        assert_eq!(seen.by_ref().collect::<Vec<_>>(), whole);
        assert_eq!(seen.trailing(), 0);
    }

    #[test]
    fn a_256_byte_unit_cannot_be_encoded() {
        let longest = unit_bytes(0x00, 255);
        let too_long = unit_bytes(0x00, 256);
        let fits = AccessUnit {
            stamp: 0,
            data: &longest,
        };
        let overflows = AccessUnit {
            stamp: 1,
            data: &too_long,
        };
        assert!(encode_audio(&OPAQUE, &[fits]).is_ok());
        let error = encode_audio(&OPAQUE, &[fits, overflows]).unwrap_err();
        assert_eq!(error, EncodeError::UnitTooLong { index: 1, len: 256 });
        assert_eq!(
            error.to_string(),
            "uplink audio packet cannot carry access unit 1: it is 256 bytes, the length byte holds at most 255"
        );
    }

    #[test]
    fn mic_side_commands_are_the_documented_bytes() {
        for (side, bytes) in [
            (MicSide::Automatic, "04 00 04 00 09 00 01 00 00 00 00"),
            (MicSide::Right, "04 00 04 00 09 00 01 01 00 00 00"),
            (MicSide::Left, "04 00 04 00 09 00 01 02 00 00 00"),
        ] {
            assert_eq!(side.command().id, ControlId::MIC_SIDE, "{side:?}");
            assert_eq!(side.command().encode(), hex(bytes), "{side:?}");
        }
    }

    #[test]
    #[expect(
        clippy::as_conversions,
        reason = "reads each declared discriminant to compare it with the wire value"
    )]
    fn mic_side_reads_back_its_own_command_value_and_rejects_the_rest() {
        for side in [MicSide::Automatic, MicSide::Right, MicSide::Left] {
            let [wire, ..] = side.command().value;
            assert_eq!(side as u8, wire, "{side:?}");
            assert_eq!(MicSide::try_from(wire), Ok(side), "{side:?}");
        }
        assert_eq!(
            MicSide::try_from(3),
            Err(ParseError::UnknownValue {
                parser: Parser::Control,
                field: "microphone side",
                value: 3
            })
        );
    }
}
