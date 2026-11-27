//! The comparison: what counts as a difference, how an offset is found, and what is never compared.

use super::*;

/// A frame short enough to write out, where nothing depends on the real frame length.
pub(crate) const FRAME: usize = 4;
/// One step of a 16 bit sample, as a float; `one_lsb_difference_passes` ties it to the scale of the comparison.
pub(crate) const ONE_LSB: f32 = 1.0 / 32768.0;
/// One frame as each of two decodes holds it.
pub(crate) type Laid = ([f32; FRAME], [f32; FRAME]);
/// A frame 2 LSB apart at a peak of 4000 LSB: past 1 LSB, and at half the share of its peak that still agrees.
pub(crate) const BETWEEN_THE_RULES: Laid = (
    [lsb(4000.0), lsb(1000.0), 0.0, 0.0],
    [lsb(4000.0), lsb(1002.0), 0.0, 0.0],
);
/// A frame 2 LSB apart at a peak of 8 LSB: past 1 LSB and a quarter of its peak.
pub(crate) const PAST_BOTH_RULES: Laid = (
    [lsb(8.0), lsb(4.0), 0.0, 0.0],
    [lsb(8.0), lsb(6.0), 0.0, 0.0],
);

const fn lsb(steps: f32) -> f32 {
    steps * ONE_LSB
}

/// `frames` frames whose samples all differ, so no frame matches another at any offset.
fn ramp(frames: usize) -> Vec<f32> {
    let samples = u16::try_from(frames.saturating_mul(FRAME)).unwrap();
    (1..=samples).map(|k| f32::from(k) / 1024.0).collect()
}

/// Two decodes of the fewest frames, equal but for frame 3 and those after it, which hold `cases` in order: the first decode one side of each, the second the other.
pub(crate) fn laid(cases: &[Laid]) -> (Vec<f32>, Vec<f32>) {
    let mut a = ramp(FEWEST_UNITS);
    let mut b = a.clone();
    let (in_a, in_b) = (a.as_chunks_mut::<FRAME>().0, b.as_chunks_mut::<FRAME>().0);
    let frames = in_a.iter_mut().zip(in_b);
    for ((in_a, in_b), (case_a, case_b)) in frames.skip(SEARCH_UNITS).zip(cases) {
        (*in_a, *in_b) = (*case_a, *case_b);
    }
    (a, b)
}

#[test]
fn a_frame_agrees_within_the_larger_of_one_lsb_and_the_share_of_its_peak() {
    let (past_one, past_share) = (lsb(7.0).next_up(), lsb(3004.0).next_up());
    // How far a sample is past a whole count of steps: the least a float can be past each limit.
    let over = |sample: f32, steps: f64| f64::from(sample) * LSB_PER_FULL_SCALE - steps;
    let (over_one, over_share) = (over(past_one, 6.0), over(past_share, 3000.0));
    assert!(over_one > 1.0 && over_one < 1.000_001, "{over_one}");
    assert!(over_share > 4.0 && over_share < 4.001, "{over_share}");
    let at = |peak: f32, a: f32, b: f32| ([lsb(peak), lsb(a), 0.0, 0.0], [lsb(peak), b, 0.0, 0.0]);
    // Each case: the frame in each decode, the largest difference, the largest share, the frames past 1 LSB and the frames over the gate.
    let cases = [
        ("silence", ([0.0; FRAME], [0.0; FRAME]), 0.0, 0.0, 0, 0),
        (
            "inside by 1 LSB alone",
            at(8.0, 2.0, lsb(2.5)),
            0.5,
            0.5 / 8.0,
            0,
            0,
        ),
        ("at 1 LSB", at(8.0, 4.0, lsb(5.0)), 1.0, 1.0 / 8.0, 0, 0),
        (
            "just past 1 LSB",
            at(8.0, 6.0, past_one),
            over_one,
            over_one / 8.0,
            1,
            1,
        ),
        (
            "inside by the share alone",
            BETWEEN_THE_RULES,
            2.0,
            2.0 / 4000.0,
            1,
            0,
        ),
        (
            "at the share",
            at(4000.0, 2000.0, lsb(2004.0)),
            4.0,
            4.0 / 4000.0,
            1,
            0,
        ),
        (
            "just past the share",
            at(4000.0, 3000.0, past_share),
            over_share,
            over_share / 4000.0,
            1,
            1,
        ),
        (
            "at the share of a peak one decode holds",
            ([lsb(4000.0), 0.0, 0.0, 0.0], [lsb(3996.0), 0.0, 0.0, 0.0]),
            4.0,
            4.0 / 4000.0,
            1,
            0,
        ),
        ("past both", PAST_BOTH_RULES, 2.0, 2.0 / 8.0, 1, 1),
        (
            "not a number",
            at(100.0, 0.0, f32::NAN),
            f64::INFINITY,
            f64::INFINITY,
            1,
            1,
        ),
        (
            "infinite",
            at(100.0, 0.0, f32::NEG_INFINITY),
            f64::INFINITY,
            f64::INFINITY,
            1,
            1,
        ),
    ];
    for (name, case, max_lsb, max_of_peak, frames_over_one_lsb, frames_over_gate) in cases {
        let expected = Comparison {
            // The case is the one frame the offsets are judged on, so a case that holds no number names no offset.
            offset_units: max_lsb.is_finite().then_some(0),
            frames: FEWEST_UNITS,
            max_lsb,
            frames_over_one_lsb,
            max_of_peak,
            frames_over_gate,
        };
        let (a, b) = laid(&[case]);
        assert_eq!(compare(&a, &b, FRAME), Ok(expected), "{name}");
        // The peak is that of either decode, so the two change places and nothing else does.
        assert_eq!(
            compare(&b, &a, FRAME),
            Ok(expected),
            "{name}, the other way"
        );
    }
    // The share of a peak of 4000 LSB is 4 LSB to the last bit, so the frame 4 LSB apart there is at the limit and not near it.
    assert_eq!(AGREEMENT_OF_PEAK * 4000.0, 4.0);
}

