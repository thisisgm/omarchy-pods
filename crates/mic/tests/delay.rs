//! Support tests of the delay the reader holds and of every edge it hides, through the chain's public API, with the test owning the clock; the chain has no user path yet.

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

    use pods_mic::decode::Decoded;
    use pods_mic::level::LIMIT_CEILING_DBFS;
    use pods_mic::{JITTER_COVER_MS, NODE_LATENCY_MS, SAMPLES_PER_UNIT};
    use pods_samples::uplink::tone_unit;

    use super::support::*;

    /// A run with one gap: twenty packets on time, then nothing for 150 ms, then the five packets that were due land together and the rest on time. With the tape, where on it the audio is back.
    fn gap_run() -> (Rig, Vec<f32>, usize) {
        let mut rig = Rig::new();
        let late = landing(24);
        let mut lands = on_time(0..20);
        lands.extend([late; 5]);
        lands.extend(on_time(25..40));
        rig.resync.request();
        let times = reads(landing(3), 50);
        let tape = rig.play(&lands, &times);
        let back = times.iter().position(|&now| now >= late).unwrap();
        (rig, tape, back.saturating_mul(QUANTUM))
    }

    /// Two ms: a read shorter than any fall.
    const SHORT: usize = 128;

    /// A rig whose audio ended twice inside one unit, and the time of its last read: the ring ran dry, the late packet played for 128 frames, and the run ended 192 frames into the fall.
    fn two_ends() -> (Rig, Duration) {
        let mut rig = Rig::new();
        rig.land(FIRST);
        rig.resync.request();
        // The first read ends 64 frames before the ring runs dry.
        let start = after(FIRST, ms(1), NODE_LATENCY_MS.saturating_sub(1));
        drop(rig.read(start, QUANTUM));
        let dry = after(start, ms(2), 1);
        drop(rig.read(dry, SHORT));
        assert_eq!(Rig::count(&rig.counters.underruns), 1);
        rig.land(dry);
        let back = after(start, ms(2), 2);
        drop(rig.read(back, SHORT));
        rig.chain.reset();
        (rig, back)
    }

    #[test]
    fn a_recorder_linking_at_any_moment_gets_the_same_delay() {
        const MARKED: u32 = 10;
        for lag_ms in [0, 7, 15, 29] {
            let mut rig = Rig::new();
            let link = FIRST + period() * 5 + Duration::from_millis(lag_ms);
            for at in on_time(0..6) {
                rig.land(at);
            }
            rig.resync.request();
            let times = reads(link, 20);
            let tape = rig.play(&on_time(6..20), &times);

            let landed = FIRST + period() * MARKED;
            let first = usize::try_from(MARKED).unwrap() * PACKET_FRAMES;
            let last = first + PACKET_FRAMES - 1;
            let place =
                |frame: usize| find(&tape, &levelled(&rig.decoded[frame..frame + 64])).unwrap();
            let (first_in, last_in) = (place(first) / QUANTUM, (place(last - 63) + 63) / QUANTUM);
            let (cover, latency) = (landed + ms(JITTER_COVER_MS), landed + ms(NODE_LATENCY_MS));
            assert!(
                times[first_in] > cover && times[first_in - 1] <= cover,
                "{lag_ms} ms"
            );
            assert!(
                times[last_in] >= latency && times[last_in - 1] < latency,
                "{lag_ms} ms"
            );
        }
    }

    #[test]
    fn a_gap_past_the_cover_fades_out_and_never_steps_to_zero() {
        let (rig, tape, back) = gap_run();
        assert_eq!(Rig::count(&rig.counters.underruns), 1);
        let unit = usize::from(SAMPLES_PER_UNIT);
        let ran_out = 20 * PACKET_FRAMES;
        let dry = ran_out - base_of(FIRST + period() * 3);
        assert_shaped(&tape[dry..dry + unit], falling(&rig.decoded[..ran_out]));
        assert!(
            largest_step(&tape[dry..dry + unit]) > 0.0,
            "the dry stretch opens silent"
        );
        assert!(
            largest_step(&tape) <= largest_step(&levelled(&rig.decoded)),
            "a step"
        );
        // The late packets land 65 ms after the ring ran dry, and the unit played backwards is 7.5 ms of that.
        assert!(
            back > dry + 2 * QUANTUM,
            "the gap was {} frames",
            back - dry
        );
        assert!(silent(&tape[dry + unit..back]));
    }

    #[test]
    fn late_audio_is_dropped_not_delayed() {
        let (rig, tape, at) = gap_run();
        let base = base_of(FIRST + period() * 3);
        // The frame of the run that the first sample after the gap is due to be: the reader moved on with time while the ring was dry.
        let frame = base + at;
        assert!(silent(&tape[at - QUANTUM..at]));
        let rising = (0..FADE).map(|k| rig.decoded[frame + k] * rise(k));
        assert_shaped(&tape[at..at + FADE], rising);
        assert_same(
            &tape[at + FADE..],
            &rig.decoded[frame + FADE..base + tape.len()],
        );
        // A unit before the gap and one after it stand as far from where they were decoded: nothing was held back.
        let stretch = |frame: usize| levelled(&rig.decoded[frame..frame + 64]);
        let delay_of = |frame: usize| find(&tape, &stretch(frame)).unwrap() + base;
        let (before, after) = (10 * PACKET_FRAMES, 30 * PACKET_FRAMES);
        assert_eq!((delay_of(before), delay_of(after)), (before, after));
    }

    #[test]
    fn the_time_before_the_first_audio_is_silence_and_not_an_underrun() {
        let mut rig = Rig::new();
        // An SDU with no unit in it is no arrival.
        rig.send(Duration::ZERO, &[]);
        for now in reads(Duration::ZERO, 50) {
            let (heard, ratio) = rig.read(now, QUANTUM);
            assert!(silent(&heard));
            assert!(unsteered(ratio), "{ratio}");
        }
        let tape = rig.play(&on_time(30..40), &reads(FIRST + period() * 30, 10));
        assert!(largest_step(&tape) > 0.01, "no audio came");
        assert_eq!(Rig::count(&rig.counters.underruns), 0);
    }

    #[test]
    fn resync_gives_live_audio_on_the_first_read() {
        const LONG: usize = HELD + 4096;
        let mut rig = Rig::new();
        // The last of eleven reads runs 200 frames past the audio of ten packets, so what ended still sounds when the recorder leaves.
        let first = landing(3) + ms(65) + Duration::from_micros(3125);
        let left = first + QUANTUM_TIME * 10;
        let heard_before = rig.play(&on_time(0..10), &reads(first, 11));
        assert!(largest_step(&heard_before) > 0.01, "no audio came");
        assert_eq!(due(left), 10 * PACKET_FRAMES + 200);

        // A recorder links while the ring is dry: it gets nothing of the audio that ended, and no underrun of its own.
        rig.resync.request();
        assert!(silent(&rig.read(left + QUANTUM_TIME, QUANTUM).0));
        assert_eq!(Rig::count(&rig.counters.underruns), 1);
        // Three packets land late, and the next read fades in on the audio due then.
        for _ in 10..13 {
            rig.land(left + ms(30));
        }
        let back = left + QUANTUM_TIME * 2;
        assert_live(&mut rig, back);
        // No read for 97 ms: that recorder left with audio in hand. Another links, and its first read fades in from nothing.
        for at in on_time(13..17) {
            rig.land(at);
        }
        rig.resync.request();
        assert_live(&mut rig, back + ms(97));

        // A read longer than the ring holds: what the writer has overwritten, or may be writing over, is silence and never newer audio.
        for at in on_time(17..72) {
            rig.land(at);
        }
        rig.resync.request();
        let (long, _) = rig.read(landing(71) + ms(NODE_LATENCY_MS), LONG);
        let (lost, kept) = long.split_at(LONG - HELD);
        assert!(silent(lost));
        assert_same(
            &kept[FADE..],
            &rig.decoded[rig.decoded.len() - HELD + FADE..],
        );
    }

    #[test]
    fn a_reader_off_its_target_realigns_with_no_step() {
        const MISSED: Duration = Duration::from_millis(25);
        let mut rig = Rig::new();
        rig.resync.request();
        let from = FIRST + period() * 3;
        let mut times = reads(from, 10);
        times.extend(reads(from + QUANTUM_TIME * 10 + MISSED, 10));
        let tape = rig.play(&on_time(0..30), &times);

        let expected = |read: usize| {
            let end = due(times[read]);
            bits(&levelled(&rig.decoded[end - QUANTUM..end]))
        };
        let heard = |read: usize| bits(&tape[read * QUANTUM..(read + 1) * QUANTUM]);
        for read in (1..20).filter(|&read| read != 10) {
            assert_eq!(heard(read), expected(read), "read {read}");
        }
        // The one read that jumped ends on its target too, past the fade.
        assert_eq!(heard(10)[FADE..], expected(10)[FADE..]);
        assert_ne!(heard(10)[..FADE], expected(10)[..FADE]);
        assert!(
            largest_step(&tape) <= largest_step(&levelled(&rig.decoded)),
            "a step"
        );
        assert_eq!(Rig::count(&rig.counters.underruns), 0);
    }

    #[test]
    fn the_sum_of_a_fade_out_and_a_fade_in_never_passes_the_ceiling() {
        const MISSED: Duration = Duration::from_millis(25);
        /// A tone that decodes just under full scale, so after the gain both sides of the realign stand far over the knee.
        const FULL_LEVEL: u8 = 195;
        let ceiling = 10_f32.powf(LIMIT_CEILING_DBFS / 20.0);
        let mut rig = Rig::new();
        let packet =
            vec![tone_unit(LINES[1], FULL_LEVEL); usize::try_from(UNITS_PER_PACKET).unwrap()];
        for at in 0..30 {
            for unit in &packet {
                let Decoded::Samples(samples) = rig.twin.unit(unit) else {
                    panic!("the twin refused the tone");
                };
                rig.decoded.extend_from_slice(samples);
            }
            rig.send(landing(at), &packet);
        }
        rig.resync.request();
        let from = FIRST + period() * 3;
        let mut times = reads(from, 10);
        times.extend(reads(from + QUANTUM_TIME * 10 + MISSED, 10));
        let mut tape = Vec::new();
        for &now in &times {
            tape.extend(rig.read(now, QUANTUM).0);
        }
        let loudest = tape
            .iter()
            .fold(0.0_f32, |most, sample| most.max(sample.abs()));
        assert!(
            loudest < ceiling,
            "{loudest} at or over the ceiling {ceiling}"
        );

        // The read that jumped: what it left falls, backwards from its last frame, under the rise of what it came to.
        let (left, came) = (due(times[9]) - 1, due(times[10]) - QUANTUM);
        let falls = |at: usize| rig.decoded[left - at] * fall(at, FADE);
        let rises = |at: usize| rig.decoded[came + at] * rise(at);
        let jumped = &tape[10 * QUANTUM..][..FADE];
        assert_shaped(jumped, (0..FADE).map(|at| rises(at) + falls(at)));
        // Each side under the level alone would sum past the ceiling: the level comes after the sum.
        let apart = (0..FADE).map(|at| levelled(&[rises(at)])[0] + levelled(&[falls(at)])[0]);
        let apart = apart.fold(0.0_f32, |most, sum| most.max(sum.abs()));
        assert!(apart > ceiling, "the two sides sum to {apart} at most");
        assert!(Rig::count(&rig.counters.limited) > 0);
        assert_eq!(Rig::count(&rig.counters.nonfinite), 0);
    }

    #[test]
    fn a_second_realign_waits_for_the_first_to_fade() {
        const MISSED: Duration = Duration::from_millis(25);
        for lag_ms in 0..8 {
            let mut rig = Rig::new();
            for at in on_time(0..8) {
                rig.land(at);
            }
            rig.resync.request();
            let mut now = landing(5) + ms(lag_ms);
            let mut tape = rig.read(now, QUANTUM).0;
            let mut times = Vec::new();
            // A consumer that takes 2 ms at a time misses 25 ms, twice in a row.
            for late in [MISSED, MISSED].into_iter().chain([Duration::ZERO; 5]) {
                now += ms(2) + late;
                times.push(now);
                tape.extend(rig.read(now, SHORT).0);
            }
            let heard = |read: usize| &tape[QUANTUM + read * SHORT..][..SHORT];
            let owed = |read: usize| &rig.decoded[due(times[read]) - SHORT..due(times[read])];
            // The first realign fades for 320 frames, so the second comes at read 3: past that fade read 2 still runs on from read 0, and read 6 is past the second.
            let (clear, on) = (FADE - SHORT * 2, due(times[0]) + SHORT);
            assert_same(&heard(2)[clear..], &rig.decoded[on + clear..on + SHORT]);
            assert_same(heard(6), owed(6));
            assert!(
                largest_step(&tape) <= largest_step(&levelled(&rig.decoded)),
                "a step, {lag_ms} ms"
            );
        }
    }

    #[test]
    fn audio_that_ends_while_it_fades_in_falls_from_the_level_it_stood_at() {
        // What the writer left in the ring of a run's first frames, which it fades in.
        let written = |rig: &Rig, frame: usize| {
            let gain = if frame < FADE { rise(frame) } else { 1.0 };
            rig.decoded[frame] * gain
        };
        let unit = usize::from(SAMPLES_PER_UNIT);

        // One unit lands, and a recorder links 200 frames before its end: the ring runs dry 200 frames into the reader's fade in.
        let mut rig = Rig::new();
        rig.land_units(FIRST, 1);
        rig.resync.request();
        let (heard, _) = rig.read(
            FIRST + ms(NODE_LATENCY_MS) + Duration::from_micros(12_500),
            1000,
        );
        let dry = (0..unit).map(|at| written(&rig, unit - 1 - at) * fall(at, unit) * rise(200));
        assert_shaped(&heard[200..200 + unit], dry);
        assert!(silent(&heard[200 + unit..]));

        // Two packets land on time, a recorder links 128 frames into the first, and its next read comes 25 ms late: a jump 128 frames into the fade in.
        let mut rig = Rig::new();
        rig.land(FIRST);
        rig.land(landing(1));
        rig.resync.request();
        let linked = FIRST + ms(JITTER_COVER_MS + 2);
        let late = linked + QUANTUM_TIME + ms(25);
        drop(rig.read(linked, QUANTUM));
        let (heard, _) = rig.read(late, QUANTUM);
        let from = due(late) - QUANTUM;
        let left = |at: usize| {
            let behind = 127_usize.checked_sub(at);
            behind.map_or(0.0, |frame| written(&rig, frame)) * fall(at, FADE) * rise(128)
        };
        let joined = (0..FADE).map(|at| rig.decoded[from + at] * rise(at) + left(at));
        assert_shaped(&heard[..FADE], joined);
        assert_same(&heard[FADE..], &rig.decoded[from + FADE..from + QUANTUM]);
    }

    #[test]
    fn audio_that_ends_while_another_end_falls_cuts_neither() {
        /// How far the first fall is when the second begins.
        const FALLEN: usize = 192;
        let unit = usize::from(SAMPLES_PER_UNIT);
        let (mut rig, back) = two_ends();
        let mut heard = Vec::new();
        for read in 1..=5 {
            heard.extend(rig.read(after(back, ms(2), read), SHORT).0);
        }

        let dried = PACKET_FRAMES;
        let cut = dried + FALLEN;
        let first = |at: usize| rig.decoded[dried - 1 - at] * fall(at, unit);
        let second = |at: usize| rig.decoded[cut - 1 - at] * fall(at, unit) * rise(SHORT);
        let rest = |at: usize| if at < unit { first(at) } else { 0.0 };
        assert_shaped(
            &heard[..unit],
            (0..unit).map(|at| second(at) + rest(FALLEN + at)),
        );
        assert!(silent(&heard[unit..]));
    }

    #[test]
    fn a_recorder_that_links_hears_no_fall_from_before() {
        // The control is a reader no recorder links to: it goes on hearing the falls.
        for links in [false, true] {
            let (mut rig, back) = two_ends();
            assert!(!silent(&rig.read(back + ms(2), SHORT).0), "nothing falls");
            // A resync needs a target, which the first packet of the next run gives; none of that run is due yet.
            rig.land(back + ms(2));
            if links {
                rig.resync.request();
            }
            let (heard, _) = rig.read(back + ms(4), SHORT);
            assert_eq!(silent(&heard), links, "linked: {links}");
        }
    }

    #[test]
    fn a_recorder_that_links_before_a_run_has_a_packet_hears_nothing_from_before() {
        /// A rig whose reader stopped with audio in hand before the run ended, and the time of its next read.
        fn ended() -> (Rig, Duration) {
            let mut rig = Rig::new();
            rig.resync.request();
            let before = rig.play(&on_time(0..6), &reads(landing(3), 5));
            assert!(largest_step(&before) > 0.01, "no audio came");
            rig.chain.reset();
            (rig, landing(3) + QUANTUM_TIME * 5)
        }
        // The control is a reader no recorder links to: it hears what it was playing fall away.
        let (mut rig, at) = ended();
        assert!(!silent(&rig.read(at, SHORT).0), "nothing falls");

        let (mut rig, at) = ended();
        rig.resync.request();
        let (heard, ratio) = rig.read(at, SHORT);
        assert!(
            silent(&heard),
            "the recorder heard audio from before it linked"
        );
        assert!(unsteered(ratio), "{ratio}");
        // The next run puts the reader 10 ms from where it stood, less than a realign moves, so only the link moves it.
        rig.renew_twin();
        let again = at + ms(200);
        let lands: Vec<Duration> = (0..6).map(|packet| again + period() * packet).collect();
        // The reads of the next run start this long after its first packet lands.
        let lead_ms = 2;
        let tape = rig.play(&lands, &reads(again + ms(lead_ms), 8));
        // The run's first sample is due the cover after its packet landed: the cover less the lead after the first read's time, so after that read.
        let first = frames_in(ms(JITTER_COVER_MS - lead_ms)) + QUANTUM;
        assert!(silent(&tape[..first]));
        assert_shaped(&tape[first..first + FADE], run_start(&rig.decoded));
        assert_same(
            &tape[first + FADE..],
            &rig.decoded[FADE..tape.len() - first],
        );
    }
}
