//! The Opus candidate behind its guard, where no command can reach: what a call that panics costs, and what a decoder that miscounts gives.

use std::cell::Cell;

use pods_proto::dictation::TOC_CELT_WIDEBAND_20MS_MONO;

use super::*;

/// The one sample every call of the stand-in decoder gives.
const STAND_IN_SAMPLE: f32 = 0.25;

/// A decoder call that gives one sample, except the second call, which panics, the fourth, which is an error, and the fifth, which claims more samples than the buffer holds.
fn faulty() -> Call {
    let calls = Cell::new(0_u32);
    Box::new(move |_packet, pcm| {
        calls.set(calls.get().saturating_add(1));
        match calls.get() {
            2 => panic!("the stand-in decoder panics on its second call"),
            4 => Err("the fourth call is an error".to_owned()),
            5 => Ok(pcm.len().saturating_add(1)),
            _ => {
                pcm[0] = STAND_IN_SAMPLE;
                Ok(1)
            }
        }
    })
}

#[test]
fn a_panicking_decoder_call_is_counted_not_fatal() {
    let mut candidate = Candidate::calling(faulty(), OutputRate::Hz48000);
    let frame = [TOC_CELT_WIDEBAND_20MS_MONO];
    let results: Vec<FrameResult> = (0..6).map(|_| candidate.frame(&frame)).collect();
    let samples = || FrameResult::Samples(vec![STAND_IN_SAMPLE]);
    assert_eq!(
        results,
        [
            samples(),
            FrameResult::Panic,
            samples(),
            FrameResult::Error("the fourth call is an error".to_owned()),
            FrameResult::Error("5761 samples in a buffer of 5760".to_owned()),
            samples(),
        ]
    );
}

#[test]
fn concealment_asks_the_decoder_for_one_frame_and_no_packet() {
    for rate in [OutputRate::Hz16000, OutputRate::Hz48000] {
        let asked = Box::new(|packet: Option<&[u8]>, pcm: &mut [f32]| {
            assert_eq!(packet, None);
            Ok(pcm.len())
        });
        let mut candidate = Candidate::calling(asked, rate);
        let FrameResult::Samples(samples) = candidate.conceal() else {
            panic!("no samples at {rate:?}");
        };
        assert_eq!(samples.len(), usize::from(rate.frame()));
    }
}
