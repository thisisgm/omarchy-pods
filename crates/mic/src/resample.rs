//! The fixed ratio resampler between a decoder's rate and the node's: a Kaiser windowed sinc held in phases, for four to three and three to four.

use std::f64::consts::PI;

const TAPS: u8 = 32;
/// The taps of one phase: with them the filter has fallen 80 dB at 0.11 of the lower rate over its cut, and delays under 0.4 ms.
#[expect(clippy::as_conversions, reason = "a const context has no `From`")]
pub const RESAMPLE_TAPS: usize = TAPS as usize;
/// The largest phase count across `RATIOS` (four for three to four, three for four to three).
const MAX_PHASES: usize = 4;
/// Doubled ring buffer so every `RESAMPLE_TAPS` window is a single contiguous slice.
const HISTORY_LEN: usize = RESAMPLE_TAPS * 2;
/// Bitmask for wrapping the power-of-two history head without division.
const HISTORY_MASK: usize = RESAMPLE_TAPS - 1;
/// Independent multiply-accumulate lanes so LLVM autovectorizes the 32-tap dot product.
const DOT_LANES: usize = 8;
/// The cut, as a part of the lower rate.
const CUT: f64 = 0.45;
/// Kaiser's rule gives 7.86 for 80 dB; the rest holds the first side lobe 2 dB under that.
const KAISER_BETA: f64 = 8.2;
/// Past the terms of the series a float still takes at `KAISER_BETA`.
const BESSEL_TERMS: u8 = 32;
/// The ratios built, each as the phases of its filter and the phases one output moves on by: four to three, then three to four.
const RATIOS: [(u8, u8); 2] = [(3, 4), (4, 3)];

const _: () = assert!(
    RESAMPLE_TAPS.is_power_of_two()
        && RESAMPLE_TAPS.is_multiple_of(DOT_LANES)
        && HISTORY_LEN == RESAMPLE_TAPS * 2
);

/// The modified Bessel function of the first kind and order zero, by its series.
fn bessel(x: f64) -> f64 {
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..=BESSEL_TERMS {
        term *= (x / (2.0 * f64::from(k))).powi(2);
        sum += term;
    }
    sum
}

#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "a tap inside one, as the float the samples are"
)]
fn narrow(tap: f64) -> f32 {
    tap as f32
}

/// The taps of phase `phase` of the filter for `up` phases, the tap of the newest input first, summing to one.
fn phase_taps(phase: u8, up: u8, step: u8) -> [f32; RESAMPLE_TAPS] {
    // The centre lies between two taps, so none stands where the sinc divides by zero.
    let centre = (f64::from(up) * f64::from(TAPS) - 1.0) / 2.0;
    let turn = PI * 2.0 * CUT / f64::from(up.max(step));
    let mut raw = [0.0_f64; RESAMPLE_TAPS];
    let mut sum = 0.0_f64;
    for (tap, slot) in (0..TAPS).zip(&mut raw) {
        let off = f64::from(phase) + f64::from(tap) * f64::from(up) - centre;
        let window = bessel(KAISER_BETA * (1.0 - (off / centre).powi(2)).sqrt());
        let value = (turn * off).sin() / (turn * off) * window;
        *slot = value;
        sum += value;
    }
    raw.map(|tap| narrow(tap / sum))
}

