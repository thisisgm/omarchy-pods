//! The comparison: what counts as a difference, how an offset is found, and what is never compared.

use super::*;

/// A frame short enough to write out, where nothing depends on the real frame length.
const FRAME: usize = 4;
/// One step of a 16 bit sample, as a float; `one_lsb_difference_passes` ties it to the scale of the comparison.
pub(crate) const ONE_LSB: f32 = 1.0 / 32768.0;

/// `frames` frames whose samples all differ, so no frame matches another at any offset.
fn ramp(frames: usize) -> Vec<f32> {
    let samples = u16::try_from(frames.saturating_mul(FRAME)).unwrap();
    (1..=samples).map(|k| f32::from(k) / 1024.0).collect()
}

#[test]
fn two_lsb_difference_fails() {
    let a = ramp(FEWEST_UNITS);
    let mut b = a.clone();
    b[5] += 2.0 * ONE_LSB;
    assert_eq!(
        compare(&a, &b, FRAME),
        Ok(Comparison {
            offset_units: 0,
            frames: FEWEST_UNITS,
            max_lsb: 2.0,
            frames_over_one_lsb: 1,
        })
    );
}

#[test]
fn one_lsb_difference_passes() {
    let a = vec![0.5_f32; FEWEST_UNITS * FRAME];
    let mut b = a.clone();
    b[5] += ONE_LSB;
    let same = compare(&a, &b, FRAME).unwrap();
    assert_eq!(same.max_lsb, 1.0);
    assert_eq!(same.frames_over_one_lsb, 0);
    assert_eq!(same.frames, FEWEST_UNITS);
}

#[test]
fn every_frame_that_differs_is_counted_once() {
    let a = ramp(FEWEST_UNITS);
    let mut b = a.clone();
    for at in [0, 1, 2, 3, 13] {
        b[at] -= 3.0 * ONE_LSB;
    }
    let found = compare(&a, &b, FRAME).unwrap();
    assert_eq!((found.frames_over_one_lsb, found.max_lsb), (2, 3.0));
}

#[test]
fn a_one_unit_offset_is_found_and_reported() {
    let ahead = ramp(10);
    let behind = [&[0.0; FRAME][..], &ahead[..9 * FRAME]].concat();
    let found = compare(&behind, &ahead, FRAME).unwrap();
    assert_eq!(
        (found.offset_units, found.frames, found.max_lsb),
        (1, 9, 0.0)
    );
    let found = compare(&ahead, &behind, FRAME).unwrap();
    assert_eq!(
        (found.offset_units, found.frames, found.max_lsb),
        (-1, 9, 0.0)
    );
}

#[test]
fn equal_streams_are_compared_whole_at_no_offset() {
    let silence = vec![0.0_f32; 10 * FRAME];
    let found = compare(&silence, &silence, FRAME).unwrap();
    assert_eq!((found.offset_units, found.frames), (0, 10));
}

#[test]
fn frame_count_mismatch_is_an_error() {
    let long = vec![0.0_f32; 10 * FRAME];
    let apart = compare(&long, &long[..6 * FRAME], FRAME);
    assert_eq!(apart, Err(CompareError::TooFarApart { a: 10, b: 6 }));
    let ragged = compare(&long, &long[..=9 * FRAME], FRAME);
    assert_eq!(
        ragged,
        Err(CompareError::NotWholeUnits {
            samples: 9 * FRAME + 1,
            samples_per_unit: FRAME
        })
    );
    // Three units apart is the most an offset can explain.
    assert_eq!(compare(&long, &long[..7 * FRAME], FRAME).unwrap().frames, 7);
}

#[test]
fn streams_too_short_to_judge_an_offset_on_are_an_error_not_equal() {
    let too_short = |a, b| Err(CompareError::TooShort { a, b });
    assert_eq!(compare(&[], &[], FRAME), too_short(0, 0));
    assert_eq!(compare(&[0.0; FRAME], &[], FRAME), too_short(1, 0));

    // The shortest streams compared still have their offset found; one unit fewer and nothing judges it.
    let ahead = ramp(FEWEST_UNITS);
    let behind = [&[0.0; FRAME][..], &ahead[..(FEWEST_UNITS - 1) * FRAME]].concat();
    let found = compare(&behind, &ahead, FRAME).unwrap();
    assert_eq!((found.offset_units, found.max_lsb), (1, 0.0));
    let cut = (FEWEST_UNITS - 1) * FRAME;
    assert_eq!(
        compare(&behind[..cut], &ahead[..cut], FRAME),
        too_short(FEWEST_UNITS - 1, FEWEST_UNITS - 1)
    );
}

#[test]
fn streams_that_differ_everywhere_are_compared_whole_at_no_offset() {
    let loud = vec![0.5_f32; FEWEST_UNITS * FRAME];
    let silent = vec![0.0_f32; FEWEST_UNITS * FRAME];
    let found = compare(&loud, &silent, FRAME).unwrap();
    assert_eq!((found.offset_units, found.frames), (0, FEWEST_UNITS));
    assert_eq!(
        (found.max_lsb, found.frames_over_one_lsb),
        (16384.0, FEWEST_UNITS)
    );
}

#[test]
fn every_offset_two_decodes_may_differ_by_is_tried() {
    let furthest = i32::try_from(SEARCH_UNITS).unwrap();
    let mut tried = OFFSETS;
    tried.sort_unstable();
    assert_eq!(tried.to_vec(), (-furthest..=furthest).collect::<Vec<i32>>());
}

#[test]
fn a_sample_that_is_not_a_number_is_a_difference() {
    let a = vec![0.0_f32; FEWEST_UNITS * FRAME];
    for bad in [f32::NAN, f32::INFINITY] {
        let mut b = a.clone();
        b[1] = bad;
        let found = compare(&a, &b, FRAME).unwrap();
        assert_eq!(found.frames_over_one_lsb, 1, "{bad}");
        assert_eq!(found.max_lsb, f64::INFINITY, "{bad}");
    }
}

#[test]
fn a_frame_length_of_zero_is_refused() {
    let refused = compare(&[0.0], &[0.0], 0);
    assert_eq!(
        refused,
        Err(CompareError::NotWholeUnits {
            samples: 1,
            samples_per_unit: 0
        })
    );
}
