//! The proximity keys the pods hand over on request: one resolves their random address, one opens their advertisement.

use std::fmt;

use zeroize::Zeroize;

use crate::error::{HEADER_LEN, ParseError, Parser, strip_header};

const HEADER: [u8; HEADER_LEN] = [0x04, 0x00, 0x04, 0x00, 0x31, 0x00];
const ENTRY_COUNT: u8 = 0x02;
const IDENTITY_TAG: u8 = 0x01;
const PAYLOAD_TAG: u8 = 0x04;
pub const KEY_LEN: usize = 16;
/// A tag, a big endian length and one reserved byte.
const ENTRY_HEAD_LEN: usize = 4;
const ENTRY_LEN: usize = ENTRY_HEAD_LEN + KEY_LEN;
/// 47, the shortest reply `parseMagicCloudKeysPacket` in `daemon/airpods_packets.h` reads; later bytes are ignored there too.
const MIN_LEN: usize = HEADER_LEN + 1 + 2 * ENTRY_LEN;

/// Wiped when dropped, and never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct Key([u8; KEY_LEN]);

impl Key {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn expose(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key(redacted)")
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProximityKeys {
    /// Resolves the pods' resolvable private address.
    pub identity: Key,
    /// Decrypts the end of the pods' proximity advertisement.
    pub payload: Key,
}

impl ProximityKeys {
    /// Sample input: `04 00 04 00 31 00 02 01 00 10 00 <16 bytes> 04 00 10 00 <16 bytes>`, a count and two tagged entries.
    pub fn parse(packet: &[u8]) -> Result<Self, ParseError> {
        let body = strip_header(Parser::ProximityKeys, packet, HEADER, MIN_LEN)?;
        let Some((&count, entries)) = body.split_first() else {
            return Err(too_short(Parser::ProximityKeys, MIN_LEN, packet.len()));
        };
        if count != ENTRY_COUNT {
            return Err(ParseError::UnknownValue {
                parser: Parser::ProximityKeys,
                field: "entry count",
                value: count,
            });
        }
        let (identity, rest) = entry(entries, IDENTITY_TAG)?;
        let (payload, _) = entry(rest, PAYLOAD_TAG)?;
        Ok(Self { identity, payload })
    }
}

/// Sample input: `01 00 10 00 <16 bytes>`, the tag `tag`, length 16 big endian, a reserved byte, the key.
fn entry(bytes: &[u8], tag: u8) -> Result<(Key, &[u8]), ParseError> {
    let Some((&[found, high, low, _reserved], rest)) = bytes.split_first_chunk() else {
        return Err(too_short(Parser::ProximityKeyEntry, ENTRY_LEN, bytes.len()));
    };
    if found != tag {
        return Err(ParseError::UnknownValue {
            parser: Parser::ProximityKeyEntry,
            field: "tag",
            value: found,
        });
    }
    let len = usize::from(u16::from_be_bytes([high, low]));
    if len != KEY_LEN {
        return Err(ParseError::WrongLength {
            parser: Parser::ProximityKeyEntry,
            expected: KEY_LEN,
            got: len,
        });
    }
    let Some((key, rest)) = rest.split_first_chunk::<KEY_LEN>() else {
        return Err(too_short(Parser::ProximityKeyEntry, ENTRY_LEN, bytes.len()));
    };
    Ok((Key(*key), rest))
}

const fn too_short(parser: Parser, need: usize, got: usize) -> ParseError {
    ParseError::TooShort { parser, need, got }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpp_source::PACKETS;
    use pods_samples::host::MAGIC_PAIRING;
    use pods_samples::{IDENTITY_KEY, PAYLOAD_KEY, keys_of};
    use proptest::prelude::*;

    /// Where the payload entry's length sits: header, count, the identity entry, the payload tag.
    const PAYLOAD_LEN_AT: usize = HEADER_LEN + 1 + ENTRY_LEN + 1;

    fn sample() -> ProximityKeys {
        ProximityKeys {
            identity: Key::from_bytes(IDENTITY_KEY),
            payload: Key::from_bytes(PAYLOAD_KEY),
        }
    }

    #[test]
    fn reads_both_keys_and_ignores_bytes_after_them() {
        let mut packet = keys_of(IDENTITY_KEY, PAYLOAD_KEY);
        assert_eq!(packet.len(), MIN_LEN);
        assert_eq!(ProximityKeys::parse(&packet), Ok(sample()));
        packet.extend([0xee; 3]);
        assert_eq!(ProximityKeys::parse(&packet), Ok(sample()));
    }

    #[test]
    fn a_reply_cut_anywhere_is_too_short() {
        let packet = keys_of(IDENTITY_KEY, PAYLOAD_KEY);
        for len in 0..MIN_LEN {
            let refused = ProximityKeys::parse(&packet[..len]);
            assert!(
                matches!(refused, Err(ParseError::TooShort { need: MIN_LEN, got, .. }) if got == len),
                "{len}: {refused:?}"
            );
        }
    }

    #[test]
    fn a_wrong_count_tag_or_length_is_refused_by_name() {
        let wrong = |at: usize, value: u8| {
            let mut packet = keys_of(IDENTITY_KEY, PAYLOAD_KEY);
            packet[at] = value;
            ProximityKeys::parse(&packet).unwrap_err()
        };
        let count = ParseError::UnknownValue {
            parser: Parser::ProximityKeys,
            field: "entry count",
            value: 0x03,
        };
        assert_eq!(wrong(HEADER_LEN, 0x03), count);
        let tag = ParseError::UnknownValue {
            parser: Parser::ProximityKeyEntry,
            field: "tag",
            value: 0x02,
        };
        assert_eq!(wrong(HEADER_LEN + 1, 0x02), tag);
        assert_eq!(wrong(HEADER_LEN + 1 + ENTRY_LEN, 0x02), tag);
        let length = |got| ParseError::WrongLength {
            parser: Parser::ProximityKeyEntry,
            expected: KEY_LEN,
            got,
        };
        assert_eq!(wrong(PAYLOAD_LEN_AT + 1, 0x0f), length(15));
        assert_eq!(wrong(PAYLOAD_LEN_AT, 0x01), length(0x0110));
    }

    #[test]
    fn no_error_or_debug_text_holds_a_key_byte() {
        let shown = format!("{:?}", sample());
        assert_eq!(
            shown,
            "ProximityKeys { identity: Key(redacted), payload: Key(redacted) }"
        );
        let mut packet = keys_of(IDENTITY_KEY, PAYLOAD_KEY);
        packet.truncate(MIN_LEN - 1);
        let error = ProximityKeys::parse(&packet).unwrap_err();
        assert_eq!(
            error.to_string(),
            "proximity keys reply too short: need 47 bytes, got 46"
        );
    }

    #[test]
    fn the_layout_is_the_cpp_one() {
        let cpp_header = PACKETS.hex(MAGIC_PAIRING, "MAGIC_CLOUD_KEYS_HEADER");
        assert_eq!(cpp_header, [&HEADER[..], &[ENTRY_COUNT]].concat());
        let parser = PACKETS.body("inline MagicCloudKeys parseMagicCloudKeysPacket");
        assert!(
            parser.contains(&format!("data.size() < {MIN_LEN}")),
            "{parser}"
        );
        assert!(
            parser.contains(&format!("!= {IDENTITY_TAG:#04x})")),
            "{parser}"
        );
        assert!(
            parser.contains(&format!("!= {PAYLOAD_TAG:#04x})")),
            "{parser}"
        );
        assert!(parser.contains(&format!("!= {KEY_LEN})")), "{parser}");
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..96)) {
            drop(ProximityKeys::parse(&bytes));
            drop(ProximityKeys::parse(&[&HEADER[..], &[ENTRY_COUNT], &bytes].concat()));
        }

        #[test]
        fn any_two_keys_round_trip(identity in any::<[u8; KEY_LEN]>(), payload in any::<[u8; KEY_LEN]>()) {
            let parsed = ProximityKeys::parse(&keys_of(identity, payload)).unwrap();
            prop_assert_eq!(parsed.identity.expose(), &identity);
            prop_assert_eq!(parsed.payload.expose(), &payload);
        }
    }
}
