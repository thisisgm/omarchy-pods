//! Support tests: the frame step with decoder calls that panic or refuse to conceal, which no frame can be made to do on order, and what the stage keeps of a frame, which no caller sees.

use std::iter;
use std::sync::Arc;

use pods_samples::opus::tone;

use super::*;
use crate::conceal::UNIT_FRAMES;
use crate::{Counters, NODE_LATENCY_MS, chain};

#[expect(clippy::as_conversions, reason = "a const context has no `From`")]
const FRAME: usize = FRAME_FRAMES as usize;
#[expect(clippy::as_conversions, reason = "a const context has no `From`")]
const UNIT: usize = UNIT_FRAMES as usize;
/// The longest fade, 5 ms at the node's rate; past it a sample is the decoder's own.
#[expect(clippy::as_conversions, reason = "a const context has no `From`")]
const FADE: usize = (RATE_HZ / 200) as usize;

/// What stands in for the frames of a run from its fourth on.
type Broken = fn(&mut Chain);

/// Three frames of the tone, `broken` in place of the next `stood_for` frames, then one more: what a recorder hears of them all, and the resets, the concealed and the errors.
fn around(broken: Broken, stood_for: u16) -> (Vec<f32>, [u32; 3]) {
    let counters = Arc::new(Counters::default());
    let (mut chain, mut reader, resync) = chain(Arc::clone(&counters)).unwrap();
    chain.dictation_run();
    let frames: Vec<&[u8]> = tone().take(5).collect();
    for (counter, frame) in (0_u16..).zip(&frames[..3]) {
        chain.frames(Duration::ZERO, iter::once((counter, *frame)));
    }
    broken(&mut chain);
    let after = stood_for.saturating_add(3);
    chain.dictation.expected = Some(after);
    chain.frames(Duration::ZERO, iter::once((after, frames[4])));

    // Every frame landed at zero, so the read that ends on the last sample written is the one a latency later.
    resync.request();
    let mut heard = vec![0.0; usize::from(after).saturating_add(1).saturating_mul(FRAME)];
    reader.read(
        Duration::from_millis(u64::from(NODE_LATENCY_MS)),
        &mut heard,
    );
    let counted = [
        &counters.decoder_resets,
        &counters.concealed,
        &counters.decode_errors,
    ];
    (
        heard,
        counted.map(|counter| counter.load(Ordering::Relaxed)),
    )
}

/// What the writer put in the ring for the two frames after a new run that follows `before` frames of the tone.
fn new_run_after(before: usize) -> Vec<u32> {
    let (mut chain, _reader, _resync) = chain(Arc::default()).unwrap();
    chain.dictation_run();
    for (counter, frame) in (0_u16..).zip(tone().take(before)) {
        chain.frames(Duration::ZERO, iter::once((counter, frame)));
    }
    chain.dictation_run();
    let from = chain.ring.end();
    for (counter, frame) in (0_u16..).zip(tone().skip(7).take(2)) {
        chain.frames(Duration::ZERO, iter::once((counter, frame)));
    }
    let run = chain.ring.run();
    (from..chain.ring.end())
        .map(|frame| chain.ring.sample(run, frame).to_bits())
        .collect()
}

#[test]
fn a_new_run_holds_nothing_of_the_run_before() {
    let alone = new_run_after(0);
    assert_eq!(alone.len(), 2 * FRAME);
    assert!(alone.iter().any(|&bits| bits != 0), "no audio came");
    // The decoder, the filter that brings its frames to the node's rate and the rise all start again: bit for bit what a chain that heard nothing writes.
    assert_eq!(new_run_after(3), alone);
}

#[test]
fn a_call_that_panics_or_will_not_conceal_ends_in_the_fall_and_a_fresh_decoder() {
    let cases: [(Broken, [u32; 3]); 3] = [
        (
            |chain| {
                chain.frame(3, |decoder| {
                    decoder.fenced(|_, _| panic!("the stand-in for the decoder panics"))
                });
            },
            [1, 1, 0],
        ),
        (
            |chain| chain.lost(|decoder| decoder.fenced(|_, _| None)),
            [0, 1, 0],
        ),
        (
            |chain| {
                chain.lost(|decoder| {
                    decoder.fenced(|_, _| panic!("the stand-in for the decoder panics"))
                });
            },
            [1, 1, 0],
        ),
    ];
    // What a decoder that has heard nothing makes of the fifth frame, at the node's rate.
    let mut fresh = Dictation::new().unwrap();
    let fifth = tone().nth(4).unwrap();
    let Decoded::Samples(alone) = fresh.at_node_rate(|decoder| decoder.frame(fifth)) else {
        panic!("the twin refused the frame");
    };
    for (case, (broken, counted)) in cases.into_iter().enumerate() {
        let (heard, counters) = around(broken, 1);
        assert_eq!(counters, counted, "case {case}");
        let (fall, after) = heard[3 * FRAME..].split_at(FRAME);
        assert!(
            fall[..UNIT].iter().any(|&sample| sample != 0.0),
            "case {case}: a block of zeros"
        );
        assert!(
            fall[UNIT..].iter().all(|&sample| sample == 0.0),
            "case {case}"
        );
        // The frame after rises from nothing, and past the rise it is a fresh decoder's.
        assert!(after[0].to_bits() == 0.0_f32.to_bits(), "case {case}");
        let bits = |samples: &[f32]| -> Vec<u32> { samples.iter().map(|s| s.to_bits()).collect() };
        assert_eq!(bits(&after[FADE..]), bits(&alone[FADE..]), "case {case}");
    }
}

#[test]
fn a_frame_lost_after_the_fall_is_silence() {
    // A concealment the decoder refuses, then a frame that never came: the fresh decoder conceals it to silence.
    let (heard, counters) = around(
        |chain| {
            chain.lost(|decoder| decoder.fenced(|_, _| None));
            chain.lost(Decoder::conceal);
        },
        2,
    );
    assert_eq!(counters, [0, 2, 0]);
    let (fall, lost) = heard[3 * FRAME..5 * FRAME].split_at(UNIT);
    assert!(fall.iter().any(|&sample| sample != 0.0), "a block of zeros");
    // Nothing the run held before the fall comes after it: every sample from its end to the next audio is zero.
    let loud = lost.iter().position(|&sample| sample != 0.0);
    assert_eq!(loud, None, "audio from before the fall");
}

#[test]
fn a_staged_frame_that_came_too_long_is_still_refused_when_placed() {
    let counters = Arc::new(Counters::default());
    let (mut chain, _reader, _resync) = chain(Arc::clone(&counters)).unwrap();
    chain.dictation_run();
    let long = [0; MAX_OPUS_FRAME_BYTES + 40];
    let plain = tone().next().unwrap();
    chain.frames(Duration::ZERO, [(0, plain), (1, &long[..])].into_iter());
    // The stage holds no more of it than tells it came too long.
    let staged = chain.dictation.stage.as_ref().unwrap();
    assert_eq!(staged[1].1.len(), MAX_OPUS_FRAME_BYTES + 1);
    chain.frames(Duration::ZERO, iter::once((2, plain)));
    assert!(chain.dictation.stage.is_none());
    let counted = [&counters.decode_errors, &counters.concealed];
    assert_eq!(
        counted.map(|counter| counter.load(Ordering::Relaxed)),
        [1, 1]
    );
}
