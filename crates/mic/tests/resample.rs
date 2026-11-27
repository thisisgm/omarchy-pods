//! Support tests of the fixed ratio resampler, through its public API; the resampler has no user path yet.

#[cfg(test)]
mod tests {
    use std::f64::consts::{SQRT_2, TAU};

    use pods_mic::resample::{RESAMPLE_TAPS, Resampler};
    use proptest::prelude::*;

    const LOW_HZ: u32 = 48_000;
    const HIGH_HZ: u32 = 64_000;
    /// From and to: four to three, then three to four.
    const RATIOS: [(u32, u32); 2] = [(HIGH_HZ, LOW_HZ), (LOW_HZ, HIGH_HZ)];
    /// What a measure takes in: a multiple of both ratios, 120 ms at the higher rate.
    const FRAMES: u32 = 7680;
    /// The outputs a measure leaves out, in which the history still holds the silence before the tone.
    const SETTLE: usize = 2 * RESAMPLE_TAPS;
    /// Where the filter has fallen 80 dB, as a part of the lower rate: 0.11 over the cut at 0.45.
    const STOP_FROM: f64 = 0.56;
    const STOP_DB: f64 = -80.0;
    /// The lowest tone swept: a measure takes in 12 turns of it or more.
    const SWEEP_FROM_HZ: f64 = 100.0;
    /// From one tone swept to the next: under a tenth of the 1500 Hz a side lobe of the longer filter spans, 192 kHz over its 128 taps.
    const SWEEP_STEP_HZ: f64 = 97.0;
    /// A block is shorter than this, which is over a unit of 480 samples and many times the history.
    const BLOCK_UNDER: usize = 600;

    fn seconds(frame: u32, rate: u32) -> f64 {
        f64::from(frame) / f64::from(rate)
    }

