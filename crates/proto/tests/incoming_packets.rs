//! Random input never panics the classifier or the metadata parser; each sorts or cuts by its rule.

use pods_proto::battery::BatteryReport;
use pods_proto::control::ControlCommand;
use pods_proto::ear::EarReport;
use pods_proto::keys::ProximityKeys;
use pods_proto::metadata::{FIELD_MAX_BYTES, Metadata};
use pods_proto::packet::{Incoming, classify, opcode_of};
use pods_proto::uplink::UplinkState;
use pods_samples::uplink::{session_marker, state_marker};
use proptest::prelude::*;

const METADATA_HEADER: [u8; 6] = [0x04, 0x00, 0x04, 0x00, 0x1d, 0x00];
/// Every packet the pods send after the handshake starts with these bytes; the opcode follows.
const ACCESSORY_HEADER: [u8; 4] = [0x04, 0x00, 0x04, 0x00];
/// Battery, ear, control, the state marker, metadata, proximity keys, the session marker and uplink: the opcodes `classify` does not always report as `Other`.
const ROUTED_OPCODES: [u16; 8] = [
    0x0004, 0x0006, 0x0009, 0x000e, 0x001d, 0x0031, 0x0055, 0x0058,
];
/// Where the body of a packet begins, after the accessory header and the opcode.
const BODY_AT: usize = 6;
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

/// Any body, or the body of an uplink marker with any six bytes in the state marker, which a random body almost never is.
fn body() -> impl Strategy<Value = Vec<u8>> {
    let state = (any::<bool>(), any::<[u8; 6]>()).prop_map(|(running, opaque)| {
        let says = state_marker(running).last().copied();
        opaque.into_iter().chain(says).collect()
    });
    let session = any::<bool>().prop_map(|running| {
        let marker = session_marker(running).into_iter();
        marker.skip(BODY_AT).collect()
    });
    prop_oneof![
        proptest::collection::vec(any::<u8>(), 0..ROUTED_BODY_BOUND),
        state,
        session,
    ]
}

/// The state whose sample marker `is_said` by a body, if either is.
fn said(is_said: impl Fn(bool) -> bool) -> Option<UplinkState> {
    if is_said(true) {
        Some(UplinkState::Running)
    } else if is_said(false) {
        Some(UplinkState::Stopped)
    } else {
        None
    }
}

/// What a body under the state marker's opcode says: as long as the sample marker's body, and only the last byte is read.
fn state_marker_says(body: &[u8]) -> Option<UplinkState> {
    said(|running| {
        let marker = state_marker(running);
        let sample = marker.get(BODY_AT..);
        sample.is_some_and(|sample| sample.len() == body.len() && sample.last() == body.last())
    })
}

/// What a body under the session marker's opcode says: the whole body of the sample marker.
fn session_marker_says(body: &[u8]) -> Option<UplinkState> {
    said(|running| session_marker(running).get(BODY_AT..) == Some(body))
}

proptest! {
    #[test]
    fn classify_never_panics(b in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _outcome = classify(&b);
        let _opcode = opcode_of(&b);
    }

    // An opcode with a parser gets its result, a marker its state, 0x58 is handed over whole, any other is Other.
    #[test]
    fn classify_sorts_every_opcode(
        opcode in prop_oneof![proptest::sample::select(&ROUTED_OPCODES), any::<u16>()],
        body in body(),
    ) {
        let mut sdu = ACCESSORY_HEADER.to_vec();
        sdu.extend(opcode.to_le_bytes());
        sdu.extend(&body);
        let expected = match opcode {
            0x0004 => BatteryReport::parse(&sdu).map(Incoming::Battery),
            0x0006 => EarReport::parse(&sdu).map(Incoming::Ear),
            0x0009 => ControlCommand::parse(&sdu).map(Incoming::Control),
            0x001d => Metadata::parse(&sdu).map(Incoming::Metadata),
            0x0031 => ProximityKeys::parse(&sdu).map(Incoming::Keys),
            0x0058 => Ok(Incoming::Uplink(&sdu)),
            0x000e | 0x0055 => {
                let says = if opcode == 0x000e { state_marker_says(&body) } else { session_marker_says(&body) };
                Ok(says.map_or(Incoming::Other { opcode }, Incoming::UplinkState))
            }
            _ => Ok(Incoming::Other { opcode }),
        };
        prop_assert_eq!(classify(&sdu), expected);
        prop_assert_eq!(opcode_of(&sdu), Some(opcode));
    }

    #[test]
    fn classify_without_the_accessory_header_is_foreign(
        b in proptest::collection::vec(any::<u8>(), 0..ROUTED_BODY_BOUND)
            .prop_filter("starts with the accessory header", |b| !b.starts_with(&ACCESSORY_HEADER)),
    ) {
        prop_assert_eq!(classify(&b), Ok(Incoming::Foreign));
        prop_assert_eq!(opcode_of(&b), None);
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
