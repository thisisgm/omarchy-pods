//! Random input never panics a parser, and a parsed control command survives a round trip.

use pods_proto::battery::BatteryReport;
use pods_proto::control::ControlCommand;
use pods_proto::ear::EarReport;
use pods_proto::error::{ParseError, Parser};
use pods_proto::uplink::{
    AUDIO_HEADER_LEN, AccessUnit, OPAQUE_LEN, UNIT_PREFIX_LEN, UplinkPacket, encode_audio,
};
use proptest::prelude::*;

// Sample output: 04 00 04 00 09 00 <id> <v0> <v1> <v2> <v3>, the control channel frame shape.
fn sample_frame(id: u8, v: [u8; 4]) -> Vec<u8> {
    let mut frame = vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, id];
    frame.extend(v);
    frame
}

const CONTROL_HEADER: [u8; 6] = [0x04, 0x00, 0x04, 0x00, 0x09, 0x00];
const BATTERY_HEADER: [u8; 6] = [0x04, 0x00, 0x04, 0x00, 0x04, 0x00];
const EAR_HEADER: [u8; 6] = [0x04, 0x00, 0x04, 0x00, 0x06, 0x00];
const UPLINK_AUDIO_GATE: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x01, 0x00];
const UPLINK_CONTROL_GATE: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x00, 0x00];
/// Length bound of a generated SDU, room for sixteen access units of the longest kind.
const MAX_SDU: usize = 4200;
/// Bytes 8 and 9 of an uplink control packet, its declared length.
const DECLARED_LEN_BYTES: usize = 2;
/// The gate plus the declared length, the shortest uplink control packet.
const UPLINK_CONTROL_MIN_LEN: usize = 10;
/// Where the length byte sits in an access unit, after the four stamp bytes.
const UNIT_LENGTH_AT: usize = 4;

// Sample output: [04, 01, 55, 02, 01], kind, spacer, level, status, end; marks are mostly 0x01 so the entry loop runs.
fn battery_entry() -> impl Strategy<Value = [u8; 5]> {
    let kind = prop_oneof![
        Just(0x01_u8),
        Just(0x02),
        Just(0x04),
        Just(0x08),
        any::<u8>()
    ];
    let status = prop_oneof![Just(0x01_u8), Just(0x02), Just(0x04), any::<u8>()];
    let mark = prop_oneof![9 => Just(0x01_u8), 1 => any::<u8>()];
    (kind, mark.clone(), any::<u8>(), status, mark)
        .prop_map(|(kind, spacer, level, status, end)| [kind, spacer, level, status, end])
}