    fn db(ratio: f64) -> f64 {
        20.0 * ratio.log10()
    }

    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "a value inside full scale, as the float a decoder gives"
    )]
    fn sample(value: f64) -> f32 {
        value as f32
    }

    /// A sine at full scale.
    fn tone(hz: f64, rate: u32) -> Vec<f32> {
        (0..FRAMES)
            .map(|frame| sample((TAU * hz * seconds(frame, rate)).sin()))
            .collect()
    }

    /// What a new resampler makes of `input`, less the outputs it settles over.
    fn settled(from: u32, to: u32, input: &[f32]) -> Vec<f32> {
        let mut out = Vec::new();
        Resampler::new(from, to).unwrap().push(input, &mut out);
        let made = u32::try_from(out.len()).unwrap();
        assert_eq!(seconds(made, to), seconds(FRAMES, from), "{from} to {to}");
        out.split_off(SETTLE)
    }

    fn rms(samples: impl Iterator<Item = f64>) -> f64 {
        let (sum, count) = samples.fold((0.0, 0.0), |(sum, count), sample| {
            (sum + sample * sample, count + 1.0)
        });
        (sum / count).sqrt()
    }

    /// The amplitude of the tone at `hz` that fits `heard` with the least squares, and the level of what stands beside it.
    fn fit(heard: &[f32], hz: f64, rate: u32) -> (f64, f64) {
        let waves: Vec<(f64, f64)> = (0..)
            .zip(heard)
            .map(|(frame, _)| (TAU * hz * seconds(frame, rate)).sin_cos())
            .collect();
        let pairs = || waves.iter().zip(heard.iter().copied().map(f64::from));
        let sum = |term: fn((f64, f64), f64) -> f64| -> f64 {
            pairs().map(|(&wave, sample)| term(wave, sample)).sum()
        };
        let (ss, sc, cc) = (
            sum(|(sin, _), _| sin * sin),
            sum(|(sin, cos), _| sin * cos),
            sum(|(_, cos), _| cos * cos),
        );
        let (ys, yc) = (sum(|(sin, _), y| y * sin), sum(|(_, cos), y| y * cos));
        let scale = ss * cc - sc * sc;
        let (a, b) = ((ys * cc - yc * sc) / scale, (yc * ss - ys * sc) / scale);
        let beside = rms(pairs().map(|(&(sin, cos), y)| y - a * sin - b * cos));
        (a.hypot(b), beside)
    }

    /// The tones from `from` up to `to`, `SWEEP_STEP_HZ` apart.
    fn tones(from: f64, to: f64) -> impl Iterator<Item = f64> {
        (0..)
            .map(move |at| from + SWEEP_STEP_HZ * f64::from(at))
            .take_while(move |&hz| hz < to)
    }

    #[test]
    fn four_to_three_and_three_to_four_keep_a_1_khz_tone_within_0_1_db() {
        const TONE_HZ: f64 = 1000.0;
        const KEPT_DB: f64 = 0.1;
        for (from, to) in RATIOS {
            let heard = settled(from, to, &tone(TONE_HZ, from));
            let (amplitude, _) = fit(&heard, TONE_HZ, to);
            assert!(
                db(amplitude).abs() <= KEPT_DB,
                "{from} to {to}: {:.4} dB",
                db(amplitude)
            );
        }
    }

    #[test]
    fn the_stopband_is_80_db_down_at_both_ratios() {
        // A tone under the mirror of the edge in the lower rate's half has every image and fold of it over the edge.
        let edge = STOP_FROM * f64::from(LOW_HZ);
        let mirror = f64::from(LOW_HZ) - edge;
        for (from, to) in RATIOS {
            for hz in tones(SWEEP_FROM_HZ, mirror) {
                let (_, beside) = fit(&settled(from, to, &tone(hz, from)), hz, to);
                let down = db(beside * SQRT_2);
                assert!(
                    down <= STOP_DB,
                    "{from} to {to}, beside {hz} Hz: {down:.1} dB"
                );
            }
        }
        // Four to three only: a tone over the edge would fold under half the new rate.
        for hz in tones(edge, f64::from(HIGH_HZ) / 2.0) {
            let heard = settled(HIGH_HZ, LOW_HZ, &tone(hz, HIGH_HZ));
            let down = db(rms(heard.iter().copied().map(f64::from)) * SQRT_2);
            assert!(down <= STOP_DB, "{hz} Hz folds at {down:.1} dB");
        }
    }

    #[test]
    fn an_impulse_is_delayed_by_under_0_4_ms() {
        const AT: u32 = 96;
        const DELAY_UNDER_S: f64 = 0.0004;
        for (from, to) in RATIOS {
            let mut input = vec![0.0_f32; 2 * usize::try_from(AT).unwrap()];
            input[usize::try_from(AT).unwrap()] = 1.0;
            let mut out = Vec::new();
            Resampler::new(from, to).unwrap().push(&input, &mut out);
            let peak = (0_u32..)
                .zip(&out)
                .max_by(|(_, one), (_, other)| one.abs().total_cmp(&other.abs()))
                .map(|(frame, _)| frame)
                .unwrap();
            let delay = seconds(peak, to) - seconds(AT, from);
            assert!(
                (0.0..DELAY_UNDER_S).contains(&delay),
                "{from} to {to}: {:.3} ms",
                delay * 1000.0
            );
        }
    }

    #[test]
    fn equal_rates_build_nothing() {
        for rate in [LOW_HZ, HIGH_HZ, 0, u32::MAX] {
            assert!(Resampler::new(rate, rate).is_none(), "{rate}");
        }
        for (from, to) in RATIOS {
            assert!(Resampler::new(from, to).is_some(), "{from} to {to}");
        }
        // Only four to three and three to four are built.
        for (from, to) in [
            (96_000, LOW_HZ),
            (16_000, LOW_HZ),
            (HIGH_HZ, 0),
            (0, LOW_HZ),
        ] {
            assert!(Resampler::new(from, to).is_none(), "{from} to {to}");
        }
    }

    #[test]
    fn stack_resampler_matches_reference_kaiser_sinc_within_1e_6() {
        use std::collections::VecDeque;
        use std::f64::consts::PI;

        const CUT: f64 = 0.45;
        const BETA: f64 = 8.2;
        const BESSEL_TERMS: u8 = 32;
        const MAX_DIFF: f32 = 1e-6;

        fn bessel(x: f64) -> f64 {
            let (mut term, mut sum) = (1.0, 1.0);
            for k in 1..=BESSEL_TERMS {
                term *= (x / (2.0 * f64::from(k))).powi(2);
                sum += term;
            }
            sum
        }

        fn ref_taps(up: u8, step: u8) -> Vec<Vec<f32>> {
            let taps = u8::try_from(RESAMPLE_TAPS).unwrap();
            let centre = (f64::from(up) * f64::from(taps) - 1.0) / 2.0;
            let turn = PI * 2.0 * CUT / f64::from(up.max(step));
            (0..up)
                .map(|phase| {
                    let raw: Vec<f64> = (0..taps)
                        .map(|tap| {
                            let off = f64::from(phase) + f64::from(tap) * f64::from(up) - centre;
                            let win = bessel(BETA * (1.0 - (off / centre).powi(2)).sqrt());
                            (turn * off).sin() / (turn * off) * win
                        })
                        .collect();
                    let sum: f64 = raw.iter().sum();
                    raw.into_iter().map(|v| sample(v / sum)).collect()
                })
                .collect()
        }

        // Deterministic multi-component waveform spanning full scale.
        let input: Vec<f32> = (0..FRAMES)
            .map(|i| {
                let t = seconds(i, HIGH_HZ);
                let v = 0.45 * (TAU * 440.0 * t).sin()
                    + 0.35 * (TAU * 3700.0 * t).cos()
                    + 0.20 * (TAU * 11_300.0 * t).sin();
                sample(v)
            })
            .collect();

        for ((from, to), (up, step)) in RATIOS.into_iter().zip([(3_u8, 4_u8), (4_u8, 3_u8)]) {
            let phases = ref_taps(up, step);
            let mut history: VecDeque<f32> = std::iter::repeat_n(0.0, RESAMPLE_TAPS).collect();
            let mut phase = 0_usize;
            let mut ref_f32 = Vec::new();
            let mut ref_f64 = Vec::new();
            for &x in &input {
                history.pop_back();
                history.push_front(x);
                while let Some(taps) = phases.get(phase) {
                    let sum_f32: f32 = taps.iter().zip(&history).map(|(t, s)| t * s).sum();
                    let sum_f64: f64 = taps
                        .iter()
                        .zip(&history)
                        .map(|(&t, &s)| f64::from(t) * f64::from(s))
                        .sum();
                    ref_f32.push(sum_f32);
                    ref_f64.push(sample(sum_f64));
                    phase += usize::from(step);
                }
                phase -= phases.len();
            }

            let mut actual = Vec::new();
            Resampler::new(from, to).unwrap().push(&input, &mut actual);
            assert_eq!(actual.len(), ref_f32.len(), "{from} to {to}");
            let max_diff_f32 = actual
                .iter()
                .zip(&ref_f32)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0_f32, f32::max);
            let max_diff_f64 = actual
                .iter()
                .zip(&ref_f64)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0_f32, f32::max);
            assert!(
                max_diff_f32 <= MAX_DIFF,
                "{from} to {to}: max diff vs serial f32 {max_diff_f32:e}"
            );
            assert!(
                max_diff_f64 <= MAX_DIFF,
                "{from} to {to}: max diff vs f64 {max_diff_f64:e}"
            );
        }
    }

    fn block() -> impl Strategy<Value = Vec<f32>> {
        proptest::collection::vec(-1.0_f32..1.0, 0..BLOCK_UNDER)
    }

    proptest! {
        #[test]
        fn a_block_split_anywhere_gives_the_same_samples(
            input in block(),
            cuts in any::<[prop::sample::Index; 2]>(),
            ratio in 0..RATIOS.len(),
        ) {
            let (from, to) = RATIOS[ratio];
            let mut whole = Vec::new();
            Resampler::new(from, to).unwrap().push(&input, &mut whole);
            let mut cuts = cuts.map(|cut| cut.index(input.len().saturating_add(1)));
            cuts.sort_unstable();
            let (mut split, mut parts) = (Resampler::new(from, to).unwrap(), Vec::new());
            for part in [&input[..cuts[0]], &input[cuts[0]..cuts[1]], &input[cuts[1]..]] {
                split.push(part, &mut parts);
            }
            prop_assert_eq!(whole, parts);
        }

        #[test]
        fn a_reset_resampler_gives_what_a_new_one_gives(
            before in block(),
            input in block(),
            ratio in 0..RATIOS.len(),
        ) {
            let (from, to) = RATIOS[ratio];
            let mut new = Vec::new();
            Resampler::new(from, to).unwrap().push(&input, &mut new);
            let (mut used, mut again) = (Resampler::new(from, to).unwrap(), Vec::new());
            used.push(&before, &mut again);
            used.reset();
            again.clear();
            used.push(&input, &mut again);
            prop_assert_eq!(new, again);
        }
    }
}
