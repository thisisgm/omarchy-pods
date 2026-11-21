//! Random input never panics the classifier or the metadata parser; each sorts or cuts by its rule.

use pods_proto::battery::BatteryReport;
use pods_proto::control::ControlCommand;
use pods_proto::ear::EarReport;
use pods_proto::metadata::{FIELD_MAX_BYTES, Metadata};
use pods_proto::packet::{Incoming, classify};
use proptest::prelude::*;

const METADATA_HEADER: [u8; 6] = [0x04, 0x00, 0x04, 0x00, 0x1d, 0x00];
/// Every packet the pods send after the handshake starts with these bytes; the opcode follows.
const ACCESSORY_HEADER: [u8; 4] = [0x04, 0x00, 0x04, 0x00];
/// Battery, ear, control, metadata and uplink: the opcodes `classify` does not report as `Other`.
const ROUTED_OPCODES: [u16; 5] = [0x0004, 0x0006, 0x0009, 0x001d, 0x0058];
/// Past every fixed length the routed parsers check.
const ROUTED_BODY_BOUND: usize = 64;
const METADATA_SKIPPED_LEN: usize = 5;
/// One more than the most strings a generated metadata packet carries, more than the three read.
const METADATA_STRING_BOUND: usize = 6;
/// Longest generated printable field, short enough that no field is cut.
const SHORT_FIELD_LEN: usize = 40;
const _: () = assert!(SHORT_FIELD_LEN < FIELD_MAX_BYTES);
/// Long enough that random fields often run past `FIELD_MAX_BYTES`.
const METADATA_BODY_BOUND: usize = 1024;

proptest! {
    #[test]
    fn classify_never_panics(b in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _outcome = classify(&b);
    }

    // An opcode with a parser gets its result, 0x58 is handed over whole, any other is Other.
    #[test]
    fn classify_sorts_every_opcode(
        opcode in prop_oneof![proptest::sample::select(&ROUTED_OPCODES), any::<u16>()],
        body in proptest::collection::vec(any::<u8>(), 0..ROUTED_BODY_BOUND),
    ) {
        let mut sdu = ACCESSORY_HEADER.to_vec();
        sdu.extend(opcode.to_le_bytes());
        sdu.extend(&body);
        let expected = match opcode {
            0x0004 => BatteryReport::parse(&sdu).map(Incoming::Battery),
            0x0006 => EarReport::parse(&sdu).map(Incoming::Ear),
            0x0009 => ControlCommand::parse(&sdu).map(Incoming::Control),
            0x001d => Metadata::parse(&sdu).map(Incoming::Metadata),
            0x0058 => Ok(Incoming::Uplink(&sdu)),
            _ => Ok(Incoming::Other { opcode }),
        };
        prop_assert_eq!(classify(&sdu), expected);
    }

    #[test]
    fn classify_without_the_accessory_header_is_foreign(
        b in proptest::collection::vec(any::<u8>(), 0..ROUTED_BODY_BOUND)
            .prop_filter("starts with the accessory header", |b| !b.starts_with(&ACCESSORY_HEADER)),
    ) {
        prop_assert_eq!(classify(&b), Ok(Incoming::Foreign));
    }

    // Short printable fields read back as the first three zero ended strings, missing ones empty.
    #[test]
    fn metadata_reads_the_first_three_strings(
        skipped in any::<[u8; METADATA_SKIPPED_LEN]>(),
        strings in proptest::collection::vec(
            proptest::collection::vec(proptest::char::range(' ', '~'), 0..=SHORT_FIELD_LEN).prop_map(String::from_iter),
            0..METADATA_STRING_BOUND,
        ),
    ) {
        let mut sdu = METADATA_HEADER.to_vec();
        sdu.extend(skipped);
        sdu.extend(strings.join("\0").as_bytes());
        let m = Metadata::parse(&sdu).unwrap();
        let expected = |at: usize| strings.get(at).map_or("", String::as_str);
        prop_assert_eq!((m.name.as_str(), m.model_number.as_str(), m.manufacturer.as_str()), (expected(0), expected(1), expected(2)));
    }

    // Random fields, some invalid UTF-8 that grows when replaced, each come back cut to the bound.
    #[test]
    fn metadata_fields_are_cut_to_the_bound(
        skipped in any::<[u8; METADATA_SKIPPED_LEN]>(),
        body in proptest::collection::vec(any::<u8>(), 0..METADATA_BODY_BOUND),
    ) {
        let mut sdu = METADATA_HEADER.to_vec();
        sdu.extend(skipped);
        sdu.extend(&body);
        let m = Metadata::parse(&sdu).unwrap();
        prop_assert!(m.name.len() <= FIELD_MAX_BYTES && m.model_number.len() <= FIELD_MAX_BYTES && m.manufacturer.len() <= FIELD_MAX_BYTES);
    }

    #[test]
    fn metadata_never_panics_and_stays_bounded(b in proptest::collection::vec(any::<u8>(), 0..4096)) {
        if let Ok(m) = Metadata::parse(&b) {
            prop_assert!(m.name.len() <= FIELD_MAX_BYTES && m.model_number.len() <= FIELD_MAX_BYTES && m.manufacturer.len() <= FIELD_MAX_BYTES);
        }
    }
}
