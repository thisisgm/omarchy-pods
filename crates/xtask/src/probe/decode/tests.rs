//! The rate a decode is labelled with, and the units a log gives up.

use pods_proto::uplink::START;

use super::super::analyse::support::{OPAQUE, audio, from_pods, to_pods};
use super::*;

#[test]
fn the_label_is_the_rate_every_clock_is_within_one_percent_of() {
    assert_eq!(label(&["63998.412"]).unwrap(), 64_000);
    assert_eq!(label(&["64639.000", "63361.000"]).unwrap(), 64_000);
    assert_eq!(label(&["48000.000"]).unwrap(), 48_000);
    assert_eq!(label(&["47520.100"]).unwrap(), 48_000);
}

#[test]
fn a_clock_that_is_neither_rate_or_was_not_measured_is_refused() {
    for hz in [
        "56000.000",
        "64641.000",
        "63359.000",
        "47519.000",
        "NaN",
        "0",
    ] {
        let refused = label(&[hz]).unwrap_err();
        assert!(
            matches!(&refused, DecodeError::NeitherRate { hz: named } if named == hz),
            "{refused}"
        );
    }
    let refused = label(&["64000.000", "refused"]).unwrap_err();
    assert!(
        matches!(refused, DecodeError::ClockNotMeasured { .. }),
        "{refused}"
    );
    let refused = label(&["64000.000", "48000.000"]).unwrap_err();
    assert!(matches!(refused, DecodeError::ClocksDisagree), "{refused}");
    assert!(matches!(label(&[]).unwrap_err(), DecodeError::NoClock));
}

#[test]
fn the_units_are_those_of_every_audio_sdu_from_the_pods_in_order() {
    let records = [
        to_pods(1, &START),
        audio(2, &OPAQUE, &[(0, &[0xa1]), (480, &[0xb1, 0xb2])]),
        from_pods(3, &[0x04, 0x00, 0x04, 0x00, 0x1d, 0x00]),
        audio(4, &OPAQUE, &[(960, &[]), (1440, &[0xc1])]),
        // A unit the host sent is not one the pods sent.
        to_pods(5, &audio(5, &OPAQUE, &[(0, &[0xd1])]).sdu),
    ];
    let units = coded_units(&records);
    assert_eq!(units, [&[0xa1][..], &[0xb1, 0xb2], &[], &[0xc1]]);
}

#[test]
fn the_peak_is_the_largest_sample_and_a_decode_of_no_sample_has_none() {
    assert_eq!(peak_lsb(&[0.25, -0.5, 0.125]), Some(16384.0));
    assert_eq!(peak_lsb(&[0.5, f32::NAN]), Some(f64::INFINITY));
    assert_eq!(peak_lsb(&[]), None);
}
