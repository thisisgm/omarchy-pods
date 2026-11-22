//! The candidate decoder behind its guard: what the real decoder gives, and what a call that panics costs.

use std::cell::Cell;

use super::super::super::analyse::support::SILENT_UNIT;
use super::super::super::fixture::TONE_UNIT;
use super::super::{EXPECTED_CONFIG, SAMPLES_PER_UNIT};
use super::*;

const UNITS: usize = 200;
const STAND_IN_FRAME: [f32; 1] = [0.25];

/// A decoder call that gives one sample, except the second call, which panics, and the fourth, which is an error.
fn faulty() -> Call {
    let calls = Cell::new(0_u32);
    Box::new(move |_unit| {
        calls.set(calls.get().saturating_add(1));
        match calls.get() {
            2 => panic!("the stand-in decoder panics on its second call"),
            4 => Err("the fourth call is an error".to_owned()),
            _ => Ok(STAND_IN_FRAME.to_vec()),
        }
    })
}

#[test]
fn candidate_decodes_silent_units_to_one_frame_of_zero_samples_each() {
    let mut candidate = Candidate::new(EXPECTED_CONFIG).unwrap();
    for unit in 0..UNITS {
        let UnitResult::Samples(samples) = candidate.unit(&SILENT_UNIT) else {
            panic!("unit {unit} gave no samples");
        };
        assert_eq!(samples.len(), usize::from(SAMPLES_PER_UNIT));
        assert!(samples.iter().all(|&sample| sample == 0.0), "unit {unit}");
    }
}

#[test]
fn candidate_decodes_a_unit_that_is_not_silence_to_sound_and_is_timed() {
    let frame = usize::from(SAMPLES_PER_UNIT);
    let mut candidate = Candidate::new(EXPECTED_CONFIG).unwrap();
    let tally = Tally::of(&mut candidate, &[&TONE_UNIT[..]; UNITS], frame);
    assert_eq!((tally.decoded, tally.errors, tally.panics), (UNITS, 0, 0));
    assert_eq!(tally.samples.len(), UNITS * frame);
    let loud = tally.samples.iter().filter(|sample| sample.abs() > 0.01);
    assert!(loud.count() > UNITS, "the decode is close to silence");
    assert!(tally.slowest > Duration::ZERO, "no call was timed");
    assert!(tally.total > tally.slowest, "{tally:?}");
}

#[test]
fn a_panicking_decoder_call_is_counted_not_fatal() {
    let mut candidate = Candidate { call: faulty() };
    let results: Vec<UnitResult> = (0..5).map(|_| candidate.unit(&SILENT_UNIT)).collect();
    let samples = || UnitResult::Samples(STAND_IN_FRAME.to_vec());
    assert_eq!(
        results,
        [
            samples(),
            UnitResult::Panic,
            samples(),
            UnitResult::Error("the fourth call is an error".to_owned()),
            samples(),
        ]
    );
}

#[test]
fn a_tally_counts_every_unit_once_and_keeps_the_first_error() {
    let mut candidate = Candidate { call: faulty() };
    let tally = Tally::of(&mut candidate, &[&SILENT_UNIT[..]; 5], STAND_IN_FRAME.len());
    assert_eq!((tally.decoded, tally.errors, tally.panics), (3, 1, 1));
    assert_eq!(tally.samples, [0.25; 3]);
    assert_eq!(
        tally.first_error.as_deref(),
        Some("the fourth call is an error")
    );
}

#[test]
fn samples_that_are_not_one_frame_are_an_error_and_are_left_out() {
    let mut candidate = Candidate { call: faulty() };
    let tally = Tally::of(&mut candidate, &[&SILENT_UNIT[..]; 5], 2);
    assert_eq!((tally.decoded, tally.errors, tally.panics), (0, 4, 1));
    assert_eq!(tally.samples, [0.0_f32; 0]);
    assert_eq!(
        tally.first_error.as_deref(),
        Some("1 samples from one unit, where a frame is 2")
    );
}

#[test]
fn only_a_call_longer_than_the_limit_is_slow() {
    let over = SLOW_CALL.saturating_add(Duration::from_nanos(1));
    let mut tally = Tally::default();
    tally.took(SLOW_CALL);
    assert_eq!((tally.slow, tally.slowest), (0, SLOW_CALL));
    tally.took(over);
    assert_eq!((tally.slow, tally.slowest), (1, over));
    tally.took(Duration::ZERO);
    assert_eq!((tally.slow, tally.slowest), (1, over));
    assert_eq!(tally.total, SLOW_CALL.saturating_add(over));
}

#[test]
fn a_configuration_that_is_no_configuration_is_refused_not_fatal() {
    assert!(Candidate::new([0x00; 4]).is_err());
}