#[test]
fn every_frame_over_each_rule_is_counted_under_that_rule() {
    let (a, b) = laid(&[BETWEEN_THE_RULES, PAST_BOTH_RULES, BETWEEN_THE_RULES]);
    let found = compare(&a, &b, FRAME).unwrap();
    assert_eq!((found.frames_over_one_lsb, found.frames_over_gate), (3, 1));
    assert_eq!((found.max_lsb, found.max_of_peak), (2.0, 2.0 / 8.0));
}

#[test]
fn two_lsb_difference_fails() {
    let a = ramp(FEWEST_UNITS);
    let mut b = a.clone();
    b[5] += 2.0 * ONE_LSB;
    assert_eq!(
        compare(&a, &b, FRAME),
        Ok(Comparison {
            offset_units: Some(0),
            frames: FEWEST_UNITS,
            max_lsb: 2.0,
            frames_over_one_lsb: 1,
            max_of_peak: 2.0 / 256.0,
            frames_over_gate: 1,
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
        (Some(1), 9, 0.0)
    );
    let found = compare(&ahead, &behind, FRAME).unwrap();
    assert_eq!(
        (found.offset_units, found.frames, found.max_lsb),
        (Some(-1), 9, 0.0)
    );
}

#[test]
fn equal_streams_are_compared_whole_at_no_offset() {
    let silence = vec![0.0_f32; 10 * FRAME];
    let found = compare(&silence, &silence, FRAME).unwrap();
    assert_eq!((found.offset_units, found.frames), (Some(0), 10));
    // Silence has no peak to take a share of, and no frame of it is outside.
    assert_eq!((found.max_of_peak, found.frames_over_gate), (0.0, 0));
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
    assert_eq!((found.offset_units, found.max_lsb), (Some(1), 0.0));
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
    assert_eq!((found.offset_units, found.frames), (Some(0), FEWEST_UNITS));
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
fn a_sample_that_is_not_a_number_moves_no_offset() {
    let ahead = ramp(20);
    let behind = [&[0.0; FRAME][..], &ahead[..19 * FRAME]].concat();
    for (a, b, offset) in [
        (&ahead, &ahead, 0),
        (&behind, &ahead, 1),
        (&ahead, &behind, -1),
    ] {
        // One sample of each frame in turn, in the first decode and then in the second.
        for frame in 0..20 {
            let (mut bad_a, mut bad_b) = (a.clone(), b.clone());
            (bad_a[frame * FRAME], bad_b[frame * FRAME]) = (f32::NAN, f32::NAN);
            for (which, found) in [
                ("first", compare(&bad_a, b, FRAME).unwrap()),
                ("second", compare(a, &bad_b, FRAME).unwrap()),
            ] {
                assert_eq!(found.offset_units, Some(offset), "{which}, frame {frame}");
                // Where the two decodes are level every frame is compared, the one that holds it too.
                assert!(
                    offset != 0 || found.max_lsb.is_infinite(),
                    "{which} {frame}"
                );
            }
        }
    }
}

#[test]
fn a_decode_that_stops_being_a_number_moves_no_offset_and_names_none_with_no_frame_left() {
    let ahead = ramp(20);
    let behind = [&[0.0; FRAME][..], &ahead[..19 * FRAME]].concat();
    for (a, b, offset) in [
        (&ahead, &ahead, 0),
        (&behind, &ahead, 1),
        (&ahead, &behind, -1),
    ] {
        // The first decode gone from frame 5 leaves no frame to judge on and from frame 12 it leaves six; the second gone leaves two and nine.
        for (from, first, second) in [(5, None, Some(offset)), (12, Some(offset), Some(offset))] {
            let (mut bad_a, mut bad_b) = (a.clone(), b.clone());
            bad_a[from * FRAME..].fill(f32::NAN);
            bad_b[from * FRAME..].fill(f32::NAN);
            let found = compare(&bad_a, b, FRAME).unwrap();
            assert_eq!(found.offset_units, first, "first, from frame {from}");
            // With no offset named the decodes are compared level, every frame of them.
            assert!(first.is_some() || found.frames == 20, "{}", found.frames);
            let found = compare(a, &bad_b, FRAME).unwrap();
            assert_eq!(found.offset_units, second, "second, from frame {from}");
        }
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

#[test]
fn quality_metrics_reports_exact_max_rms_and_snr() {
    let oracle = [0.5_f32, -0.5, 0.5, -0.5];
    let exact = quality_metrics(&oracle, &oracle, FRAME, 0);
    assert_eq!(exact.frames, 1);
    assert_eq!(exact.max_abs, 0.0);
    assert_eq!(exact.rms_error, 0.0);
    assert!(exact.snr_db.is_infinite() && exact.snr_db.is_sign_positive());

    let candidate = [0.5_f32 + ONE_LSB, -0.5, 0.5, -0.5];
    let q = quality_metrics(&candidate, &oracle, FRAME, 0);
    assert_eq!(q.frames, 1);
    assert!((q.max_lsb - 1.0).abs() < 1e-6, "{q:?}");
    assert!(
        (q.rms_error - f64::from(ONE_LSB) / 2.0).abs() < 1e-10,
        "{q:?}"
    );
    assert!(q.snr_db > 90.0 && q.snr_db < 91.0, "{q:?}");
}
