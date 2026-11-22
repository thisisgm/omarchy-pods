//! Generated signals shared by the unit tests of the acoustic measures and the test of the built command.

use std::f32::consts::TAU;
use std::ops::Range;

use super::xorshift::next_random;

/// The sines `band_noise` sums lie this far apart, each moved by a seeded part of a quarter of it.
const SINE_SPACING_HZ: f32 = 100.0;
/// Under this every sine of `tilted_noise` keeps its full amplitude.
pub(crate) const TILT_FROM_HZ: f32 = 500.0;
/// One slot of a burst train.
const SLOT_MS: u32 = 20;
/// The quietest and the loudest burst of a train.
const BURST_GAINS: (f32, f32) = (0.05, 0.4);

/// A room at -70 dBFS: uniform noise of this amplitude.
pub(crate) const ROOM: f32 = 5.48e-4;
/// One step of the grid a spectrum is read on, which a planted frequency is found within.
pub(crate) const BIN_HZ: u32 = 250;
/// The seed of the signals, and that of a scene unlike the first.
pub(crate) const SEED: u64 = 0x5eed_0008;
pub(crate) const OTHER_SEED: u64 = 0x5eed_0009;

pub(crate) fn close(found: f64, planted: f64, within: f64) -> bool {
    (found - planted).abs() <= within
}

/// A seeded value from -1 up to 1.
fn unit(seed: &mut u64) -> f32 {
    let [.., high, top] = next_random(seed).to_le_bytes();
    f32::from(u16::from_le_bytes([high, top])) / 32768.0 - 1.0
}

/// `rate` as the number a sine is built from; every rate of the tests lies under 65536, which an `f32` holds whole.
fn hertz(rate: u32) -> f32 {
    f32::from(u16::try_from(rate).unwrap())
}

/// The samples `ms` milliseconds hold at `rate`.
pub(crate) fn samples_in(ms: u32, rate: u32) -> usize {
    usize::try_from(u64::from(ms).saturating_mul(u64::from(rate)) / 1000).unwrap()
}

/// The samples of `len_ms` milliseconds from `from_ms` on, at `rate`.
pub(crate) fn span(from_ms: u32, len_ms: u32, rate: u32) -> Range<usize> {
    samples_in(from_ms, rate)..samples_in(from_ms.saturating_add(len_ms), rate)
}

/// A sine that starts `phase` radians into its turn.
pub(crate) fn sine(hz: f32, rate: u32, amplitude: f32, mut phase: f32, len: usize) -> Vec<f32> {
    let step = TAU * hz / hertz(rate);
    let mut next = || {
        let sample = amplitude * phase.sin();
        phase = (phase + step) % TAU;
        sample
    };
    (0..len).map(|_| next()).collect()
}

/// White noise, uniform from -`amplitude` to `amplitude`: its RMS is the amplitude over the root of 3.
pub(crate) fn noise(seed: &mut u64, amplitude: f32, len: usize) -> Vec<f32> {
    (0..len).map(|_| amplitude * unit(seed)).collect()
}

/// Noise behind a brick wall: sines of `amplitude` each at seeded phases, about 100 Hz apart, every one below `top_hz`.
pub(crate) fn band_noise(
    seed: &mut u64,
    top_hz: f32,
    rate: u32,
    amplitude: f32,
    len: usize,
) -> Vec<f32> {
    tilted_noise(seed, top_hz, rate, amplitude, 0.0, len)
}

/// The noise of `band_noise` with the long term spectrum of a voice: each sine over 500 Hz lies `tilt_db` an octave under `amplitude`.
pub(crate) fn tilted_noise(
    seed: &mut u64,
    top_hz: f32,
    rate: u32,
    amplitude: f32,
    tilt_db: f32,
    len: usize,
) -> Vec<f32> {
    let mut sum = vec![0.0_f32; len];
    let mut centre_hz = SINE_SPACING_HZ;
    while centre_hz + SINE_SPACING_HZ / 4.0 < top_hz {
        let hz = centre_hz + unit(seed) * SINE_SPACING_HZ / 4.0;
        let phase = (unit(seed) + 1.0) * TAU / 2.0;
        let octaves = (centre_hz / TILT_FROM_HZ).max(1.0).log2();
        let gain = 10.0_f32.powf(-tilt_db * octaves / 20.0);
        add(&mut sum, 0, &sine(hz, rate, amplitude * gain, phase, len));
        centre_hz += SINE_SPACING_HZ;
    }
    sum
}

/// Adds `what` into `into` from sample `at` on; what runs past the end is left out.
pub(crate) fn add(into: &mut [f32], at: usize, what: &[f32]) {
    for (sample, added) in into.iter_mut().skip(at).zip(what) {
        *sample += added;
    }
}

/// A train of noise bursts in 20 ms slots, each silent or at a seeded gain; the envelope comes from `envelope_seed` alone, so two rates carry one envelope.
pub(crate) fn bursts(envelope_seed: u64, carrier_seed: u64, rate: u32, slots: usize) -> Vec<f32> {
    let (mut envelope, mut carrier) = (envelope_seed, carrier_seed);
    let (quietest, loudest) = BURST_GAINS;
    let mut train = Vec::new();
    for _ in 0..slots {
        let drawn = unit(&mut envelope);
        let gain = if drawn < 0.0 {
            0.0
        } else {
            quietest + (loudest - quietest) * drawn
        };
        train.extend(noise(&mut carrier, gain, samples_in(SLOT_MS, rate)));
    }
    train
}

/// The RMS of `samples` in dBFS, by the plainest arithmetic there is.
pub(crate) fn rms_dbfs(samples: &[f32]) -> f64 {
    let squares: f64 = samples
        .iter()
        .map(|&sample| f64::from(sample).powi(2))
        .sum();
    10.0 * (squares / f64::from(u32::try_from(samples.len()).unwrap())).log10()
}
