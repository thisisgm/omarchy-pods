//! Random input never panics a parser, and a parsed control command survives a round trip.

use pods_proto::battery::BatteryReport;
use pods_proto::control::ControlCommand;
use pods_proto::ear::EarReport;
use proptest::prelude::*;

// Sample output: 04 00 04 00 09 00 <id> <v0> <v1> <v2> <v3>, the control channel frame shape.
fn sample_frame(id: u8, v: [u8; 4]) -> Vec<u8> {
    let mut frame = vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, id];
    frame.extend(v);
    frame
}

const CONTROL_HEADER: [u8; 6] = [0x04, 0x00, 0x04, 0x00, 0x09, 0x00];

proptest! {
    #[test]
    fn battery_parse_never_panics(b in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _outcome = BatteryReport::parse(&b);
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
}
