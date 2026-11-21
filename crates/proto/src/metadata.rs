//! The metadata packet: the name, model number and manufacturer the pods report about themselves.

use crate::error::{HEADER_LEN, ParseError, Parser, strip_header};

const HEADER: [u8; HEADER_LEN] = [0x04, 0x00, 0x04, 0x00, 0x1d, 0x00];
/// Bytes between the header and the name, which no known code reads.
const SKIPPED_LEN: usize = 5;
/// Where parseMetadata in daemon/main.cpp starts reading strings.
const MIN_LEN: usize = HEADER_LEN + SKIPPED_LEN;
const TERMINATOR: u8 = 0x00;

/// 248, the longest Bluetooth device name.
pub const FIELD_MAX_BYTES: usize = 248;

/// The serial numbers behind these three fields are never read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Metadata {
    pub name: String,
    pub model_number: String,
    pub manufacturer: String,
}

impl Metadata {
    /// Sample input: `04 00 04 00 1d 00 01 02 03 04 05 4e 00 41 00`, five skipped, 00 ended fields.
    pub fn parse(sdu: &[u8]) -> Result<Self, ParseError> {
        let body = strip_header(Parser::Metadata, sdu, HEADER, MIN_LEN)?;
        // strip_header refused anything under MIN_LEN, so the skipped bytes are always there.
        let strings = body.get(SKIPPED_LEN..).unwrap_or_default();
        // Lossy conversion may allocate three times a field before the cut; callers cap the SDU.
        let mut fields = strings.split(|&byte| byte == TERMINATOR);
        let name = text(fields.next());
        let model_number = text(fields.next());
        let manufacturer = text(fields.next());
        Ok(Self {
            name,
            model_number,
            manufacturer,
        })
    }
}

fn text(field: Option<&[u8]>) -> String {
    let mut text = String::from_utf8_lossy(field.unwrap_or_default()).into_owned();
    text.truncate(text.floor_char_boundary(FIELD_MAX_BYTES));
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{Header, Parser};
    use cpp_source::PACKETS;

    const NAME: &str = "Test\u{2019}s AirPods Pro";
    const SKIPPED: [u8; 5] = [0x01, 0x02, 0x03, 0x04, 0x05];
    /// parseMetadata in daemon/main.cpp skips six bytes behind its five byte header.
    const CPP_SKIPPED_LEN: usize = 6;

    fn packet(strings: &[u8]) -> Vec<u8> {
        [&HEADER[..], &SKIPPED, strings].concat()
    }

    fn fields(name: &str, model_number: &str, manufacturer: &str) -> Metadata {
        Metadata {
            name: name.to_owned(),
            model_number: model_number.to_owned(),
            manufacturer: manufacturer.to_owned(),
        }
    }

    #[test]
    fn reads_name_model_and_manufacturer() {
        let sample = Metadata::parse(&pods_samples::metadata()).unwrap();
        assert_eq!(sample, fields(NAME, "A3063", "Apple Inc."));
        let short = Metadata::parse(&packet(b"Name\0A1234\0Maker\0")).unwrap();
        assert_eq!(short, fields("Name", "A1234", "Maker"));
    }

    #[test]
    fn keeps_a_typographic_apostrophe() {
        let mut strings = NAME.as_bytes().to_vec();
        strings.extend(b"\0A3063\0Apple Inc.\0");
        let metadata = Metadata::parse(&packet(&strings)).unwrap();
        assert_eq!(metadata.name, NAME);
        assert!(metadata.name.contains('\u{2019}'));
    }

    #[test]
    fn invalid_utf8_is_replaced_not_refused() {
        let metadata = Metadata::parse(&packet(b"A\xffB\0\xc3\0F\0")).unwrap();
        assert_eq!(metadata, fields("A\u{fffd}B", "\u{fffd}", "F"));
    }

    #[test]
    fn a_missing_terminator_ends_at_the_packet() {
        let metadata = Metadata::parse(&packet(b"Name")).unwrap();
        assert_eq!(metadata, fields("Name", "", ""));
        let metadata = Metadata::parse(&packet(b"Name\0A1234")).unwrap();
        assert_eq!(metadata, fields("Name", "A1234", ""));
        let metadata = Metadata::parse(&packet(b"")).unwrap();
        assert_eq!(metadata, fields("", "", ""));
    }

    #[test]
    fn a_name_over_248_bytes_is_cut_at_a_character() {
        let longest = "a".repeat(248);
        let mut strings = longest.as_bytes().to_vec();
        strings.extend(b"b\0");
        strings.extend(longest.as_bytes());
        strings.push(0);
        let metadata = Metadata::parse(&packet(&strings)).unwrap();
        assert_eq!(metadata.name, longest);
        assert_eq!(metadata.model_number, longest);

        // A two byte and a four byte character that each cross byte 248 are dropped whole.
        for (kept, crossing) in [(247, "\u{e9}"), (245, "\u{20000}")] {
            let name = format!("{}{crossing}", "a".repeat(kept));
            let metadata = Metadata::parse(&packet(name.as_bytes())).unwrap();
            assert_eq!(metadata.name, "a".repeat(kept), "{crossing}");
        }

        let mut strings = b"\0\0".to_vec();
        strings.extend([0xff; 248]);
        let metadata = Metadata::parse(&packet(&strings)).unwrap();
        assert_eq!(metadata.manufacturer, "\u{fffd}".repeat(82));
    }

    #[test]
    fn serial_number_fields_are_not_in_the_result() {
        let sample = pods_samples::metadata();
        let serial = b"SERIAL0001";
        assert!(sample.windows(serial.len()).any(|window| window == serial));
        let metadata = Metadata::parse(&sample).unwrap();
        assert!(!format!("{metadata:?}").contains("SERIAL0001"));
    }

    #[test]
    fn a_packet_shorter_than_the_skipped_bytes_is_refused() {
        let mut cut = packet(b"");
        cut.pop();
        let too_short = ParseError::TooShort {
            parser: Parser::Metadata,
            need: 11,
            got: 10,
        };
        assert_eq!(Metadata::parse(&cut), Err(too_short));
    }

    #[test]
    fn a_packet_with_another_header_is_refused() {
        let mut other = packet(b"Name\0");
        other[4] = 0x1e;
        let bad_header = ParseError::BadHeader {
            parser: Parser::Metadata,
            expected: Header(HEADER),
            got: Header([0x04, 0x00, 0x04, 0x00, 0x1e, 0x00]),
        };
        assert_eq!(Metadata::parse(&other), Err(bad_header));
    }

    #[test]
    fn strings_start_where_the_cpp_parser_starts_them() {
        let cpp_header = PACKETS.hex(Some("namespace Parse"), "METADATA");
        assert!(HEADER.starts_with(&cpp_header));
        assert_eq!(MIN_LEN, cpp_header.len() + CPP_SKIPPED_LEN);
    }
}
