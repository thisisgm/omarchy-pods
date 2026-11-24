//! Support tests: the decode step with decoder calls that panic or give another length, which no unit can be made to do.

use std::iter;

use pods_samples::uplink::{silent_unit, tone_unit};

use super::*;

#[expect(clippy::as_conversions, reason = "a const context has no `From`")]
const UNIT: usize = SAMPLES_PER_UNIT as usize;

#[test]
fn a_panic_in_decode_is_caught_counted_and_resets_the_decoder() {
    let counters = Arc::new(Counters::default());
    let (mut chain, mut reader, resync) = chain(Arc::clone(&counters)).unwrap();
    let tone = tone_unit(10, 180);
    for stamp in [0, STAMP_STEP, 2 * STAMP_STEP] {
        chain.sdu(Duration::ZERO, iter::once((stamp, tone.as_slice())));
    }
    chain.unit(3 * STAMP_STEP, |decoder| {
        decoder.fenced(|_| panic!("the stand-in for the decoder panics"))
    });
    let silent = silent_unit();
    chain.sdu(
        Duration::ZERO,
        iter::once((4 * STAMP_STEP, silent.as_slice())),
    );
    let counted = [
        &counters.decoder_resets,
        &counters.concealed,
        &counters.decode_errors,
    ];
    assert_eq!(
        counted.map(|counter| counter.load(Ordering::Relaxed)),
        [1, 1, 0]
    );

    // Every SDU landed at zero, so the read that ends on the last sample written is the one a latency later.
    resync.request();
    let mut heard = [0.0_f32; 5 * UNIT];
    reader.read(
        Duration::from_millis(u64::from(NODE_LATENCY_MS)),
        &mut heard,
    );
    assert!(
        heard[3 * UNIT..4 * UNIT]
            .iter()
            .any(|&sample| sample != 0.0),
        "the unit that panicked is a block of zeros"
    );
    // A decoder that kept its state would let the tone ring on into the silent unit; a fresh one gives silence.
    assert!(
        heard[4 * UNIT..].iter().all(|&sample| sample == 0.0),
        "the decoder was not made again"
    );
}

#[test]
fn a_decoder_that_will_not_open_is_counted_and_the_held_one_stays() {
    let counters = Counters::default();
    let refused = || counters.decoder_renew_refused.load(Ordering::Relaxed);
    // No decoder can be made to refuse on order, so a number stands in for one.
    let mut held = 1_u8;
    renew(&mut held, &counters, Some(2));
    assert_eq!((held, refused()), (2, 0));
    renew(&mut held, &counters, None);
    assert_eq!((held, refused()), (2, 1));
}

#[test]
fn a_frame_of_another_length_is_refused() {
    let mut decoder = Decoder::new().unwrap();
    for length in [UNIT - 1, UNIT + 1] {
        let other = decoder.fenced(|_| Some(vec![0.0; length]));
        assert_eq!(other, Decoded::Error, "{length} samples");
    }
    assert_eq!(decoder.fenced(|_| None), Decoded::Error);
    let whole = decoder.fenced(|_| Some(vec![0.5; UNIT]));
    assert_eq!(whole, Decoded::Samples(&[0.5; UNIT]));
}

#[test]
fn the_reader_has_read_all_only_once_a_read_reaches_the_last_frame_written() {
    let (mut chain, mut reader, resync) = chain(Arc::default()).unwrap();
    let tone = tone_unit(10, 180);
    let stamps = [0, STAMP_STEP, 2 * STAMP_STEP];
    chain.sdu(
        Duration::ZERO,
        stamps.into_iter().map(|stamp| (stamp, tone.as_slice())),
    );
    let latency = Duration::from_millis(u64::from(NODE_LATENCY_MS));
    let unit_time = Duration::from_secs(u64::from(SAMPLES_PER_UNIT)) / RATE_HZ;
    let mut heard = [0.0_f32; UNIT];
    resync.request();
    // A latency less one unit after the landing, the read ends one unit short of the end.
    reader.read(latency.checked_sub(unit_time).unwrap(), &mut heard);
    assert!(!reader.read_all());
    reader.read(latency, &mut heard);
    assert!(reader.read_all());
    chain.sdu(
        Duration::ZERO,
        iter::once((3 * STAMP_STEP, tone.as_slice())),
    );
    assert!(
        !reader.read_all(),
        "a unit written after the read is not read"
    );
}

#[test]
fn a_stamp_two_packets_past_the_expected_one_is_the_last_gap_and_a_step_back_is_a_new_run() {
    let expected = Some(10 * STAMP_STEP);
    assert_eq!(lost_before(None, 7), Some(0));
    assert_eq!(lost_before(expected, 10 * STAMP_STEP), Some(0));
    assert_eq!(lost_before(expected, 18 * STAMP_STEP), Some(MAX_GAP_UNITS));
    assert_eq!(lost_before(expected, 19 * STAMP_STEP), None);
    assert_eq!(lost_before(expected, 0), None);
}

#[test]
fn the_writes_given_for_each_stamp_are_what_the_chain_writes() {
    let (mut chain, _reader, _resync) = chain(Arc::default()).unwrap();
    let tone = tone_unit(10, 180);
    // A run, one unit lost, a gap past the last the chain fills, a step back to zero, and on.
    let stamps = [0, 1, 3, 40, 41, 0, 1].map(|unit| unit * STAMP_STEP);
    for stamp in stamps {
        let before = chain.ring.end();
        let writes = unit_writes(chain.expected, stamp);
        chain.sdu(Duration::ZERO, iter::once((stamp, tone.as_slice())));
        let wrote = chain.ring.end().checked_sub(before).unwrap();
        assert_eq!(
            wrote,
            i64::from(writes.units * u32::from(SAMPLES_PER_UNIT)),
            "at {stamp}"
        );
        let run_from_here = chain.ring.run() == u64::try_from(before).unwrap();
        assert_eq!(run_from_here, writes.new_run || before == 0, "at {stamp}");
        assert_eq!(chain.expected, Some(stamp.wrapping_add(STAMP_STEP)));
    }
}
