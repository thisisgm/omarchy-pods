//! The units a log gives up, and the peak of a decode.

use pods_proto::uplink::START;

use super::super::analyse::support::{OPAQUE, audio, from_pods, to_pods};
use super::*;

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
