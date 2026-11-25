//! The dictation stream on its way to the ring: frames by their counter, each decoded, brought to the node's rate and written, and what stands for one that never came.

use std::sync::atomic::Ordering;
use std::time::Duration;

use super::decode::Decoded;
use super::opus::{DICTATION_DECODE_HZ, Decoder, FRAME_SAMPLES, MAX_OPUS_FRAME_BYTES};
use super::resample::Resampler;
use super::ring::Path;
use super::{Chain, Counters, FRAMES_PER_MS, RATE_HZ, renew};

/// One step of the counter.
pub const FRAME_MS: u32 = 20;
/// The burst that opens a stream held at most 5 frames a notification; the rest of one is dropped and counted.
pub const MAX_FRAMES_PER_NOTIFICATION: usize = 8;
/// 80 ms of the decoder's own concealment: a larger hole in the counters is a new run.
pub const MAX_GAP_FRAMES: u16 = 4;
/// A notification of one frame is the steady cadence.
pub const STEADY_FRAMES: usize = 1;
/// 1.6 s of the burst that opens a stream, which was measured at 63 frames; a longer one is placed when the stage is full.
pub const PREROLL_STAGE_FRAMES: usize = 80;
/// Keep this much leading silence for the recorder's voice activity detector.
const SILENCE_COLLAR_FRAMES: usize = 3;
const SILENCE_RMS_DBFS: f64 = -58.0;
const DB_PER_POWER_DECADE: f64 = 10.0;
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
const FRAME_TIME: Duration = Duration::from_millis(FRAME_MS as u64);
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
    /// The frames of the burst that opens a stream, by counter, held until a steady notification places them; none once they are placed.
    stage: Option<Vec<(u16, Vec<u8>)>>,
}

impl Dictation {
    /// None when the decoder will not open.
    pub(crate) fn new() -> Option<Self> {
        Some(Self {
            decoder: Decoder::new()?,
            expected: None,
            resampler: Resampler::new(DICTATION_DECODE_HZ, RATE_HZ),
            resampled: Vec::new(),
            stage: None,
        })
    }