/// Contiguous 32-tap dot product across `DOT_LANES` independent lanes for SIMD autovectorization.
fn dot(taps: &[f32; RESAMPLE_TAPS], window: &[f32; RESAMPLE_TAPS]) -> f32 {
    let mut lanes = [0.0_f32; DOT_LANES];
    let (tap_chunks, _) = taps.as_chunks::<DOT_LANES>();
    let (win_chunks, _) = window.as_chunks::<DOT_LANES>();
    let mut pairs = tap_chunks.iter().zip(win_chunks);
    // Seed from the first chunk to avoid signed-zero barriers that block SLP vectorization.
    if let Some((tap_chunk, win_chunk)) = pairs.next() {
        for ((acc, &tap), &sample) in lanes.iter_mut().zip(tap_chunk).zip(win_chunk) {
            *acc = tap * sample;
        }
    }
    for (tap_chunk, win_chunk) in pairs {
        for ((acc, &tap), &sample) in lanes.iter_mut().zip(tap_chunk).zip(win_chunk) {
            *acc += tap * sample;
        }
    }
    let [l0, l1, l2, l3, l4, l5, l6, l7] = lanes;
    let [s0, s1, s2, s3] = [l0 + l4, l1 + l5, l2 + l6, l3 + l7];
    (s0 + s1) + (s2 + s3)
}

#[derive(Debug)]
pub struct Resampler {
    /// The taps of each phase, active entries in `0..phase_count`.
    phases: [[f32; RESAMPLE_TAPS]; MAX_PHASES],
    /// Doubled ring buffer of the last `RESAMPLE_TAPS` inputs; `head..head + RESAMPLE_TAPS` is newest first.
    history: [f32; HISTORY_LEN],
    /// Index in `0..RESAMPLE_TAPS` of the newest input in `history`.
    head: usize,
    /// The phase of the next output against the newest input.
    phase: usize,
    /// Active phase count (`3` for four to three, `4` for three to four).
    phase_count: usize,
    /// The phases one output moves on by.
    step: usize,
}

impl Resampler {
    /// Some only for four to three and three to four.
    #[must_use]
    pub fn new(from_hz: u32, to_hz: u32) -> Option<Self> {
        let (from, to) = (u64::from(from_hz), u64::from(to_hz));
        // Zero and zero keep either ratio and are no rate.
        let (up, step) = RATIOS.into_iter().find(|&(up, step)| {
            from != to && from.saturating_mul(u64::from(up)) == to.saturating_mul(u64::from(step))
        })?;
        let mut phases = [[0.0_f32; RESAMPLE_TAPS]; MAX_PHASES];
        for (phase, slot) in (0..up).zip(&mut phases) {
            *slot = phase_taps(phase, up, step);
        }
        Some(Self {
            phases,
            history: [0.0; HISTORY_LEN],
            head: 0,
            phase: 0,
            phase_count: usize::from(up),
            step: usize::from(step),
        })
    }

    /// Appends what `input` is at the new rate; the next call goes on where this one ended.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let phases = self.phases.get(..self.phase_count).unwrap_or(&[]);
        let mut head = self.head;
        let mut phase = self.phase;
        let step = self.step;
        let produced = input
            .len()
            .saturating_mul(phases.len())
            .saturating_sub(phase)
            .saturating_add(step.saturating_sub(1))
            .checked_div(step)
            .unwrap_or(0);
        let start = out.len();
        out.resize(start.saturating_add(produced), 0.0);
        let mut dst = out.get_mut(start..).unwrap_or(&mut []).iter_mut();
        for &sample in input {
            head = head.wrapping_sub(1) & HISTORY_MASK;
            if let Some(slot) = self.history.get_mut(head) {
                *slot = sample;
            }
            if let Some(slot) = self.history.get_mut(head | RESAMPLE_TAPS) {
                *slot = sample;
            }
            if let Some(window) = self
                .history
                .get(head..)
                .and_then(<[f32]>::first_chunk::<RESAMPLE_TAPS>)
            {
                while let Some(taps) = phases.get(phase) {
                    if let Some(slot) = dst.next() {
                        *slot = dot(taps, window);
                    }
                    phase = phase.saturating_add(step);
                }
                phase = phase.saturating_sub(phases.len());
            }
        }
        self.head = head;
        self.phase = phase;
    }

    /// Forgets every input, as a new one has heard none.
    pub fn reset(&mut self) {
        self.history.fill(0.0);
        self.head = 0;
        self.phase = 0;
    }
}
