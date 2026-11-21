//! A Bluetooth device address that no formatter can print.

use std::fmt;
use std::str::FromStr;

const OCTETS: usize = 6;
/// Two hex digits and the colon after them.
const GROUP_LEN: usize = 3;
/// Six groups, the last one without its colon.
const TEXT_LEN: usize = OCTETS * GROUP_LEN - 1;
const HEX_RADIX: u32 = 16;

/// Octets in the order people read them.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BdAddr([u8; OCTETS]);

/// Names the place and cannot hold any of the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BdAddrError {
    #[error("Bluetooth address is {got} bytes, expected {expected}", expected = TEXT_LEN)]
    WrongLength { got: usize },
    /// `pair` counts from 1.
    #[error("Bluetooth address pair {pair} is not two hex digits")]
    BadDigit { pair: usize },
    /// `pair` counts from 1.
    #[error("Bluetooth address has no colon after pair {pair}")]
    BadSeparator { pair: usize },
}

impl BdAddr {
    #[must_use]
    pub fn octets(self) -> [u8; OCTETS] {
        self.0
    }
}

impl From<[u8; OCTETS]> for BdAddr {
    /// Octets in the order people read them, as `octets` returns them.
    fn from(octets: [u8; OCTETS]) -> Self {
        Self(octets)
    }
}

impl fmt::Debug for BdAddr {
    // The address names its owner's device, so a log line or a panic message must not carry it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BdAddr(redacted)")
    }
}

impl FromStr for BdAddr {
    type Err = BdAddrError;

    // Sample input: `AA:BB:CC:DD:EE:FF` or `aa:bb:cc:dd:ee:ff`, six hex pairs with colons between.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let bytes = text.as_bytes();
        if bytes.len() != TEXT_LEN {
            return Err(BdAddrError::WrongLength { got: bytes.len() });
        }
        let mut octets = [0_u8; OCTETS];
        let groups = bytes.chunks(GROUP_LEN).zip(1_usize..);
        for (octet, (group, pair)) in octets.iter_mut().zip(groups) {
            match *group {
                [high, low] | [high, low, b':'] => {
                    *octet = hex_pair(high, low).ok_or(BdAddrError::BadDigit { pair })?;
                }
                _ => return Err(BdAddrError::BadSeparator { pair }),
            }
        }
        Ok(Self(octets))
    }
}

// Sample input: `b'a'` and `b'9'`, or `b'A'` and `b'9'`, read as 0xa9; a sign or space is refused.
fn hex_pair(high: u8, low: u8) -> Option<u8> {
    let high = char::from(high).to_digit(HEX_RADIX)?;
    let low = char::from(low).to_digit(HEX_RADIX)?;
    u8::try_from(high.checked_mul(HEX_RADIX)?.checked_add(low)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const UPPER: &str = "AA:BB:CC:DD:EE:FF";
    const LOWER: &str = "aa:bb:cc:dd:ee:ff";

    fn refusal(text: &str) -> BdAddrError {
        text.parse::<BdAddr>().unwrap_err()
    }

    #[test]
    fn parses_both_cases() {
        let octets = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
        assert_eq!(UPPER.parse::<BdAddr>().unwrap().octets(), octets);
        assert_eq!(LOWER.parse::<BdAddr>().unwrap().octets(), octets);
        assert_eq!(
            "02:00:00:00:00:01".parse::<BdAddr>().unwrap().octets(),
            [0x02, 0x00, 0x00, 0x00, 0x00, 0x01]
        );
    }

    #[test]
    fn rejects_wrong_length_separator_and_digit() {
        for (text, error) in [
            ("", BdAddrError::WrongLength { got: 0 }),
            ("AA:BB:CC:DD:EE:F", BdAddrError::WrongLength { got: 16 }),
            ("AA:BB:CC:DD:EE:FF:", BdAddrError::WrongLength { got: 18 }),
            ("AA-BB:CC:DD:EE:FF", BdAddrError::BadSeparator { pair: 1 }),
            ("AA:BB:CC:DD:EE-FF", BdAddrError::BadSeparator { pair: 5 }),
            ("AABBCCDDEEFF:::::", BdAddrError::BadSeparator { pair: 1 }),
            ("AG:BB:CC:DD:EE:FF", BdAddrError::BadDigit { pair: 1 }),
            ("+A:BB:CC:DD:EE:FF", BdAddrError::BadDigit { pair: 1 }),
            ("AA:BB:CC:DD:EE:F ", BdAddrError::BadDigit { pair: 6 }),
            ("\u{e9}:BB:CC:DD:EE:FF", BdAddrError::BadDigit { pair: 1 }),
        ] {
            assert_eq!(refusal(text), error, "case {error:?}");
        }
    }

    #[test]
    fn every_refusal_names_the_place_and_never_the_text() {
        assert_eq!(
            refusal("AA:BB").to_string(),
            "Bluetooth address is 5 bytes, expected 17"
        );
        assert_eq!(
            refusal("AA:BB:CC:DG:EE:FF").to_string(),
            "Bluetooth address pair 4 is not two hex digits"
        );
        assert_eq!(
            refusal("AA:BB:CC-DD:EE:FF").to_string(),
            "Bluetooth address has no colon after pair 3"
        );
    }

    #[test]
    fn octets_come_back_as_they_went_in() {
        let octets = [0x02, 0x00, 0x00, 0x00, 0x00, 0x09];
        assert_eq!(BdAddr::from(octets).octets(), octets);
        assert_eq!(BdAddr::from(octets), "02:00:00:00:00:09".parse().unwrap());
    }

    #[test]
    fn debug_never_shows_the_octets() {
        let addr = "11:22:33:44:55:66".parse::<BdAddr>().unwrap();
        assert_eq!(format!("{addr:?}"), "BdAddr(redacted)");
        assert_eq!(format!("{addr:#?}"), "BdAddr(redacted)");
        assert_eq!(format!("{:?}", Some(addr)), "Some(BdAddr(redacted))");
    }
}
