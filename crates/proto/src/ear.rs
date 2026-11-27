//! Ear detection: where each bud sits.

use crate::error::{HEADER_LEN, ParseError, Parser, strip_header};

const HEADER: [u8; HEADER_LEN] = [0x04, 0x00, 0x04, 0x00, 0x06, 0x00];
/// Header plus the primary and secondary placement bytes.
const MIN_LEN: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EarPlacement {
    InEar,
    NotInEar,
    InCase,
    /// Not reported, or a placement byte the protocol does not define.
    #[default]
    Disconnected,
}

impl EarPlacement {
    // 00 in ear, 01 out of ear, 02 in the case, anything else unknown.
    fn from_wire(byte: u8) -> Self {
        match byte {
            0x00 => Self::InEar,
            0x01 => Self::NotInEar,
            0x02 => Self::InCase,
            _ => Self::Disconnected,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EarReport {
    pub primary: EarPlacement,
    pub secondary: EarPlacement,
}

impl EarReport {
    /// Sample input: `04 00 04 00 06 00 00 01`, header, primary, secondary; later bytes ignored.
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let body = strip_header(Parser::Ear, bytes, HEADER, MIN_LEN)?;
        let Some(&[primary, secondary]) = body.first_chunk::<2>() else {
            return Err(ParseError::TooShort {
                parser: Parser::Ear,
                need: MIN_LEN,
                got: bytes.len(),
            });
        };
        Ok(Self {
            primary: EarPlacement::from_wire(primary),
            secondary: EarPlacement::from_wire(secondary),
        })
    }

    /// For a source that knows only in or out.
    #[must_use]
    pub fn from_in_ear(primary_in_ear: bool, secondary_in_ear: bool) -> Self {
        let placement = |in_ear| {
            if in_ear {
                EarPlacement::InEar
            } else {
                EarPlacement::NotInEar
            }
        };
        Self {
            primary: placement(primary_in_ear),
            secondary: placement(secondary_in_ear),
        }
    }

    #[must_use]
    pub fn primary_in_ear(&self) -> bool {
        self.primary == EarPlacement::InEar
    }

    #[must_use]
    pub fn secondary_in_ear(&self) -> bool {
        self.secondary == EarPlacement::InEar
    }

    #[must_use]
    pub fn any_in_ear(&self) -> bool {
        self.primary_in_ear() || self.secondary_in_ear()
    }

    #[must_use]
    pub fn any_in_case(&self) -> bool {
        self.primary == EarPlacement::InCase || self.secondary == EarPlacement::InCase
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Header;

    fn packet(primary: u8, secondary: u8) -> Vec<u8> {
        let mut buf = HEADER.to_vec();
        buf.extend([primary, secondary]);
        buf
    }

    #[test]
    fn initial_state() {
        let d = EarReport::default();
        assert_eq!(d.primary, EarPlacement::Disconnected);
        assert_eq!(d.secondary, EarPlacement::Disconnected);
        assert!(!d.primary_in_ear());
        assert!(!d.secondary_in_ear());
        assert!(!d.any_in_ear());
        assert!(!d.any_in_case());
    }

    #[test]
    fn rejects_short_packets() {
        for size in 0..=7 {
            assert!(EarReport::parse(&vec![0; size]).is_err(), "size {size}");
        }
    }

    #[test]
    fn rejects_a_short_packet_that_has_the_ear_header() {
        let mut buf = HEADER.to_vec();
        buf.push(0x00);
        assert_eq!(
            EarReport::parse(&buf),
            Err(ParseError::TooShort {
                parser: Parser::Ear,
                need: 8,
                got: 7
            })
        );
    }

    #[test]
    fn accepts_eight_byte_packet() {
        let d = EarReport::parse(&packet(0x00, 0x01)).unwrap();
        assert_eq!(d.primary, EarPlacement::InEar);
        assert_eq!(d.secondary, EarPlacement::NotInEar);
        assert!(d.primary_in_ear());
        assert!(!d.secondary_in_ear());
        assert!(d.any_in_ear());
    }

    #[test]
    fn parses_all_status_bytes() {
        for (byte, status) in [
            (0x00, EarPlacement::InEar),
            (0x01, EarPlacement::NotInEar),
            (0x02, EarPlacement::InCase),
            (0x03, EarPlacement::Disconnected),
            (0xFF, EarPlacement::Disconnected),
        ] {
            assert_eq!(
                EarReport::parse(&packet(byte, 0x01)).unwrap().primary,
                status,
                "byte {byte:#04x}"
            );
        }
    }

    #[test]
    fn in_case_semantic() {
        let d = EarReport::parse(&packet(0x02, 0x01)).unwrap();
        assert!(d.any_in_case());
        assert!(!d.any_in_ear());
    }

    #[test]
    fn reset_clears_state() {
        let mut d = EarReport::parse(&packet(0x00, 0x00)).unwrap();
        assert!(d.any_in_ear());
        d = EarReport::default();
        assert_eq!(d.primary, EarPlacement::Disconnected);
        assert_eq!(d.secondary, EarPlacement::Disconnected);
        assert!(!d.any_in_ear());
    }

    #[test]
    fn override_matches_protocol() {
        let d = EarReport::from_in_ear(true, false);
        assert_eq!(d.primary, EarPlacement::InEar);
        assert_eq!(d.secondary, EarPlacement::NotInEar);
        let d = EarReport::from_in_ear(false, true);
        assert_eq!(d.primary, EarPlacement::NotInEar);
        assert_eq!(d.secondary, EarPlacement::InEar);
    }

    #[test]
    fn rejects_a_packet_without_the_ear_header() {
        assert_eq!(
            EarReport::parse(&[0; 8]),
            Err(ParseError::BadHeader {
                parser: Parser::Ear,
                expected: Header(HEADER),
                got: Header([0; 6]),
            })
        );
    }
}
