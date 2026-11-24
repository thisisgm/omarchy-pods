//! Support tests of what the chain makes of dictation frames, through its public API, with the test owning the clock; the dictation path has no user path yet.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the suites of this crate share one rig, and this one leaves some of its helpers unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::iter;
    use std::sync::Arc;
    use std::time::Duration;

    use pods_mic::opus::{Decoder, MAX_OPUS_FRAME_BYTES};
    use pods_mic::{
        ARRIVAL_WINDOW, Chain, Counters, FRAME_MS, MAX_FRAMES_PER_NOTIFICATION, MAX_GAP_FRAMES,
        MAX_UNITS_PER_SDU, NODE_LATENCY_MS, RATE_HZ, Reader, Resync, STAMP_STEP, STEADY_FRAMES,
        chain,
    };
    use pods_samples::opus::{TONE_DBFS, TONE_HZ, tone};
    use pods_samples::uplink::tone_unit;

    use super::support::{
        FADE, FIRST, HELD, LEVELS, LINES, PACKET_FRAMES, QUANTUM, Rig, TONE_SLACK_DB,
        TONE_SLACK_HZ, Twin, after, assert_same, assert_shaped, frame_frames, frames_in,
        largest_step, ms, period, reads, run_start, silent, unsteered,
    };

    /// A frame of the tone past any a run here lands, so its bytes are not those a counter before it carried.
    const OTHER_FRAME: usize = 20;

    /// When the frame `step` frames after a run's first lands if it is on time.
    fn at(step: u32) -> Duration {
        after(FIRST, ms(FRAME_MS), step)
    }

    /// The frame of the run that the first sample of a tape holds, when its first read was at `from`, the frames landed on time and nothing jumped since.
    fn base_of(from: Duration) -> usize {
        let before = FIRST.saturating_add(ms(NODE_LATENCY_MS));
        let played = from.saturating_add(ms(FRAME_MS)).checked_sub(before);
        frames_in(played.unwrap()).checked_sub(QUANTUM).unwrap()
    }

    /// What was concealed, refused, reset and run dry.
    fn counted(counters: &Counters) -> [u32; 4] {
        [
            &counters.concealed,
            &counters.decode_errors,
            &counters.decoder_resets,
            &counters.underruns,
        ]
        .map(Rig::count)
    }

    /// A chain on a dictation run fed the frames of the tone, and a twin decoder fed the same.
    struct Stream {
        chain: Chain,
        reader: Reader,
        resync: Resync,
        counters: Arc<Counters>,
        twin: Twin,
        /// The counter of the next frame, the frames of the tone that went by, and the frame times that did.
        counter: u16,
        sent: usize,
        step: u32,
    }

    impl Stream {
        fn new() -> Self {
            Self::from_counter(0)
        }

        fn from_counter(counter: u16) -> Self {
            let counters = Arc::new(Counters::default());
            let (mut chain, reader, resync) = chain(Arc::clone(&counters)).unwrap();
            chain.dictation_run();
            Self {
                chain,
                reader,
                resync,
                counters,
                twin: Twin::new(),
                counter,
                sent: 0,
                step: 0,
            }
        }

        /// One notification at `now` of the next `count` frames of the tone; the twin decodes those under the cap.
        fn notify(&mut self, now: Duration, count: usize) {
            let frames: Vec<&[u8]> = tone().cycle().skip(self.sent).take(count).collect();
            for frame in frames.iter().take(MAX_FRAMES_PER_NOTIFICATION) {
                self.twin.call(|twin| twin.frame(frame));
            }
            let first = self.counter;
            let counters = (0..).map(|frame: u16| first.wrapping_add(frame));
            self.chain.frames(now, counters.zip(frames.iter().copied()));
            self.sent = self.sent.saturating_add(count);
            self.counter = first.wrapping_add(u16::try_from(count).unwrap());
        }

        /// Lands the next `frames` frames of the tone on time, each alone in its notification.
        fn land(&mut self, frames: u32) {
            for _ in 0..frames {
                self.notify(at(self.step), STEADY_FRAMES);
                self.step = self.step.saturating_add(1);
            }
        }

        /// `frames` frames never come: their counters and their times go by, and the twin conceals each.
        fn lose(&mut self, frames: u16) {
            for _ in 0..frames {
                self.twin.call(Decoder::conceal);
            }
            self.counter = self.counter.wrapping_add(frames);
            self.sent = self.sent.saturating_add(usize::from(frames));
            self.step = self.step.saturating_add(u32::from(frames));
        }

        fn read(&mut self, now: Duration, frames: usize) -> (Vec<f32>, f64) {
            let mut heard = vec![0.0; frames];
            let ratio = self.reader.read(now, &mut heard);
            (heard, ratio)
        }

        /// Lands the frames due by each read on time, a landing first where the two meet, and returns what the reads gave, joined.
        fn play(&mut self, reads: &[Duration]) -> Vec<f32> {
            let mut tape = Vec::new();
            for &now in reads {
                while at(self.step) <= now {
                    self.land(1);
                }
                tape.extend(self.read(now, QUANTUM).0);
            }
            tape
        }

        /// The last `frames` frames written, as a recorder that links after the last landing reads them in one read.
        fn heard(&mut self, frames: usize) -> Vec<f32> {
            let samples = frames.saturating_mul(frame_frames());
            assert!(samples <= HELD, "more than one read can be handed");
            self.resync.request();
            let last = at(self.step.checked_sub(1).unwrap());
            self.read(last.saturating_add(ms(NODE_LATENCY_MS)), samples)
                .0
        }

        /// Asserts a recorder that links after the last landing hears the run as the twin gave it, under both fades at its start and then bit for bit; returns what it heard.
        fn assert_heard_as_decoded(&mut self) -> Vec<f32> {
            let heard = self.heard(self.twin.decoded.len().checked_div(frame_frames()).unwrap());
            assert_eq!(heard.len(), self.twin.decoded.len());
            assert_shaped(&heard[..FADE], run_start(&self.twin.decoded));
            assert_same(&heard[FADE..], &self.twin.decoded[FADE..]);
            heard
        }
    }

    /// The largest step between two samples of the tone as a run that lost nothing gives it.
    fn own_step() -> f32 {
        let mut clean = Stream::new();
        clean.land(11);
        largest_step(&clean.assert_heard_as_decoded()[FADE..])
    }

    /// A run that loses `lost` frames in a row after its fourth, and what a recorder hears of all of it.
    fn losing(lost: u16) -> (Stream, Vec<f32>) {
        let mut stream = Stream::new();
        stream.land(4);
        stream.lose(lost);
        stream.land(3);
        let heard = stream.assert_heard_as_decoded();
        (stream, heard)
    }

    #[test]
    fn steady_frames_come_out_as_the_tone_at_the_node_rate() {
        let mut stream = Stream::new();
        stream.resync.request();
        let from = at(5);
        let tape = stream.play(&reads(from, 30));
        let base = base_of(from);
        assert_same(
            &tape[FADE..],
            &stream.twin.decoded[base + FADE..base + tape.len()],
        );
        assert_eq!(counted(&stream.counters), [0; 4]);

        // Half a second at the node's rate, in which a tone rises through zero half as often as its frequency.
        let half = usize::try_from(RATE_HZ / 2).unwrap();
        let steady = &tape[QUANTUM..QUANTUM + half];
        let rises = steady
            .windows(2)
            .filter(|pair| matches!(pair, [before, after] if *before < 0.0 && *after >= 0.0));
        let hz = 2 * u32::try_from(rises.count()).unwrap();
        assert!(
            hz.abs_diff(TONE_HZ) <= TONE_SLACK_HZ,
            "the tone is at {hz} Hz"
        );
        let peak = steady
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        let level = 20.0 * f64::from(peak).log10();
        assert!(
            (level - f64::from(TONE_DBFS)).abs() <= TONE_SLACK_DB,
            "the tone peaks at {level} dBFS"
        );
    }

    #[test]
    fn one_lost_frame_is_concealed_and_counted() {
        let (stream, heard) = losing(1);
        assert_eq!(counted(&stream.counters), [1, 0, 0, 0]);
        // What stands for the frame is the decoder's own concealment: audio, with no step where it joins the frames around it.
        let frame = frame_frames();
        let own = own_step();
        let lost = &heard[4 * frame..5 * frame];
        assert!(largest_step(lost) > own / 2.0, "a block of zeros");
        let from_the_loss = largest_step(&heard[4 * frame - 1..]);
        assert!(from_the_loss <= own, "a step of {from_the_loss} for {own}");
    }

    #[test]
    fn lost_frames_in_a_row_are_concealed_with_no_step() {
        let own = own_step();
        for lost in 2..=MAX_GAP_FRAMES {
            let (stream, heard) = losing(lost);
            assert_eq!(
                counted(&stream.counters),
                [u32::from(lost), 0, 0, 0],
                "{lost} lost"
            );
            let from_the_loss = largest_step(&heard[4 * frame_frames() - 1..]);
            assert!(
                from_the_loss <= own,
                "{lost} lost: a step of {from_the_loss} for {own}"
            );
        }
    }

    #[test]
    fn five_lost_frames_start_a_new_run() {
        // One more than the decoder may conceal, and the last distance that is not a counter going back; the counters alone jump, with no time gone by.
        for hole in [MAX_GAP_FRAMES + 1, u16::MAX / 2] {
            let mut stream = Stream::new();
            stream.land(4);
            stream.counter = stream.counter.wrapping_add(hole);
            stream.twin = Twin::new();
            stream.land(2);
            assert_eq!(counted(&stream.counters), [0; 4], "a hole of {hole}");
            // Nothing was written for the hole, so the six frames before the ring's end are four of the run before, which a recorder of this run is handed as silence, and its own two.
            let heard = stream.heard(6);
            let (before, run) = heard.split_at(4 * frame_frames());
            assert!(silent(before), "a hole of {hole}");
            assert_shaped(&run[..FADE], run_start(&stream.twin.decoded));
            assert_same(&run[FADE..], &stream.twin.decoded[FADE..]);
        }
    }

    #[test]
    fn a_counter_going_back_writes_nothing() {
        let mut stream = Stream::new();
        stream.land(6);
        // Other bytes under counters the run has passed: the frame before, three back, and half the range back.
        let other = tone().nth(OTHER_FRAME).unwrap();
        for back in [1, 3, 1 << 15] {
            let counter = stream.counter.wrapping_sub(back);
            stream
                .chain
                .frames(at(stream.step), iter::once((counter, other)));
        }
        stream.land(3);
        assert_eq!(counted(&stream.counters), [0; 4]);
        assert_eq!(Rig::count(&stream.counters.dictation_repeats), 3);
        // The decoder met none of them: the frames after are what a twin that saw none decoded.
        stream.assert_heard_as_decoded();
    }

    #[test]
    fn a_run_that_has_not_told_its_time_is_not_played() {
        const BURST: usize = 5;
        let mut stream = Stream::new();
        stream.resync.request();
        assert!(!silent(&stream.play(&reads(at(5), 6))));
        // Five frames in one notification under counters past the gap: a new run, and no steady notification to say when it plays.
        stream.counter = stream.counter.wrapping_add(MAX_GAP_FRAMES + 1);
        stream.twin = Twin::new();
        stream.notify(at(11), BURST);
        // The first read holds the fall of the run that ended; the reader then passes the place of the new run's frames and hands out none of them.
        stream.read(at(11), QUANTUM);
        for step in 12..21 {
            let (heard, ratio) = stream.read(at(step), QUANTUM);
            assert!(
                silent(&heard) && unsteered(ratio),
                "the read at step {step}"
            );
        }
        // Its next steady notification tells the time, and the read after it is handed the run.
        stream.step = 21;
        stream.land(1);
        assert!(!silent(&stream.read(at(21), QUANTUM).0));
        // The burst waited whole: a recorder that links now hears its five frames and the steady one as a fresh decoder gave them.
        stream.assert_heard_as_decoded();
        assert_eq!(counted(&stream.counters), [0; 4]);
    }

    #[test]
    fn a_flood_of_repeats_does_not_move_the_target() {
        let mut stream = Stream::new();
        stream.resync.request();
        let from = at(5);
        let mut tape = stream.play(&reads(from, 6));
        // A window of repeats of the last frame, each alone in its notification, just before the next frame and the next read are due.
        let late = at(10) + ms(FRAME_MS - 1);
        let repeat = (stream.counter.wrapping_sub(1), tone().next().unwrap());
        for _ in 0..ARRIVAL_WINDOW {
            stream.chain.frames(late, iter::once(repeat));
        }
        // A target the repeats had moved would have steered the reader, or sent it back over what it already played.
        let (heard, ratio) = stream.read(at(11), QUANTUM);
        assert!(unsteered(ratio), "{ratio}");
        tape.extend(heard);
        let base = base_of(from);
        assert_same(
            &tape[FADE..],
            &stream.twin.decoded[base + FADE..base + tape.len()],
        );
        assert_eq!(counted(&stream.counters), [0; 4]);
    }

    #[test]
    fn a_counter_wrap_has_no_gap() {
        let mut stream = Stream::from_counter(u16::MAX - 1);
        stream.land(5);
        assert_eq!(stream.counter, 3);
        assert_eq!(counted(&stream.counters), [0; 4]);
        stream.assert_heard_as_decoded();
    }

    #[test]
    fn a_frame_over_94_bytes_is_concealed_and_counted() {
        let mut stream = Stream::new();
        stream.land(4);
        // A frame one byte past the cap under the stream's own first byte and one of no bytes, each on time under the next counter: the decoder meets neither and conceals both.
        let mut long = [0x55; MAX_OPUS_FRAME_BYTES + 1];
        long[0] = tone().next().unwrap()[0];
        for refused in [long.as_slice(), &[]] {
            let frame = (stream.counter, refused);
            stream.chain.frames(at(stream.step), iter::once(frame));
            stream.lose(1);
            stream.land(2);
        }
        assert_eq!(counted(&stream.counters), [2, 2, 0, 0]);
        stream.assert_heard_as_decoded();
    }

    #[test]
    fn nine_frames_in_one_notification_write_eight_and_count_one() {
        let mut stream = Stream::new();
        // One steady frame first, so the nine are of a run under way and not of its opening, which is staged.
        stream.land(1);
        stream.notify(at(1), MAX_FRAMES_PER_NOTIFICATION + 1);
        assert_eq!(counted(&stream.counters), [0, 1, 0, 0]);
        // The ninth never reached the decoder, so the frame after it finds one lost.
        stream.twin.call(Decoder::conceal);
        stream.step = u32::try_from(MAX_FRAMES_PER_NOTIFICATION + 2).unwrap();
        stream.land(1);
        assert_eq!(counted(&stream.counters), [1, 1, 0, 0]);
        stream.assert_heard_as_decoded();
    }

    #[test]
    fn a_burst_does_not_move_the_target() {
        const BURST: usize = 5;
        let mut stream = Stream::new();
        stream.resync.request();
        let from = at(5);
        let mut tape = stream.play(&reads(from, 6));
        // Five frames in one notification leave the ring 80 ms further on than the cadence of the steady ones has it.
        stream.notify(at(10) + ms(FRAME_MS / 2), BURST);
        for now in [at(11), at(12)] {
            let (heard, ratio) = stream.read(now, QUANTUM);
            assert!(unsteered(ratio), "{ratio}");
            tape.extend(heard);
        }
        let base = base_of(from);
        assert_same(
            &tape[FADE..],
            &stream.twin.decoded[base + FADE..base + tape.len()],
        );
        assert_eq!(counted(&stream.counters), [0; 4]);
    }

    #[test]
    fn uplink_units_on_a_dictation_run_write_nothing() {
        let mut stream = Stream::new();
        stream.land(3);
        // A packet of the uplink that was still on its way, longer than an SDU may be: none of it is written and none of it counted.
        let unit = tone_unit(LINES[0], LEVELS[0]);
        let stamps = (0..).map(|unit: u32| unit * STAMP_STEP);
        let stale = stamps.zip(iter::repeat_n(unit.as_slice(), MAX_UNITS_PER_SDU + 1));
        stream.chain.sdu(at(stream.step), stale);
        stream.land(3);
        assert_eq!(counted(&stream.counters), [0; 4]);
        stream.assert_heard_as_decoded();
    }

    #[test]
    fn dictation_frames_on_an_uplink_run_write_nothing() {
        let mut rig = Rig::new();
        rig.land(FIRST);
        // A notification of the dictation stream that was still on its way, longer than one may be.
        let stale = (0_u16..).zip(tone().take(MAX_FRAMES_PER_NOTIFICATION + 1));
        rig.chain.frames(FIRST, stale);
        rig.land(FIRST + period());
        assert_eq!(counted(&rig.counters), [0; 4]);
        rig.resync.request();
        let (heard, _) = rig.read(FIRST + period() + ms(NODE_LATENCY_MS), 2 * PACKET_FRAMES);
        assert_shaped(&heard[..FADE], run_start(&rig.decoded));
        assert_same(&heard[FADE..], &rig.decoded[FADE..]);
    }
}
