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
