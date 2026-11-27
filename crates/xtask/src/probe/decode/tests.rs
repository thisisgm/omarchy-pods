//! The units a log gives up, and the peak of a decode.

use pods_proto::uplink::START;

use super::super::analyse::support::{OPAQUE, audio, from_pods, to_pods};
use super::compare::compare;
use super::compare::tests::{BETWEEN_THE_RULES, FRAME, PAST_BOTH_RULES, laid};
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

#[test]
fn the_comparison_rows_count_the_frames_past_one_lsb_whatever_their_peak() {
    // Written by hand: one frame past 1 LSB and inside the share of its peak, and one past both.
    let (candidate, oracle) = laid(&[BETWEEN_THE_RULES, PAST_BOTH_RULES]);
    let compared = compare(&candidate, &oracle, FRAME).map_err(|error| error.to_string());
    assert_eq!(compared.as_ref().map(|found| found.frames_over_gate), Ok(1));
    let rows = compared_rows(&compared);
    let found: Vec<(&str, &str)> = rows
        .iter()
        .map(|row| (row.key.as_str(), row.value.as_str()))
        .collect();
    let expected = [
        ("frames_compared", "7"),
        ("offset_units", "0"),
        ("max_lsb", "2.000"),
        ("frames_over_one_lsb", "2"),
    ];
    assert_eq!(found, expected);

    let refused = compared_rows(&Err("no decode".to_owned()));
    assert_eq!(refused.len(), expected.len());
    for row in refused {
        assert_eq!(
            (row.value.as_str(), row.note.as_str()),
            (REFUSED, "no decode")
        );
    }
}

#[test]
fn a_comparison_no_frame_could_judge_names_no_offset_and_keeps_its_other_rows() {
    // The one frame the offsets are judged on holds no number in the oracle.
    let (candidate, oracle) = laid(&[([0.0; FRAME], [f32::NAN; FRAME])]);
    let compared = compare(&candidate, &oracle, FRAME).map_err(|error| error.to_string());
    let rows = compared_rows(&compared);
    let found: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|row| (row.key.as_str(), row.value.as_str(), row.note.as_str()))
        .collect();
    assert_eq!(found[1], ("offset_units", REFUSED, NO_OFFSET));
    let values: Vec<&str> = found.iter().map(|(_, value, _)| *value).collect();
    assert_eq!(values, ["7", REFUSED, "inf", "1"]);

    for row in quality_rows(&candidate, &oracle, FRAME, &compared) {
        assert_eq!(
            (row.value.as_str(), row.note.as_str()),
            (REFUSED, NO_OFFSET)
        );
    }
}

#[test]
fn the_quality_rows_lay_the_candidate_at_the_compared_offset_and_refuse_without_a_comparison() {
    // Every oracle frame differs, and the candidate holds them one unit late with one sample 1 LSB off.
    let oracle: Vec<f32> = (1_u8..=8)
        .flat_map(|unit| [f32::from(unit) / 16.0, f32::from(unit) / -16.0, 0.0, 0.0])
        .collect();
    let mut candidate = vec![0.0_f32; FRAME];
    candidate.extend_from_slice(&oracle);
    candidate[FRAME] += compare::tests::ONE_LSB;
    let compared = compare(&candidate, &oracle, FRAME).map_err(|error| error.to_string());
    assert_eq!(
        compared.as_ref().map(|found| found.offset_units),
        Ok(Some(1))
    );

    let rows = quality_rows(&candidate, &oracle, FRAME, &compared);
    // 32 samples lie on the oracle, one of them 1 LSB off.
    let rms = f64::from(compare::tests::ONE_LSB) / 32.0_f64.sqrt();
    assert_eq!(rows[0].key, "rms_error");
    assert_eq!(rows[0].value, format!("{rms:.3e}"));
    assert_eq!(rows[1].key, "snr_db");
    let snr: f64 = rows[1].value.parse().unwrap();
    assert!(snr.is_finite() && snr > 0.0, "{snr}");

    for row in quality_rows(&candidate, &oracle, FRAME, &Err("no decode".to_owned())) {
        assert_eq!(
            (row.value.as_str(), row.note.as_str()),
            (REFUSED, "no decode")
        );
    }
}
