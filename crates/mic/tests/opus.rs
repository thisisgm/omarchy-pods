//! Support tests of the Opus decoder through its public API, on a synthetic tone and on bytes no pods would send; the decoder has no user path yet.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the suites of this crate share one rig, and this one takes only the slack of a decoded tone from it"
)]
mod support;

#[cfg(test)]
mod tests {
    use pods_mic::decode::Decoded;
    use pods_mic::opus::{DICTATION_DECODE_HZ, Decoder, FRAME_SAMPLES, MAX_OPUS_FRAME_BYTES};
    use pods_samples::opus::{MARK_HZ, TONE_DBFS, TONE_HZ, mark, tone};
    use proptest::prelude::*;

    use super::support::{TONE_SLACK_DB, TONE_SLACK_HZ};

    /// The table of contents byte of the stream's frames, and that of a 10 ms frame of the same mode.
    const TOC_20MS: u8 = 0xb8;
    const TOC_10MS: u8 = 0xb0;
    /// The frames of the fixture, which is one second.
    const FIXTURE_FRAMES: usize = 50;
    /// How far around the tone the loudest frequency is looked for.
    const SEARCH_HZ: u32 = 50;
    /// How far under its own sine a fixture holds the other fixture's, at the least.
    const APART_DB: f64 = 40.0;

    fn samples(decoded: Decoded<'_>) -> Vec<f32> {
        let Decoded::Samples(samples) = decoded else {
            panic!("no samples: {decoded:?}");
        };
        samples.to_vec()
    }

    /// A decoder that has heard `fixture` for its second, and the second of it that the decoder gave next.
    fn warmed(fixture: &[&[u8]]) -> (Decoder, Vec<f32>) {
        let mut decoder = Decoder::new().unwrap();
        for frame in fixture {
            drop(samples(decoder.frame(frame)));
        }
        let mut heard = Vec::new();
        for frame in fixture {
            heard.extend(samples(decoder.frame(frame)));
        }
        (decoder, heard)
    }

    /// The level in dBFS of the sine in `samples` that turns `turns` times over their length.
    fn level_at(samples: &[f32], turns: u32) -> f64 {
        let length = f64::from(u32::try_from(samples.len()).unwrap());
        let peak = 2.0 * power_at(samples, turns).sqrt() / length;
        20.0 * peak.log10()
    }

    /// The power of `samples` at the frequency that turns `turns` times over their length.
    fn power_at(samples: &[f32], turns: u32) -> f64 {
        let length = f64::from(u32::try_from(samples.len()).unwrap());
        let turn = 2.0 * std::f64::consts::PI * f64::from(turns) / length;
        let (mut real, mut imaginary) = (0.0, 0.0);
        for (at, &sample) in (0_u32..).zip(samples) {
            real += f64::from(sample) * (turn * f64::from(at)).cos();
            imaginary += f64::from(sample) * (turn * f64::from(at)).sin();
        }
        real * real + imaginary * imaginary
    }

    fn largest_step(samples: &[f32]) -> f32 {
        let steps = samples.windows(2).map(|pair| (pair[1] - pair[0]).abs());
        steps.fold(0.0, f32::max)
    }

    #[test]
    fn each_fixture_decodes_to_its_sine_within_1_db_and_5_hz() {
        let fixtures: [(Vec<&[u8]>, u32, u32); 2] = [
            (tone().collect(), TONE_HZ, MARK_HZ),
            (mark().collect(), MARK_HZ, TONE_HZ),
        ];
        for (fixture, hz, other) in fixtures {
            let (_, heard) = warmed(&fixture);
            // One second of samples, so a frequency in Hz turns that many times over them.
            let second = u32::try_from(heard.len()).unwrap();
            assert_eq!(second, DICTATION_DECODE_HZ);
            let near = hz - SEARCH_HZ..=hz + SEARCH_HZ;
            let loudest = near
                .max_by(|&a, &b| power_at(&heard, a).total_cmp(&power_at(&heard, b)))
                .unwrap();
            assert!(
                loudest.abs_diff(hz) <= TONE_SLACK_HZ,
                "the sine of {hz} Hz is at {loudest} Hz"
            );
            let level = level_at(&heard, loudest);
            assert!(
                (level - f64::from(TONE_DBFS)).abs() <= TONE_SLACK_DB,
                "the sine of {hz} Hz is at {level} dBFS"
            );
            // A test tells the two apart by what each holds at the other's frequency, which is next to nothing.
            let near = other - SEARCH_HZ..=other + SEARCH_HZ;
            let stray = near.map(|hz| level_at(&heard, hz)).fold(f64::MIN, f64::max);
            assert!(
                stray <= level - APART_DB,
                "the fixture of {hz} Hz holds {stray} dBFS near {other} Hz"
            );
        }
    }

