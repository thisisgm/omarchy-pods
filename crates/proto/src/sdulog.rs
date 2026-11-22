//! The capture log: one text line per packet of the control channel, written and read with no I/O.

use std::fmt;

/// This log format's own bound on one line, not a protocol constant.
pub const MAX_SDU_LEN: usize = 4096;
const FROM_PODS_MARK: &str = "<";
const TO_PODS_MARK: &str = ">";
const DIGITS_PER_BYTE: usize = 2;
const HIGH_DIGIT_SHIFT: u32 = 4;
const VALUE_OF_A: u8 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    FromPods,
    ToPods,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// Nanoseconds on the clock of whoever wrote the line.
    pub at_ns: u64,
    pub direction: Direction,
    /// 1 to `MAX_SDU_LEN` bytes.
    pub sdu: Vec<u8>,
}

/// Never repeats the line, whose bytes can identify a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LogError {
    #[error(
        "capture log line is missing a field: it needs a stamp, a direction and hex bytes, separated by single spaces"
    )]
    MissingField,
    #[error(
        "capture log line has a bad stamp: the first field must be a decimal number from 0 to {max}, with no sign and no leading zero",
        max = u64::MAX
    )]
    BadStamp,
    #[error(
        "capture log line has a bad direction: the second field must be {FROM_PODS_MARK} or {TO_PODS_MARK}"
    )]
    BadDirection,
    #[error(
        "capture log line has bad hex: byte {byte} of the third field, counting from 0, is not two lowercase hex digits"
    )]
    BadHex { byte: usize },
    /// Also what `format_line` returns for an empty packet.
    #[error("capture log line carries no bytes: the third field is empty")]
    Empty,
    /// Checked before any digit is read, so it wins over `BadHex`.
    #[error(
        "capture log line is too long: the third field is long enough for {len} bytes, at most {max} fit",
        max = MAX_SDU_LEN
    )]
    TooLong { len: usize },
}

impl Direction {
    fn mark(self) -> &'static str {
        match self {
            Self::FromPods => FROM_PODS_MARK,
            Self::ToPods => TO_PODS_MARK,
        }
    }

    // Sample input: `<` for a packet the pods sent, `>` for one the host sent.
    fn from_mark(mark: &str) -> Result<Self, LogError> {
        match mark {
            FROM_PODS_MARK => Ok(Self::FromPods),
            TO_PODS_MARK => Ok(Self::ToPods),
            _ => Err(LogError::BadDirection),
        }
    }
}

struct Hex<'a>(&'a [u8]);

impl fmt::Display for Hex<'_> {
    // Sample output: `0400040058000100`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// One line, no newline, that `parse_line` reads back; an empty or too long packet is refused.
pub fn format_line(at_ns: u64, direction: Direction, sdu: &[u8]) -> Result<String, LogError> {
    if sdu.is_empty() {
        return Err(LogError::Empty);
    }
    if sdu.len() > MAX_SDU_LEN {
        return Err(LogError::TooLong { len: sdu.len() });
    }
    Ok(format!(
        "{at_ns} {mark} {hex}",
        mark = direction.mark(),
        hex = Hex(sdu)
    ))
}

/// Sample input: `1203456789 < 0400040058000100`; only the exact form `format_line` writes.
pub fn parse_line(line: &str) -> Result<Record, LogError> {
    let Some((stamp, after_stamp)) = line.split_once(' ') else {
        return Err(LogError::MissingField);
    };
    let Some((mark, hex)) = after_stamp.split_once(' ') else {
        return Err(LogError::MissingField);
    };
    Ok(Record {
        at_ns: parse_stamp(stamp)?,
        direction: Direction::from_mark(mark)?,
        sdu: parse_hex(hex)?,
    })
}

// Sample input: `1203456789`, refused unless writing the number again gives the same text.
fn parse_stamp(stamp: &str) -> Result<u64, LogError> {
    let Ok(at_ns) = stamp.parse::<u64>() else {
        return Err(LogError::BadStamp);
    };
    if at_ns.to_string() != stamp {
        return Err(LogError::BadStamp);
    }
    Ok(at_ns)
}

/// Sample input: `0400040058000100`, two lowercase hex digits for each of 1 to `MAX_SDU_LEN` bytes.
pub fn parse_hex(hex: &str) -> Result<Vec<u8>, LogError> {
    if hex.is_empty() {
        return Err(LogError::Empty);
    }
    let (pairs, half_byte) = hex.as_bytes().as_chunks::<DIGITS_PER_BYTE>();
    if pairs.len() > MAX_SDU_LEN {
        return Err(LogError::TooLong { len: pairs.len() });
    }
    let mut sdu = Vec::with_capacity(pairs.len());
    for (at, [high, low]) in pairs.iter().enumerate() {
        let (Some(high), Some(low)) = (digit_value(*high), digit_value(*low)) else {
            return Err(LogError::BadHex { byte: at });
        };
        sdu.push(high << HIGH_DIGIT_SHIFT | low);
    }
    if !half_byte.is_empty() {
        return Err(LogError::BadHex { byte: pairs.len() });
    }
    Ok(sdu)
}

