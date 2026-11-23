//! The check that a tone a test sent through the graph came out whole, at its level and its pitch.

use std::f64::consts::SQRT_2;
use std::time::Duration;

/// The span the check judges.
pub const CHECKED: Duration = Duration::from_secs(2);
/// Skipped after the first sound, so a resampler's start is not judged as the tone.
pub const SETTLE: Duration = Duration::from_millis(100);
/// Short enough to show a gap, long enough for a steady level.
const WINDOW: Duration = Duration::from_millis(10);
/// How far the count of zero crossings may stand from two a cycle, for the two ends of the span.
const CROSSINGS_SLACK: f64 = 4.0;
const MILLIS_PER_SECOND: u128 = 1_000;

/// The frames `span` holds at `rate`.
#[must_use]
pub fn frames(span: Duration, rate: u32) -> usize {
    let frames = span.as_millis().saturating_mul(u128::from(rate)) / MILLIS_PER_SECOND;
    usize::try_from(frames).unwrap_or_else(|_| panic!("{span:?} at {rate} Hz is too long"))
}

/// Asserts `samples` hold a sine of `hz` peaking at `amplitude` of full scale, within `tolerance_db` in every window and with no gap, over `CHECKED` after the first sound and `SETTLE`.
pub fn assert_tone(samples: &[i16], rate: u32, hz: f64, amplitude: f64, tolerance_db: f64) {
    let first = samples
        .iter()
        .position(|&sample| sample != 0)
        .unwrap_or_else(|| panic!("the recording holds no sound"));
    let start = first.saturating_add(frames(SETTLE, rate));
    let span = samples
        .get(start..start.saturating_add(frames(CHECKED, rate)))
        .unwrap_or_else(|| panic!("{} samples, the tone starts at {first}", samples.len()));
    let expected = 20.0 * (amplitude / SQRT_2).log10();
    for (index, window) in span.chunks_exact(frames(WINDOW, rate)).enumerate() {
        let level = dbfs(window);
        assert!(
            (level - expected).abs() <= tolerance_db,
            "window {index} of {WINDOW:?} at {level:.2} dBFS, the tone is {expected:.2}"
        );
    }
    let crossings = span
        .array_windows::<2>()
        .filter(|[before, after]| (*before < 0) != (*after < 0))
        .count();
    let crossings = f64::from(u32::try_from(crossings).unwrap_or(u32::MAX));
    let cycles = hz * CHECKED.as_secs_f64();
    assert!(
        (crossings - 2.0 * cycles).abs() <= CROSSINGS_SLACK,
        "{crossings} zero crossings at {rate} Hz, a {hz} Hz tone makes {}",
        2.0 * cycles
    );
}

fn dbfs(window: &[i16]) -> f64 {
    let full_scale = -f64::from(i16::MIN);
    let power: f64 = window
        .iter()
        .map(|&sample| (f64::from(sample) / full_scale).powi(2))
        .sum();
    let count = f64::from(u32::try_from(window.len()).unwrap_or(u32::MAX));
    10.0 * (power / count).log10()
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;
    use std::ops::Range;
    use std::time::Duration;

    use super::{assert_tone, frames};

    const RATE: u32 = 16_000;
    const HZ: f64 = 1_000.0;
    const AMPLITUDE: f64 = 0.5;
    const TOLERANCE_DB: f64 = 0.5;
    /// One decibel down, as a factor.
    const ONE_DB_LOW: f64 = 0.891;
    /// Past the settle and the checked span.
    const LENGTH: Duration = Duration::from_millis(2_300);
    /// Ten milliseconds of samples, about one second into the checked span.
    const GAP: Range<usize> = 16_000..16_160;
    /// The settle after the first sound.
    const START: Range<usize> = 0..1_600;
    /// Fifty milliseconds of silence before the first sound.
    const LEAD: usize = 800;

    /// A sine of `hz` peaking at `amplitude`, `LENGTH` long.
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "a value inside full scale, rounded to the nearest sample"
    )]
    fn sine(hz: f64, amplitude: f64) -> Vec<i16> {
        (0..frames(LENGTH, RATE))
            .map(|n| {
                let seconds = f64::from(u32::try_from(n).unwrap()) / f64::from(RATE);
                ((TAU * hz * seconds).sin() * amplitude * f64::from(i16::MAX)).round() as i16
            })
            .collect()
    }

    #[test]
    fn a_whole_tone_passes() {
        assert_tone(&sine(HZ, AMPLITUDE), RATE, HZ, AMPLITUDE, TOLERANCE_DB);
    }

    #[test]
    fn a_start_that_is_not_the_tone_yet_is_not_judged() {
        let mut sound = sine(HZ, AMPLITUDE);
        sound[START].fill(1);
        let samples = [vec![0; LEAD], sound].concat();
        assert_tone(&samples, RATE, HZ, AMPLITUDE, TOLERANCE_DB);
    }

    #[test]
    #[should_panic(expected = "dBFS, the tone is -9.03")]
    fn a_gap_of_ten_milliseconds_fails() {
        let mut samples = sine(HZ, AMPLITUDE);
        samples[GAP].fill(0);
        assert_tone(&samples, RATE, HZ, AMPLITUDE, TOLERANCE_DB);
    }

    #[test]
    #[should_panic(expected = "window 0 of 10ms at -10.03 dBFS, the tone is -9.03")]
    fn a_tone_one_decibel_low_fails() {
        let samples = sine(HZ, AMPLITUDE * ONE_DB_LOW);
        assert_tone(&samples, RATE, HZ, AMPLITUDE, TOLERANCE_DB);
    }

    #[test]
    #[should_panic(expected = "4040 zero crossings at 16000 Hz, a 1000 Hz tone makes 4000")]
    fn a_tone_one_percent_sharp_fails() {
        let samples = sine(HZ * 1.01, AMPLITUDE);
        assert_tone(&samples, RATE, HZ, AMPLITUDE, TOLERANCE_DB);
    }
}