proptest! {
    #[test]
    fn battery_parse_never_panics(b in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _outcome = BatteryReport::parse(&b);
    }

    #[test]
    fn battery_parse_never_panics_behind_the_header(body in proptest::collection::vec(any::<u8>(), 0..64)) {
        let mut packet = BATTERY_HEADER.to_vec();
        packet.extend(body);
        let _outcome = BatteryReport::parse(&packet);
    }

    // A count that agrees with the entries and marks of 0x01 parses; a nearby count or length is refused.
    #[test]
    fn battery_parse_walks_the_entries(
        entries in proptest::collection::vec(battery_entry(), 0..=4),
        count_shift in prop_oneof![8 => Just(0_u8), 1 => Just(1), 1 => Just(u8::MAX)],
        trailing in prop_oneof![8 => Just(Vec::new()), 2 => proptest::collection::vec(any::<u8>(), 1..5)],
    ) {
        let count = u8::try_from(entries.len()).unwrap().wrapping_add(count_shift);
        let mut packet = BATTERY_HEADER.to_vec();
        packet.push(count);
        packet.extend(entries.iter().flatten());
        packet.extend(&trailing);
        let marks_valid = entries.iter().all(|e| e[1] == 0x01 && e[4] == 0x01);
        let well_formed = count_shift == 0 && trailing.is_empty() && entries.len() <= 3 && marks_valid;
        prop_assert_eq!(BatteryReport::parse(&packet).is_ok(), well_formed);
    }

    #[test]
    fn ear_parse_reads_any_body_behind_the_header(body in proptest::collection::vec(any::<u8>(), 0..16)) {
        let mut packet = EAR_HEADER.to_vec();
        packet.extend(&body);
        prop_assert_eq!(EarReport::parse(&packet).is_ok(), body.len() >= 2);
    }

    #[test]
    fn ear_parse_never_panics(b in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _outcome = EarReport::parse(&b);
    }

    #[test]
    fn control_parse_never_panics(b in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _outcome = ControlCommand::parse(&b);
    }

    #[test]
    fn control_round_trips(id in any::<u8>(), v in any::<[u8; 4]>()) {
        let c = ControlCommand::parse(&sample_frame(id, v)).unwrap();
        prop_assert_eq!(c.encode(), sample_frame(id, v));
        prop_assert_eq!(ControlCommand::parse(&c.encode()).unwrap(), c);
    }

    #[test]
    fn decrypted_battery_parse_never_panics(
        b in proptest::collection::vec(any::<u8>(), 0..64),
        primary_left in any::<bool>(),
        pod_in_case in any::<bool>(),
        headset in any::<bool>(),
    ) {
        let _outcome = BatteryReport::parse_decrypted(&b, primary_left, pod_in_case, headset);
    }

    #[test]
    fn decrypted_battery_parse_reads_every_sixteen_byte_payload(
        payload in any::<[u8; 16]>(),
        primary_left in any::<bool>(),
        pod_in_case in any::<bool>(),
        headset in any::<bool>(),
    ) {
        prop_assert!(BatteryReport::parse_decrypted(&payload, primary_left, pod_in_case, headset).is_ok());
    }

    // tst_stress parseActive_fuzzShape: pure random, header plus tail, and a short prefix of the header.
    #[test]
    fn parse_active_fuzz_shape(
        random in proptest::collection::vec(any::<u8>(), 0..=20),
        tail in proptest::collection::vec(any::<u8>(), 0..=16),
        prefix_len in 0..=6_usize,
    ) {
        let _random = ControlCommand::parse(&random);
        let mut with_header = CONTROL_HEADER.to_vec();
        with_header.extend(tail);
        let _with_header = ControlCommand::parse(&with_header);
        let _prefix = ControlCommand::parse(&CONTROL_HEADER[..prefix_len]);
    }

    // tst_stress parseState_fuzzValueByte: state is Some only for 0x01 and 0x02.
    #[test]
    fn parse_state_fuzz_value_byte(
        id in any::<u8>(),
        value in any::<u8>(),
        tail in proptest::collection::vec(any::<u8>(), 0..8),
    ) {
        let mut buf = CONTROL_HEADER.to_vec();
        buf.extend([id, value]);
        buf.extend(tail);
        let state = ControlCommand::parse(&buf).unwrap().state();
        match value {
            0x01 => prop_assert_eq!(state, Some(true)),
            0x02 => prop_assert_eq!(state, Some(false)),
            _ => prop_assert_eq!(state, None),
        }
    }

    #[test]
    fn uplink_parse_never_panics(b in proptest::collection::vec(any::<u8>(), 0..MAX_SDU)) {
        let _outcome = UplinkPacket::parse(&b);
    }

    // The yielded units encode back to the front of the SDU, and trailing counts a tail that holds no whole unit.
    #[test]
    fn uplink_parse_past_the_header_never_panics(body in proptest::collection::vec(any::<u8>(), 0..MAX_SDU)) {
        let mut sdu = UPLINK_AUDIO_GATE.to_vec();
        sdu.extend(&body);
        match UplinkPacket::parse(&sdu) {
            Ok(Some(UplinkPacket::Audio(audio))) => {
                let mut units = audio.units();
                let whole: Vec<_> = units.by_ref().collect();
                let rebuilt = encode_audio(audio.opaque, &whole).unwrap();
                prop_assert!(sdu.starts_with(&rebuilt));
                let tail = &sdu[rebuilt.len()..];
                prop_assert_eq!(units.trailing(), tail.len());
                let cut_short = tail.get(UNIT_PREFIX_LEN..).is_none_or(|data| data.len() < usize::from(tail[UNIT_LENGTH_AT]));
                prop_assert!(cut_short, "a whole unit was left in the tail {:02x?}", tail);
            }
            other => {
                prop_assert!(body.len() < OPAQUE_LEN, "{:?}", other);
                let too_short = ParseError::TooShort { parser: Parser::UplinkAudio, need: AUDIO_HEADER_LEN, got: sdu.len() };
                prop_assert_eq!(other, Err(too_short));
            }
        }
    }

    #[test]
    fn uplink_control_keeps_every_byte_behind_the_gate(body in proptest::collection::vec(any::<u8>(), 0..MAX_SDU)) {
        let mut sdu = UPLINK_CONTROL_GATE.to_vec();
        sdu.extend(&body);
        match UplinkPacket::parse(&sdu) {
            Ok(Some(UplinkPacket::Control(control))) => {
                prop_assert_eq!(control.declared_len, u16::from_le_bytes([body[0], body[1]]));
                prop_assert_eq!(control.payload, &body[DECLARED_LEN_BYTES..]);
                prop_assert_eq!(control.length_matches(), usize::from(control.declared_len) == control.payload.len());
            }
            other => {
                prop_assert!(body.len() < DECLARED_LEN_BYTES, "{:?}", other);
                let too_short = ParseError::TooShort { parser: Parser::UplinkControl, need: UPLINK_CONTROL_MIN_LEN, got: sdu.len() };
                prop_assert_eq!(other, Err(too_short));
            }
        }
    }

    // Whole units survive a round trip; a cut SDU yields every unit that still fits and counts the rest.
    #[test]
    fn uplink_audio_round_trips_and_counts_a_cut_tail(
        opaque in any::<[u8; OPAQUE_LEN]>(),
        payloads in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..=255), 0..8),
        stamps in any::<[u32; 8]>(),
        cut in any::<proptest::sample::Index>(),
    ) {
        let units: Vec<AccessUnit<'_>> = stamps.iter().zip(&payloads).map(|(&stamp, data)| AccessUnit { stamp, data }).collect();
        let sdu = encode_audio(&opaque, &units).unwrap();
        let Ok(Some(UplinkPacket::Audio(audio))) = UplinkPacket::parse(&sdu) else {
            return Err(TestCaseError::fail("an encoded SDU did not parse as audio"));
        };
        prop_assert_eq!(audio.opaque, &opaque);
        let mut seen = audio.units();
        prop_assert_eq!(&seen.by_ref().collect::<Vec<_>>(), &units);
        prop_assert_eq!(seen.trailing(), 0);

        let kept = cut.index(sdu.len()).max(AUDIO_HEADER_LEN);
        let Ok(Some(UplinkPacket::Audio(cut_audio))) = UplinkPacket::parse(&sdu[..kept]) else {
            return Err(TestCaseError::fail("a cut SDU with a whole header did not parse as audio"));
        };
        let mut cut_units = cut_audio.units();
        let whole: Vec<_> = cut_units.by_ref().collect();
        prop_assert_eq!(whole.as_slice(), &units[..whole.len()]);
        let rebuilt = encode_audio(&opaque, &whole).unwrap();
        prop_assert_eq!(kept.checked_sub(rebuilt.len()), Some(cut_units.trailing()));
        if let Some(one_more) = units.get(..=whole.len()) {
            prop_assert!(encode_audio(&opaque, one_more).unwrap().len() > kept, "a unit that fits was not yielded");
        }
    }
}