// Sample input: `b'a'` reads as 10; an uppercase digit and every other character read as None.
fn digit_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => digit.checked_sub(b'0'),
        b'a'..=b'f' => digit.checked_sub(b'a')?.checked_add(VALUE_OF_A),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The line the module documents: an audio gate the pods sent 1.2 seconds into a capture.
    const SAMPLE_LINE: &str = "1203456789 < 0400040058000100";
    const SAMPLE_AT_NS: u64 = 1_203_456_789;
    const SAMPLE_SDU: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x01, 0x00];
    const MIN_SDU_LEN: usize = 1;
    /// The largest stamp, written out so the test does not lean on the formatter.
    const LAST_STAMP: &str = "18446744073709551615";
    const STAMP_PAST_U64: &str = "18446744073709551616";
    /// The documented bound, written out so the test pins the number, not the constant.
    const LONGEST: usize = 4096;
    const TOO_LONG: usize = 4097;

    // Sample output for 3: `00 01 02`, counting bytes that stand in for one packet.
    fn counting_bytes(len: usize) -> Vec<u8> {
        std::iter::successors(Some(0_u8), |byte| Some(byte.wrapping_add(1)))
            .take(len)
            .collect()
    }

    fn direction() -> impl Strategy<Value = Direction> {
        prop_oneof![Just(Direction::FromPods), Just(Direction::ToPods)]
    }

    fn packet() -> impl Strategy<Value = Vec<u8>> {
        prop_oneof![
            Just(Vec::new()),
            proptest::collection::vec(any::<u8>(), MIN_SDU_LEN),
            proptest::collection::vec(any::<u8>(), MAX_SDU_LEN),
            proptest::collection::vec(any::<u8>(), MIN_SDU_LEN..=MAX_SDU_LEN),
            proptest::collection::vec(any::<u8>(), TOO_LONG),
        ]
    }

    #[test]
    fn formats_the_documented_sample() {
        assert_eq!(
            format_line(SAMPLE_AT_NS, Direction::FromPods, &SAMPLE_SDU),
            Ok(SAMPLE_LINE.to_owned())
        );
        assert_eq!(
            format_line(0, Direction::ToPods, &[0xff, 0x0a]),
            Ok("0 > ff0a".to_owned())
        );
    }

    #[test]
    fn format_line_refuses_what_parse_line_refuses() {
        for direction in [Direction::FromPods, Direction::ToPods] {
            assert_eq!(format_line(1, direction, &[]), Err(LogError::Empty));
            assert_eq!(
                format_line(1, direction, &counting_bytes(TOO_LONG)),
                Err(LogError::TooLong { len: TOO_LONG })
            );
            for len in [MIN_SDU_LEN, LONGEST] {
                let written = format_line(1, direction, &counting_bytes(len));
                assert!(written.is_ok(), "{direction:?} {len}: {written:?}");
            }
        }
    }

    #[test]
    fn parses_the_documented_sample() {
        let sample = Record {
            at_ns: SAMPLE_AT_NS,
            direction: Direction::FromPods,
            sdu: SAMPLE_SDU.to_vec(),
        };
        assert_eq!(parse_line(SAMPLE_LINE), Ok(sample));
        let to_pods = Record {
            at_ns: 0,
            direction: Direction::ToPods,
            sdu: vec![0xff, 0x0a],
        };
        assert_eq!(parse_line("0 > ff0a"), Ok(to_pods));
    }

    #[test]
    fn round_trips_at_every_edge() {
        for at_ns in [0, u64::MAX] {
            for direction in [Direction::FromPods, Direction::ToPods] {
                for len in [MIN_SDU_LEN, MAX_SDU_LEN] {
                    let sdu = counting_bytes(len);
                    let line = format_line(at_ns, direction, &sdu).unwrap();
                    let record = Record {
                        at_ns,
                        direction,
                        sdu,
                    };
                    assert_eq!(parse_line(&line), Ok(record), "{at_ns} {direction:?} {len}");
                }
            }
        }
    }

    proptest! {
        #[test]
        fn round_trips(
            at_ns in prop_oneof![Just(0), Just(u64::MAX), any::<u64>()],
            direction in direction(),
            sdu in packet(),
        ) {
            let written = format_line(at_ns, direction, &sdu);
            prop_assert_eq!(written.is_ok(), (MIN_SDU_LEN..=LONGEST).contains(&sdu.len()));
            if let Ok(line) = written {
                prop_assert_eq!(parse_line(&line), Ok(Record { at_ns, direction, sdu }));
            }
        }
    }

    #[test]
    fn rejects_uppercase_odd_and_empty_hex() {
        for (line, byte) in [
            ("1 < 0A", 0),
            ("1 < A0", 0),
            ("1 < 00aF", 1),
            ("1 < 0g", 0),
            ("1 < 0x00", 0),
            ("1 < 00+f", 1),
            ("1 < 00\u{e9}", 1),
            ("1 < 0", 0),
            ("1 < 000", 1),
            ("1 < 00\n", 1),
        ] {
            assert_eq!(parse_line(line), Err(LogError::BadHex { byte }), "{line:?}");
        }
        assert_eq!(parse_line("1 < "), Err(LogError::Empty));
    }

    #[test]
    fn rejects_a_missing_field() {
        for line in ["", "1203456789", "1203456789 <", "<", "1203456789<00"] {
            assert_eq!(parse_line(line), Err(LogError::MissingField), "{line:?}");
        }
    }

    #[test]
    fn rejects_a_second_space_or_a_trailing_field() {
        for (line, error) in [
            (" 1 < 00", LogError::BadStamp),
            ("1  < 00", LogError::BadDirection),
            ("1 <  00", LogError::BadHex { byte: 0 }),
            ("1 < 00 ", LogError::BadHex { byte: 1 }),
            ("1 < 00 11", LogError::BadHex { byte: 1 }),
            ("1 < 00 > 11", LogError::BadHex { byte: 1 }),
        ] {
            assert_eq!(parse_line(line), Err(error), "{line:?}");
        }
    }

    #[test]
    fn rejects_an_unknown_direction() {
        for line in ["1 x 00", "1 << 00", "1 <> 00", "1 - 00", "1 \u{2190} 00"] {
            assert_eq!(parse_line(line), Err(LogError::BadDirection), "{line:?}");
        }
    }

    #[test]
    fn rejects_4097_bytes() {
        let longest = counting_bytes(LONGEST);
        let line = format_line(1, Direction::FromPods, &longest).unwrap();
        assert_eq!(parse_line(&line).map(|record| record.sdu), Ok(longest));
        let too_long = format!("{line}00");
        assert_eq!(
            parse_line(&too_long),
            Err(LogError::TooLong { len: TOO_LONG })
        );
    }

    #[test]
    fn too_long_wins_over_bad_hex() {
        let not_hex_and_longest = format!("1 < {}", "zz".repeat(LONGEST));
        assert_eq!(
            parse_line(&not_hex_and_longest),
            Err(LogError::BadHex { byte: 0 })
        );
        let not_hex_and_too_long = format!("1 < {}", "zz".repeat(TOO_LONG));
        assert_eq!(
            parse_line(&not_hex_and_too_long),
            Err(LogError::TooLong { len: TOO_LONG })
        );
    }

    #[test]
    fn rejects_a_stamp_past_u64() {
        let last = format!("{LAST_STAMP} < 00");
        assert_eq!(parse_line(&last).map(|record| record.at_ns), Ok(u64::MAX));
        let past = format!("{STAMP_PAST_U64} < 00");
        assert_eq!(parse_line(&past), Err(LogError::BadStamp));
    }

    #[test]
    fn rejects_a_stamp_format_line_never_writes() {
        assert_eq!(parse_line("0 < 00").map(|record| record.at_ns), Ok(0));
        for line in [
            "+1 < 00",
            "-1 < 00",
            "01 < 00",
            "00 < 00",
            "1.5 < 00",
            "1e3 < 00",
            "0x10 < 00",
            "1_000 < 00",
            "\u{661} < 00",
            "< < 00",
        ] {
            assert_eq!(parse_line(line), Err(LogError::BadStamp), "{line:?}");
        }
    }

    #[test]
    fn every_error_says_what_is_wrong_and_where() {
        for (error, text) in [
            (
                LogError::MissingField,
                "capture log line is missing a field: it needs a stamp, a direction and hex bytes, separated by single spaces",
            ),
            (
                LogError::BadStamp,
                "capture log line has a bad stamp: the first field must be a decimal number from 0 to 18446744073709551615, with no sign and no leading zero",
            ),
            (
                LogError::BadDirection,
                "capture log line has a bad direction: the second field must be < or >",
            ),
            (
                LogError::BadHex { byte: 353 },
                "capture log line has bad hex: byte 353 of the third field, counting from 0, is not two lowercase hex digits",
            ),
            (
                LogError::Empty,
                "capture log line carries no bytes: the third field is empty",
            ),
            (
                LogError::TooLong { len: TOO_LONG },
                "capture log line is too long: the third field is long enough for 4097 bytes, at most 4096 fit",
            ),
        ] {
            let error: &dyn std::error::Error = &error;
            assert_eq!(error.to_string(), text);
        }
    }
}
