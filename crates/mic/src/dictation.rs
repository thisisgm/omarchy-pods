//! The dictation stream on its way to the ring: frames by their counter, each decoded, brought to the node's rate and written, and what stands for one that never came.

use std::sync::atomic::Ordering;
use std::time::Duration;

use super::decode::Decoded;
use super::opus::{DICTATION_DECODE_HZ, Decoder, FRAME_SAMPLES};
use super::resample::Resampler;
use super::ring::Path;
use super::{Chain, FRAMES_PER_MS, RATE_HZ};

/// One step of the counter.
pub const FRAME_MS: u32 = 20;
/// The burst that opens a stream held at most 5 frames a notification; the rest of one is dropped and counted.
pub const MAX_FRAMES_PER_NOTIFICATION: usize = 8;
/// 80 ms of the decoder's own concealment: a larger hole in the counters is a new run.
pub const MAX_GAP_FRAMES: u16 = 4;
/// A notification of one frame is the steady cadence.
pub const STEADY_FRAMES: usize = 1;
/// A counter this far past the expected one, the range wrapped, is one the run has passed.
const BEHIND: u16 = 1 << 15;
/// One frame at the node's rate.
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "a const context has no `TryFrom`, and the assertion under it holds the count whole"
)]
const FRAME_FRAMES: u16 = (FRAME_MS * FRAMES_PER_MS as u32) as u16;
#[expect(clippy::as_conversions, reason = "a const context has no `From`")]
const _: () = assert!(
    FRAME_FRAMES as u32 == FRAME_MS * FRAMES_PER_MS as u32
        && FRAME_SAMPLES * 1000 == (FRAME_MS * DICTATION_DECODE_HZ) as usize
        && STEADY_FRAMES < MAX_FRAMES_PER_NOTIFICATION
);

/// What a frame's counter says of it, against the counter the run expected next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// The frames lost before it, none when it is the expected one.
    After(u16),
    /// The run has passed it.
    Behind,
    /// Too far on for a loss: it starts a new run.
    NewRun,
}

fn place(expected: Option<u16>, counter: u16) -> Place {
    match expected.map_or(0, |expected| counter.wrapping_sub(expected)) {
        lost if lost <= MAX_GAP_FRAMES => Place::After(lost),
        lost if lost >= BEHIND => Place::Behind,
        _ => Place::NewRun,
    }
}

#[derive(Debug)]
pub(crate) struct Dictation {
    decoder: Decoder,
    /// The counter the next frame of the run carries; none before the run's first frame.
    expected: Option<u16>,
    /// What brings a frame to the node's rate; none while the decoder runs at it.
    resampler: Option<Resampler>,
    /// The frame the resampler made last.
    resampled: Vec<f32>,
}

impl Dictation {
    /// None when the decoder will not open.
    pub(crate) fn new() -> Option<Self> {
        Some(Self {
            decoder: Decoder::new()?,
            expected: None,
            resampler: Resampler::new(DICTATION_DECODE_HZ, RATE_HZ),
            resampled: Vec::new(),
        })
    }

    /// A fresh decoder, and a filter that holds nothing the old one gave; the decoder opened when the chain was made, and were it refused now the old one would stay.
    fn renew_decoder(&mut self) {
        if let Some(fresh) = Decoder::new() {
            self.decoder = fresh;
        }
        if let Some(resampler) = &mut self.resampler {
            resampler.reset();
        }
    }

    /// What one call into the decoder gave, its samples at the node's rate.
    fn at_node_rate(&mut self, call: impl FnOnce(&mut Decoder) -> Decoded<'_>) -> Decoded<'_> {
        match (call(&mut self.decoder), &mut self.resampler) {
            (Decoded::Samples(samples), Some(resampler)) => {
                self.resampled.clear();
                resampler.push(samples, &mut self.resampled);
                Decoded::Samples(&self.resampled)
            }
            (decoded, _) => decoded,
        }
    }
}

impl Chain {
    /// A new run of the dictation stream, as `reset` is one of the uplink; the caller opens every start of the stream with it, for a counter that starts again behind the last one reads as a repeat and writes nothing.
    pub fn dictation_run(&mut self) {
        self.dictation.renew_decoder();
        self.dictation.expected = None;
        self.start_run(Path::Dictation);
    }

    /// The frames of one notification as (counter, Opus bytes), and the time the notification arrived; on a run of the uplink it is a stale packet, neither written nor counted.
    pub fn frames<'a>(&mut self, now: Duration, frames: impl Iterator<Item = (u16, &'a [u8])>) {
        if self.ring.path() != Path::Dictation {
            return;
        }
        let mut frames = frames;
        let (mut met, mut wrote) = (0_usize, 0_usize);
        for (counter, data) in frames.by_ref().take(MAX_FRAMES_PER_NOTIFICATION) {
            met = met.saturating_add(1);
            if self.frame(counter, |decoder| decoder.frame(data)) {
                wrote = wrote.saturating_add(1);
            }
        }
        let dropped = frames.count();
        self.counters.decode_errors.fetch_add(
            u32::try_from(dropped).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );
        // A burst stands ahead of the cadence and a repeat behind it; only a steady notification that moved the ring tells the time of the run.
        if (met, wrote) == (STEADY_FRAMES, STEADY_FRAMES) {
            let earliest = self.arrivals.push(self.ring.arrival_at(now));
            self.ring.publish(earliest);
        }
    }

    /// Writes what the counter says was lost before this frame, then the frame or what stands for it; false when the run has passed the counter and nothing is written.
    fn frame(&mut self, counter: u16, decode: impl FnOnce(&mut Decoder) -> Decoded<'_>) -> bool {
        match place(self.dictation.expected, counter) {
            Place::After(lost) => (0..lost).for_each(|_| self.lost(Decoder::conceal)),
            Place::Behind => return false,
            Place::NewRun => self.dictation_run(),
        }
        self.dictation.expected = Some(counter.wrapping_add(1));
        match self.dictation.at_node_rate(decode) {
            Decoded::Samples(samples) => Self::write_audio(&self.ring, &mut self.faded, samples),
            Decoded::Error => {
                self.counters.decode_errors.fetch_add(1, Ordering::Relaxed);
                self.lost(Decoder::conceal);
            }
            Decoded::Panic => self.panicked(),
        }
        true
    }

    /// One frame in place of audio that never came: the decoder's own concealment, which joins the audio around it with no fade.
    fn lost(&mut self, conceal: impl FnOnce(&mut Decoder) -> Decoded<'_>) {
        match self.dictation.at_node_rate(conceal) {
            Decoded::Samples(samples) => {
                self.ring.write(samples.iter().copied());
                self.counters.concealed.fetch_add(1, Ordering::Relaxed);
            }
            Decoded::Error => self.fall(),
            Decoded::Panic => self.panicked(),
        }
    }

    fn panicked(&mut self) {
        self.counters.decoder_resets.fetch_add(1, Ordering::Relaxed);
        self.fall();
    }

    /// The uplink's concealment in place of the frame, and a fresh decoder: one that gave nothing may hold anything, and a fresh one conceals to silence until its first audio rises.
    fn fall(&mut self) {
        self.dictation.renew_decoder();
        self.conceal(FRAME_FRAMES);
    }
}

#[cfg(test)]
mod tests;