    #[test]
    fn every_fixture_frame_gives_960_samples() {
        let mut decoder = Decoder::new().unwrap();
        let lengths: Vec<usize> = tone()
            .map(|frame| samples(decoder.frame(frame)).len())
            .collect();
        assert_eq!(lengths, [960; FIXTURE_FRAMES]);
        assert_eq!(FRAME_SAMPLES, 960);
    }

    #[derive(Debug, PartialEq)]
    enum Met {
        Samples,
        Error,
        Panic,
    }

    /// What a decoder in the middle of the tone makes of `frame`, and whether it then goes on as one that never met it.
    fn met_and_untouched(frame: &[u8]) -> (Met, bool) {
        let frames: Vec<&[u8]> = tone().collect();
        let (before, after) = frames.split_at(FIXTURE_FRAMES / 2);
        let mut decoder = Decoder::new().unwrap();
        let mut twin = Decoder::new().unwrap();
        for frame in before {
            assert_eq!(decoder.frame(frame), twin.frame(frame));
        }
        let met = match decoder.frame(frame) {
            Decoded::Samples(_) => Met::Samples,
            Decoded::Error => Met::Error,
            Decoded::Panic => Met::Panic,
        };
        let same = after
            .iter()
            .all(|frame| decoder.frame(frame) == twin.frame(frame));
        (met, same)
    }

    #[test]
    fn a_frame_of_95_bytes_is_an_error_and_never_reaches_the_decoder() {
        let mut long = vec![0x55; MAX_OPUS_FRAME_BYTES + 1];
        long[0] = TOC_20MS;
        assert_eq!(long.len(), 95);
        assert_eq!(met_and_untouched(&long), (Met::Error, true));
        // The same bytes cut to the cap are a frame, and a decoder that took them is no longer the twin's equal.
        let at_the_cap = &long[..MAX_OPUS_FRAME_BYTES];
        assert_eq!(met_and_untouched(at_the_cap), (Met::Samples, false));
    }

    #[test]
    fn an_empty_frame_is_an_error_and_never_reaches_the_decoder() {
        assert_eq!(met_and_untouched(&[]), (Met::Error, true));
    }

    #[test]
    fn a_frame_of_another_duration_is_an_error() {
        let mut decoder = Decoder::new().unwrap();
        let mut short = tone().next().unwrap().to_vec();
        short[0] = TOC_10MS;
        assert_eq!(decoder.frame(&short), Decoded::Error);
    }

    #[test]
    fn concealment_gives_960_samples_with_no_step_at_the_join() {
        let (mut decoder, heard) = warmed(&tone().collect::<Vec<_>>());
        let own = largest_step(&heard);
        let concealed = samples(decoder.conceal());
        assert_eq!(concealed.len(), 960);
        let back = samples(decoder.frame(tone().next().unwrap()));
        let joins = [
            [*heard.last().unwrap(), concealed[0]],
            [*concealed.last().unwrap(), back[0]],
        ];
        for join in joins {
            assert!(
                largest_step(&join) <= own,
                "a step of {} at a join, where the tone's largest is {own}",
                largest_step(&join)
            );
        }
        assert!(
            largest_step(&concealed) > own / 2.0,
            "the concealment is close to silence"
        );
    }

    /// Any bytes of any length up to past the cap; half of them open as the stream's own frames do.
    fn hostile_frame() -> impl Strategy<Value = Vec<u8>> {
        let bytes = proptest::collection::vec(any::<u8>(), 0..=MAX_OPUS_FRAME_BYTES + 2);
        (bytes, any::<bool>()).prop_map(|(mut frame, as_the_stream)| {
            if let (Some(toc), true) = (frame.first_mut(), as_the_stream) {
                *toc = TOC_20MS;
            }
            frame
        })
    }

    proptest! {
        #[test]
        fn hostile_bytes_never_panic_out(
            // None is a frame that never came.
            steps in proptest::collection::vec(proptest::option::weighted(0.8, hostile_frame()), 1..24),
        ) {
            let mut decoder = Decoder::new().unwrap();
            for step in &steps {
                let Some(frame) = step else {
                    decoder.conceal();
                    continue;
                };
                let outside = frame.is_empty() || frame.len() > MAX_OPUS_FRAME_BYTES;
                let decoded = decoder.frame(frame);
                prop_assert!(!outside || decoded == Decoded::Error, "{} bytes gave {decoded:?}", frame.len());
            }
        }
    }
}
