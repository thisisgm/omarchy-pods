//! The fixed ratio resampler between a decoder's rate and the node's: a Kaiser windowed sinc held in phases, for four to three and three to four.

use std::collections::VecDeque;
use std::f64::consts::PI;
use std::iter;

const TAPS: u8 = 32;
/// The taps of one phase: with them the filter has fallen 80 dB at 0.11 of the lower rate over its cut, and delays under 0.4 ms.
#[expect(clippy::as_conversions, reason = "a const context has no `From`")]
pub const RESAMPLE_TAPS: usize = TAPS as usize;
/// The cut, as a part of the lower rate.
const CUT: f64 = 0.45;
/// Kaiser's rule gives 7.86 for 80 dB; the rest holds the first side lobe 2 dB under that.
const KAISER_BETA: f64 = 8.2;
/// Past the terms of the series a float still takes at `KAISER_BETA`.
const BESSEL_TERMS: u8 = 32;
/// The ratios built, each as the phases of its filter and the phases one output moves on by: four to three, then three to four.
const RATIOS: [(u8, u8); 2] = [(3, 4), (4, 3)];

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
fn phase_taps(phase: u8, up: u8, step: u8) -> Vec<f32> {
    // The centre lies between two taps, so none stands where the sinc divides by zero.
    let centre = (f64::from(up) * f64::from(TAPS) - 1.0) / 2.0;
    let turn = PI * 2.0 * CUT / f64::from(up.max(step));
    let taps: Vec<f64> = (0..TAPS)
        .map(|tap| {
            let off = f64::from(phase) + f64::from(tap) * f64::from(up) - centre;
            let window = bessel(KAISER_BETA * (1.0 - (off / centre).powi(2)).sqrt());
            (turn * off).sin() / (turn * off) * window
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    taps.into_iter().map(|tap| narrow(tap / sum)).collect()
}

#[derive(Debug)]
pub struct Resampler {
    /// The taps of each phase.
    phases: Vec<Vec<f32>>,
    /// The last `RESAMPLE_TAPS` inputs, the newest first.
    history: VecDeque<f32>,
    /// The phase of the next output against the newest input.
    phase: usize,
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
        Some(Self {
            phases: (0..up).map(|phase| phase_taps(phase, up, step)).collect(),
            history: iter::repeat_n(0.0, RESAMPLE_TAPS).collect(),
            phase: 0,
            step: usize::from(step),
        })
    }

    /// Appends what `input` is at the new rate; the next call goes on where this one ended.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        for &sample in input {
            self.history.pop_back();
            self.history.push_front(sample);
            while let Some(taps) = self.phases.get(self.phase) {
                let held = taps.iter().zip(&self.history);
                out.push(held.map(|(tap, sample)| tap * sample).sum());
                self.phase = self.phase.saturating_add(self.step);
            }
            self.phase = self.phase.saturating_sub(self.phases.len());
        }
    }

    /// Forgets every input, as a new one has heard none.
    pub fn reset(&mut self) {
        self.history.iter_mut().for_each(|held| *held = 0.0);
        self.phase = 0;
    }
}
