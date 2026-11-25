//! Support tests of what the chain makes of its units, through its public API, with the test owning the clock; the chain has no user path yet.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the suites of this crate share one rig, and this one leaves some of its helpers unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pods_mic::decode::{Decoded, Decoder};
    use pods_mic::{MAX_UNITS_PER_SDU, NODE_LATENCY_MS, SAMPLES_PER_UNIT, STAMP_STEP};
    use pods_samples::uplink::{silent_unit, tone_unit};

    use super::support::*;

    /// The level the line tests send their tone at, and how many steps under it the quieter tone stands.
    const LEVEL: u8 = 180;
    const STEPS: u8 = 6;
    /// One step of the level is a quarter of a doubling of the amplitude.
    const STEP_DB: f64 = 1.505;
    const DB_SLACK: f64 = 0.5;

    fn rms(samples: &[f32]) -> f64 {
        let squares: f64 = samples
            .iter()
            .map(|&sample| f64::from(sample).powi(2))
            .sum();
        (squares / f64::from(u32::try_from(samples.len()).unwrap())).sqrt()
    }

    /// The power of `samples` at the frequency that turns `turns` times over their length.
    fn power_at(samples: &[f32], turns: u16) -> f64 {
        let length = f64::from(u32::try_from(samples.len()).unwrap());
        let turn = 2.0 * std::f64::consts::PI * f64::from(turns) / length;
        let (mut real, mut imaginary) = (0.0, 0.0);
        for (at, &sample) in (0_u32..).zip(samples) {
            real += f64::from(sample) * (turn * f64::from(at)).cos();
            imaginary += f64::from(sample) * (turn * f64::from(at)).sin();
        }
        real * real + imaginary * imaginary
    }

    /// One period of a run that holds the tone in every fourth unit and silence in the rest, as a recorder that links after five packets hears the last of them.
    fn heard_of(line: u16, level: u8) -> Vec<f32> {
        let mut rig = Rig::new();
        let packet = [
            tone_unit(line, level),
            silent_unit(),
            silent_unit(),
            silent_unit(),
        ];
        for at in 0..5 {
            rig.send(landing(at), &packet);
        }
        rig.resync.request();
        let (heard, _) = rig.read(
            landing(4).saturating_add(ms(NODE_LATENCY_MS)),
            5 * PACKET_FRAMES,
        );
        heard[4 * PACKET_FRAMES..].to_vec()
    }

    #[test]
    fn tone_units_come_out_bit_equal_to_the_decoder() {
        let mut rig = Rig::new();
        rig.resync.request();
        let from = FIRST + period() * 3;
        let tape = rig.play(&on_time(0..40), &reads(from, 50));
        let base = base_of(from);
        assert_same(&tape[FADE..], &rig.decoded[base + FADE..base + tape.len()]);
        assert!(largest_step(&tape) > 0.01, "the run is close to silence");
        let untouched = [&rig.counters.underruns, &rig.counters.concealed];
        assert_eq!(untouched.map(Rig::count), [0, 0]);
    }

    #[test]
    fn a_run_of_tone_units_decodes_to_its_line_and_six_steps_are_9_db() {
        // The run repeats every packet, and line k stands at k and a half times 66.7 Hz: 2k + 1 turns over one period, of the half a period's frames a real signal can turn.
        let most = u16::try_from(PACKET_FRAMES / 2).unwrap();
        for line in [4, 5, 6, 7, 20, 340] {
            let heard = heard_of(line, LEVEL);
            let power = |turns: &u16| power_at(&heard, *turns);
            let strongest = (0..most).max_by(|a, b| power(a).total_cmp(&power(b)));
            assert_eq!(strongest, Some(2 * line + 1), "line {line}");
        }
        let quieter = rms(&heard_of(20, LEVEL - STEPS));
        let louder_db = 20.0 * (rms(&heard_of(20, LEVEL)) / quieter).log10();
        let steps_db = STEP_DB * f64::from(STEPS);
        assert!((louder_db - steps_db).abs() < DB_SLACK, "{louder_db} dB");
    }

    #[test]
    fn a_lost_unit_is_never_a_block_of_zeros_and_never_a_step() {
        let mut rig = Rig::new();
        let unit = usize::from(SAMPLES_PER_UNIT);
        rig.land(FIRST);
        rig.land_units(FIRST + period(), 2);
        // The stamps pass over one unit, which no decoder sees.
        rig.stamp = rig.stamp.wrapping_add(STAMP_STEP);
        rig.land_units(FIRST + period(), 1);
        rig.land(FIRST + period() * 2);
        assert_eq!(Rig::count(&rig.counters.concealed), 1);

        rig.resync.request();
        let (heard, _) = rig.read(FIRST + period() * 2 + ms(NODE_LATENCY_MS), 12 * unit);
        let lost = 6 * unit;
        assert_shaped(&heard[lost..lost + unit], falling(&rig.decoded[..lost]));
        assert!(
            largest_step(&heard[lost..lost + unit]) > 0.0,
            "a block of zeros"
        );
        assert!(
            largest_step(&heard) <= largest_step(&levelled(&rig.decoded)),
            "a step"
        );
        // The unit after the lost one stands one unit later than the twin decoded it, and fades in.
        let after = lost + unit;
        let rising = (0..FADE).map(|k| rig.decoded[lost + k] * rise(k));
        assert_shaped(&heard[after..after + FADE], rising);
        assert_same(&heard[after + FADE..], &rig.decoded[lost + FADE..]);
    }

    #[test]
    fn a_hostile_sdu_decodes_at_most_eight_units() {
        const SENT: u32 = 20;
        let mut rig = Rig::new();
        rig.land_units(FIRST, SENT);
        let dropped = usize::try_from(Rig::count(&rig.counters.decode_errors)).unwrap();
        assert_eq!(dropped + MAX_UNITS_PER_SDU, usize::try_from(SENT).unwrap());

        // The twin decoded all twenty; the chain's samples are its first eight units.
        let kept = MAX_UNITS_PER_SDU * usize::from(SAMPLES_PER_UNIT);
        rig.resync.request();
        let (heard, _) = rig.read(FIRST + ms(NODE_LATENCY_MS), rig.decoded.len());
        let (before, start) = heard.split_at(heard.len() - kept);
        assert!(silent(before));
        assert_shaped(&start[..FADE], run_start(&rig.decoded));
        assert_same(&start[FADE..], &rig.decoded[FADE..kept]);
    }

    #[test]
    fn a_stamp_far_ahead_starts_a_new_run_and_writes_nothing_for_the_hole() {
        let far = tone_unit(LINES[1], LEVELS[1]);
        let mut fresh = Decoder::new().unwrap();
        let Decoded::Samples(alone) = fresh.unit(&far) else {
            panic!("the twin refused the unit");
        };
        let after_a_hole = |hole: u32| {
            let mut rig = Rig::new();
            rig.land(FIRST);
            rig.stamp = rig.stamp.wrapping_add(hole);
            rig.send(FIRST + period(), std::slice::from_ref(&far));
            rig
        };
        // Eight lost units are the most a stamp may claim.
        let longest = after_a_hole(8 * STAMP_STEP);
        assert_eq!(Rig::count(&longest.counters.concealed), 8);
        // One more, half the range, and the stamp of the unit before are each a new run.
        for hole in [9 * STAMP_STEP, u32::MAX / 2, STAMP_STEP.wrapping_neg()] {
            let mut rig = after_a_hole(hole);
            assert_eq!(Rig::count(&rig.counters.concealed), 0, "hole {hole}");
            rig.resync.request();
            let (heard, _) = rig.read(
                FIRST + period() + ms(NODE_LATENCY_MS),
                PACKET_FRAMES + alone.len(),
            );
            // The unit after the hole is the newest the ring holds, a fresh decoder's first; the run before it reads as nothing.
            let (before, start) = heard.split_at(PACKET_FRAMES);
            assert!(silent(before), "hole {hole}");
            assert_shaped(&start[..FADE], run_start(alone));
            let fresh = bits(&levelled(&alone[FADE..]));
            assert_eq!(bits(&start[FADE..]), fresh, "hole {hole}");
        }
    }

    #[test]
    fn a_unit_of_the_wrong_length_is_an_error_not_a_panic() {
        let mut rig = Rig::new();
        rig.land(FIRST);
        let whole = tone_unit(LINES[0], LEVELS[0]);
        let cut = whole[..whole.len() - 1].to_vec();
        // Nothing at all, a unit cut short, and one that declares more bands than a frame has.
        let bad = [Vec::new(), cut, vec![0xff, 0xff]];
        rig.send(FIRST + period(), &bad);
        rig.land(FIRST + period() * 2);
        let counters = &rig.counters;
        let counted = [
            &counters.decode_errors,
            &counters.concealed,
            &counters.decoder_resets,
        ];
        assert_eq!(counted.map(Rig::count), [3, 3, 0]);
        // Of units refused in a row the first is the unit before backwards, and the rest are silence.
        let unit = usize::from(SAMPLES_PER_UNIT);
        rig.resync.request();
        let (heard, _) = rig.read(FIRST + period() * 2 + ms(NODE_LATENCY_MS), 12 * unit);
        let (refused, after) = (4 * unit, 7 * unit);
        assert_shaped(
            &heard[refused..refused + unit],
            falling(&rig.decoded[..refused]),
        );
        assert!(largest_step(&heard[refused..refused + unit]) > 0.0);
        assert!(silent(&heard[refused + unit..after]));
        // The decoder kept its state through the refusals: the packet after them fades in on what the twin, which saw none, decoded.
        let rising = (0..FADE).map(|k| rig.decoded[refused + k] * rise(k));
        assert_shaped(&heard[after..after + FADE], rising);
        assert_same(
            &heard[after + FADE..after + refused],
            &rig.decoded[refused + FADE..],
        );
        let mut twin = Decoder::new().unwrap();
        assert!(matches!(twin.unit(&bad[1]), Decoded::Error));
        assert!(matches!(twin.unit(&silent_unit()), Decoded::Samples(_)));
    }

    #[test]
    fn a_reset_plays_only_the_fall_of_what_was_playing() {
        let mut rig = Rig::new();
        rig.resync.request();
        let from = FIRST + period() * 3;
        let before = rig.play(&on_time(0..10), &reads(from, 10));
        assert!(largest_step(&before) > 0.01, "no audio came");
        // The ring still holds 85 ms of the first run that the reader had not come to.
        rig.chain.reset();
        let mut between = Vec::new();
        // A run with no packet yet has no target, so reads 2 ms off their time are asked for no other rate.
        for now in reads(landing(3) + QUANTUM_TIME * 10 + ms(2), 10) {
            let (heard, ratio) = rig.read(now, QUANTUM);
            between.extend(heard);
            assert!(unsteered(ratio), "{ratio}");
        }
        // What was playing falls away as audio that ran dry does, and nothing of the run comes after it.
        let unit = usize::from(SAMPLES_PER_UNIT);
        let ended = base_of(from) + before.len();
        assert_shaped(&between[..unit], falling(&rig.decoded[..ended]));
        assert!(largest_step(&between[..unit]) > 0.0, "a step to silence");
        assert!(silent(&between[unit..]));

        rig.renew_twin();
        let again = FIRST + period() * 20;
        let lands: Vec<Duration> = (0..10).map(|packet| again + period() * packet).collect();
        let from = again + period() * 3;
        let after = rig.play(&lands, &reads(from, 10));
        // The frame of the second run the tape opens on, counted from that run's own first.
        let base = due((from + FIRST).checked_sub(again).unwrap()) - QUANTUM;
        assert_same(
            &after[QUANTUM..],
            &rig.decoded[base + QUANTUM..base + after.len()],
        );
        assert_eq!(Rig::count(&rig.counters.underruns), 0);
    }
}
