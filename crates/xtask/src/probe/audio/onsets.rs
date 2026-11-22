//! Tap onsets: where the energy above 2 kHz jumps, and what the spacing of the same taps in two recordings says of their clocks.

use std::f64::consts::{FRAC_1_SQRT_2, TAU};

use super::super::analyse::stats::count;
use super::levels::{LSB_POWER, samples_in};

/// A tap is read above this frequency, where a voice has little and a knock on the bud has much.
const HIGH_PASS_HZ: f64 = 2000.0;
/// The stretch whose energy is weighed.
const ENERGY_MS: u32 = 2;
/// The stretch before it that it is weighed against.
const BEFORE_MS: u32 = 50;
pub(crate) const RISE_OVER_MS: u32 = BEFORE_MS + ENERGY_MS;
/// The rise that makes an onset.
const RISE_DB: f64 = 20.0;
/// Two onsets closer than this are one tap and its bounce.
const APART_MS: u32 = 100;
/// A tap is its own in another recording only nearer than this, half the least gap of two onsets: past it, it could be the next one.
pub(crate) const SAME_TAP_MS: u32 = APART_MS / 2;
const PARTS_PER_MILLION: f64 = 1e6;
const MS_PER_SECOND: f64 = 1000.0;

/// One second order Butterworth high pass section, from the Audio EQ Cookbook of Robert Bristow-Johnson.
#[derive(Debug, Clone, Copy)]
struct HighPass {
    feed: [f64; 3],
    back: [f64; 2],
}

impl HighPass {
    fn new(rate: u32) -> Self {
        let turn = TAU * HIGH_PASS_HZ / f64::from(rate);
        let (sin, cos) = turn.sin_cos();
        let alpha = sin * FRAC_1_SQRT_2;
        let scale = 1.0 + alpha;
        let half = (1.0 + cos) / (2.0 * scale);
        Self {
            feed: [half, -2.0 * half, half],
            back: [-2.0 * cos / scale, (1.0 - alpha) / scale],
        }
    }

    fn run(self, input: impl Iterator<Item = f64>) -> Vec<f64> {
        let ([b0, b1, b2], [a1, a2]) = (self.feed, self.back);
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        let mut output = Vec::new();
        for x0 in input {
            let y0 = b0 * x0 + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
            (x2, x1, y2, y1) = (x1, x0, y1, y0);
            output.push(y0);
        }
        output
    }
}

/// The running sum of the power above 2 kHz: entry k is the energy of the first k samples.
fn energy_before(samples: &[f32], rate: u32) -> Vec<f64> {
    let filter = HighPass::new(rate);
    let once = filter.run(samples.iter().map(|&sample| f64::from(sample)));
    let twice = filter.run(once.into_iter());
    let mut sum = 0.0_f64;
    let mut running = vec![0.0];
    running.extend(twice.iter().map(|sample| {
        sum += sample * sample;
        sum
    }));
    running
}

pub(crate) fn taps_note() -> String {
    format!(
        "rises of {RISE_DB} dB in the energy of {ENERGY_MS} ms above {HIGH_PASS_HZ} Hz over the {BEFORE_MS} ms before, {APART_MS} ms apart"
    )
}

/// Onsets where the energy of 2 ms above 2 kHz rises 20 dB over the 50 ms before it, at least 100 ms apart, each at its first loud sample.
pub(crate) fn taps(samples: &[f32], rate: u32) -> Vec<usize> {
    let (now_len, before_len) = (
        samples_in(ENERGY_MS, rate).max(1),
        samples_in(BEFORE_MS, rate).max(1),
    );
    let running = energy_before(samples, rate);
    let energy = |from: usize, len: usize| {
        let (start, end) = (running.get(from), running.get(from.saturating_add(len)));
        start
            .zip(end)
            .map(|(start, end)| (end - start) / count(len))
    };
    let rise = 10.0_f64.powf(RISE_DB / 10.0);
    let mut onsets = Vec::new();
    let mut at = before_len;
    while let (Some(before), Some(now)) = (
        energy(at.saturating_sub(before_len), before_len),
        energy(at, now_len),
    ) {
        let loud = before.max(LSB_POWER) * rise;
        if now < loud {
            at = at.saturating_add(1);
            continue;
        }
        let first_loud =
            (at..at.saturating_add(now_len)).find(|&sample| energy(sample, 1) >= Some(loud));
        let onset = first_loud.unwrap_or(at);
        onsets.push(onset);
        at = onset.saturating_add(samples_in(APART_MS, rate));
    }
    onsets
}

/// What the places of the taps of two recordings show of whether they are the same taps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SameTaps {
    /// Under three taps leave none between the first and the last, so nothing shows it.
    NotShown,
    /// Every tap lies nearer than `SAME_TAP_MS` to its own.
    Shown,
    /// They are not: this tap, counted from 1, is the first that lies `SAME_TAP_MS` or more from its own, which need not be the tap that differs.
    Not { tap: usize },
}

/// Lays the first and the last tap of two lists of seconds on each other and asks of every tap whether it then lies on its own.
pub(crate) fn same_taps(pods: &[f64], reference: &[f64]) -> SameTaps {
    let ([ours_first, between @ .., ours_last], [theirs_first, .., theirs_last]) =
        (pods, reference)
    else {
        return SameTaps::NotShown;
    };
    if between.is_empty() {
        return SameTaps::NotShown;
    }
    let stretch = (ours_last - ours_first) / (theirs_last - theirs_first);
    let near = f64::from(SAME_TAP_MS) / MS_PER_SECOND;
    let is_off = |(ours, theirs): (&f64, &f64)| {
        let off = (ours - (ours_first + (theirs - theirs_first) * stretch)).abs();
        off.is_nan() || off >= near
    };
    match pods.iter().zip(reference).position(is_off) {
        Some(index) => SameTaps::Not {
            tap: index.saturating_add(1),
        },
        None => SameTaps::Shown,
    }
}

/// How much longer the stream takes from its first tap to its last than the reference does, in parts per million; `None` unless both hold as many taps, two or more, over some time.
pub(crate) fn ratio_ppm(pods: &[f64], reference: &[f64]) -> Option<f64> {
    if pods.len() != reference.len() || pods.len() < 2 {
        return None;
    }
    let span = |onsets: &[f64]| Some(onsets.last()? - onsets.first()?);
    let (ours, theirs) = (span(pods)?, span(reference)?);
    let ppm = (ours / theirs - 1.0) * PARTS_PER_MILLION;
    (theirs > 0.0 && ppm.is_finite()).then_some(ppm)
}