    /// A fresh decoder, and a filter that holds nothing the old one gave; the decoder opened when the chain was made, and were it refused now the old one would stay.
    fn renew_decoder(&mut self, counters: &Counters) {
        renew(&mut self.decoder, counters, Decoder::new());
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
    /// A new run of the dictation stream, as `reset` is one of the uplink, with its opening staged; the caller opens every start of the stream with it, for a counter that starts again behind the last one reads as a repeat and writes nothing.
    pub fn dictation_run(&mut self) {
        self.new_run();
        self.dictation.stage = Some(Vec::with_capacity(PREROLL_STAGE_FRAMES));
    }

    fn new_run(&mut self) {
        self.dictation.renew_decoder(&self.counters);
        self.dictation.expected = None;
        self.start_run(Path::Dictation);
    }

    /// The frames of one notification as (counter, Opus bytes), and the time the notification arrived; on a run of the uplink it is a stale packet, neither written nor counted.
    pub fn frames<'a>(&mut self, now: Duration, frames: impl Iterator<Item = (u16, &'a [u8])>) {
        if self.ring.path() != Path::Dictation {
            return;
        }
        let mut frames = frames;
        let staging = self.dictation.stage.is_some();
        let (mut met, mut took) = (0_usize, 0_usize);
        let mut held = frames.by_ref().take(MAX_FRAMES_PER_NOTIFICATION).peekable();
        while let Some((counter, data)) = held.next() {
            met = met.saturating_add(1);
            let steady = met == STEADY_FRAMES && held.peek().is_none();
            if self.take(now, counter, data, steady) {
                took = took.saturating_add(1);
            }
        }
        let dropped = frames.count();
        self.counters.decode_errors.fetch_add(
            u32::try_from(dropped).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );
        // A burst stands ahead of the cadence and a repeat behind it; only a steady notification whose frame the run took tells its time, and the first one places the opening.
        if (met, took) == (STEADY_FRAMES, STEADY_FRAMES) {
            if staging {
                self.place(now, true);
            } else {
                self.arrived(now);
            }
        }
    }

    /// One frame of a notification: staged while the run's opening is, else written; false when the run has passed its counter.
    fn take(&mut self, now: Duration, counter: u16, data: &[u8], steady: bool) -> bool {
        let Some(stage) = &mut self.dictation.stage else {
            return self.frame(counter, |decoder| decoder.frame(data));
        };
        match place(self.dictation.expected, counter) {
            Place::After(_) => {}
            Place::Behind => {
                self.counters
                    .dictation_repeats
                    .fetch_add(1, Ordering::Relaxed);
                return false;
            }
            Place::NewRun => stage.clear(),
        }
        self.dictation.expected = Some(counter.wrapping_add(1));
        // One byte past the cap is kept, so the decoder still refuses a frame that came too long.
        let held = data.get(..=MAX_OPUS_FRAME_BYTES).unwrap_or(data);
        stage.push((counter, held.to_vec()));
        if stage.len() == PREROLL_STAGE_FRAMES {
            self.place(now, steady);
        }
        true
    }

    /// Places a burst that ended before the first steady notification.
    pub fn flush_dictation(&mut self, now: Duration) {
        if self.ring.path() == Path::Dictation {
            self.ring.finish();
            self.place(now, false);
        }
    }

    /// Anchors the last staged frame at `now` and places those inside the opening's reach.
    fn place(&mut self, now: Duration, steady_last: bool) {
        let Some(staged) = self.dictation.stage.take() else {
            return;
        };
        let Some(&(last, _)) = staged.last() else {
            return;
        };
        let reach = self.ring.open(now);
        self.dictation.expected = None;
        let first = staged.iter().position(|(counter, _)| {
            let back = last.wrapping_sub(*counter);
            usize::from(back) < PREROLL_STAGE_FRAMES
                && FRAME_TIME.saturating_mul(u32::from(back)) < reach
        });
        let Some(kept) = first.and_then(|first| staged.get(first..)) else {
            return;
        };
        let burst_frames = kept.len().saturating_sub(usize::from(steady_last));
        let removed = if burst_frames > SILENCE_COLLAR_FRAMES {
            leading_silence(kept.iter().take(burst_frames)).saturating_sub(SILENCE_COLLAR_FRAMES)
        } else {
            0
        };
        let (silent, kept) = kept.split_at_checked(removed).unwrap_or((&[], kept));
        // Warm both filters on the omitted prefix so the collar and speech decode as before.
        for (_, data) in silent {
            self.dictation.at_node_rate(|decoder| decoder.frame(data));
        }
        for (counter, data) in kept {
            self.frame(*counter, |decoder| decoder.frame(data));
        }
        self.arrived(now);
    }

    /// Writes what the counter says was lost before this frame, then the frame or what stands for it; false when the run has passed the counter and nothing is written.
    fn frame(&mut self, counter: u16, decode: impl FnOnce(&mut Decoder) -> Decoded<'_>) -> bool {
        match place(self.dictation.expected, counter) {
            Place::After(lost) => (0..lost).for_each(|_| self.lost(Decoder::conceal)),
            Place::Behind => {
                self.counters
                    .dictation_repeats
                    .fetch_add(1, Ordering::Relaxed);
                return false;
            }
            Place::NewRun => self.new_run(),
        }
        self.dictation.expected = Some(counter.wrapping_add(1));
        let counters = &self.counters;
        let decoded = self
            .dictation
            .at_node_rate(|decoder| counters.measure_peak(decode(decoder)));
        match decoded {
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
        self.dictation.renew_decoder(&self.counters);
        self.conceal(FRAME_FRAMES);
    }
}

/// Counts the decodable, consecutive quiet frames at the front, using the raw decoder level.
fn leading_silence<'a>(frames: impl Iterator<Item = &'a (u16, Vec<u8>)>) -> usize {
    let Some(mut decoder) = Decoder::new() else {
        return 0;
    };
    let power_limit = 10_f64.powf(SILENCE_RMS_DBFS / DB_PER_POWER_DECADE)
        * f64::from(u32::try_from(FRAME_SAMPLES).unwrap_or(u32::MAX));
    let mut previous = None;
    let mut quiet = 0_usize;
    for (counter, data) in frames {
        if previous.is_some_and(|last: u16| last.wrapping_add(1) != *counter) {
            break;
        }
        let Decoded::Samples(samples) = decoder.frame(data) else {
            break;
        };
        let power: f64 = samples
            .iter()
            .map(|&sample| f64::from(sample).powi(2))
            .sum();
        if !power.is_finite() || power >= power_limit {
            break;
        }
        previous = Some(*counter);
        quiet = quiet.saturating_add(1);
    }
    quiet
}

#[cfg(test)]
mod tests;
