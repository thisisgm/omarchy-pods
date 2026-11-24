//! Support tests of how a dictation opens, through the chain's public API with the test owning the clock: the burst placed against the recorder's edge, and the reader that starts behind and catches up; the dictation path has no user path yet.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the suites of this crate share one rig, and this one leaves some of its helpers unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::{self, Write};
    use std::iter;
    use std::ops::Range;
    use std::sync::Arc;
    use std::time::Duration;

    use pods_mic::opus::{DICTATION_DECODE_HZ, Decoder, FRAME_SAMPLES};
    use pods_mic::resample::{RESAMPLE_TAPS, Resampler};
    use pods_mic::{
        Chain, Counters, FRAME_MS, NODE_LATENCY_MS, OPENING_MAX_PPM, PREROLL_STAGE_FRAMES, RATE_HZ,
        Reader, Resync, chain,
    };
    use pods_samples::opus::tone;

    use super::support::{
        FADE, MAX_STEER, QUANTUM, QUANTUM_TIME, Rig, Twin, after, assert_live, assert_same,
        assert_shaped, bits, frame_frames, frames_in, landing, ms, on_time, rise, run_start,
        silent, unsteered,
    };

    /// The frame of the script the first steady notification holds, and when it lands; the frames before it are the burst.
    const STEADY_FRAME: usize = 40;
    const STEADY: Duration = Duration::from_secs(1);
    /// The burst as it was measured: five frames a notification, far faster than they play.
    const BURST_FRAMES: usize = 5;
    const BURST_EVERY: Duration = Duration::from_millis(5);
    /// The frame of the tone whose bytes no other frame of it carries.
    const MARK_BYTES: usize = 3;
    /// How far from its target a reader may stand before it realigns, which the crate keeps to itself; the catch-up fails its bounds when the two part.
    const REALIGN_MS: u32 = 15;
    const PER_MILLION: f64 = 1e6;
    /// What a consumer that runs `OPENING_MAX_PPM` fast needs to close one ms of distance, in ms.
    const MS_PER_MS_CLOSED: f64 = PER_MILLION / OPENING_MAX_PPM;
    const MS_PER_S: f64 = 1000.0;
    /// A recorder that links this long before the first steady notification opens 195 ms behind its target.
    const LEAD_MS: u32 = 200;

    /// A notification: when it lands, and the frames of the script it holds.
    type Landing = (Duration, Vec<usize>);

    /// The bytes of a frame of the script: the tone's first frame, which most of its frames repeat, and at `mark` the one none repeats.
    fn bytes(frame: usize, mark: Option<usize>) -> &'static [u8] {
        let of_the_tone = if mark == Some(frame) { MARK_BYTES } else { 0 };
        tone().nth(of_the_tone).unwrap()
    }

    /// Frames `frames` of the script as a burst, `per` to a notification, the last notification one step before `end`.
    fn burst(frames: Range<usize>, per: usize, end: Duration) -> Vec<Landing> {
        let frames: Vec<usize> = frames.collect();
        let count = u32::try_from(frames.chunks(per).len()).unwrap();
        let lands = (0..count).map(|sent| {
            let steps = BURST_EVERY.saturating_mul(count.saturating_sub(sent));
            end.checked_sub(steps).unwrap()
        });
        lands
            .zip(frames.chunks(per).map(<[usize]>::to_vec))
            .collect()
    }

    /// `count` frames of the steady cadence from the first one, each alone in its notification and on time.
    fn cadence(count: usize) -> Vec<Landing> {
        let lands = (0..).map(|step| after(STEADY, ms(FRAME_MS), step));
        let frames = (STEADY_FRAME..).take(count).map(|frame| vec![frame]);
        lands.zip(frames).collect()
    }

    /// A whole dictation: the burst, then `steady` frames of the cadence.
    fn dictation(steady: usize) -> Vec<Landing> {
        let mut lands = burst(0..STEADY_FRAME, BURST_FRAMES, STEADY);
        lands.extend(cadence(steady));
        lands
    }

    /// What a fresh decoder makes of frames `frames` of the script at the node's rate, with its own concealment for those of `lost`.
    fn twin(frames: Range<usize>, mark: Option<usize>, lost: &[usize]) -> Vec<f32> {
        let mut twin = Twin::new();
        for frame in frames {
            if lost.contains(&frame) {
                twin.call(Decoder::conceal);
            } else {
                twin.call(|decoder| decoder.frame(bytes(frame, mark)));
            }
        }
        twin.decoded
    }

    /// The first frame of the script an opening keeps when frame `last` is placed `lead_ms` after the recorder linked: the one that ends inside the stated latency before the edge.
    fn first_kept(last: usize, lead_ms: u32) -> usize {
        let before_last = lead_ms.saturating_add(NODE_LATENCY_MS - 1) / FRAME_MS;
        last.checked_sub(usize::try_from(before_last).unwrap())
            .unwrap()
    }

    /// When a recorder links that is `lead_ms` ahead of a placing at `placed`.
    fn linked(placed: Duration, lead_ms: u32) -> Duration {
        placed.checked_sub(ms(lead_ms)).unwrap()
    }

    /// The frames a recorder is handed before the read that meets a placing `lead_ms` after it linked.
    fn head(lead_ms: u32) -> usize {
        let reads = ms(lead_ms).as_nanos().div_ceil(QUANTUM_TIME.as_nanos());
        usize::try_from(reads).unwrap().saturating_mul(QUANTUM)
    }

    /// When the first sample of a run that opens on frame `kept` of the script landed, as the placing has it.
    fn run_start_of(kept: usize) -> Duration {
        let frames = (STEADY_FRAME + 1).checked_sub(kept).unwrap();
        let frames = u32::try_from(frames).unwrap();
        linked(STEADY, frames.saturating_mul(FRAME_MS))
    }

    /// Whether the consumer is asked for no more than the steady steering may ask.
    fn steady(ratio: f64) -> bool {
        (1.0 - MAX_STEER..=1.0 + MAX_STEER).contains(&ratio)
    }

    /// Every place `stretch` stands in `tape`, bit for bit.
    fn places(tape: &[f32], stretch: &[f32]) -> Vec<usize> {
        let (tape, stretch) = (bits(tape), bits(stretch));
        let windows = tape.windows(stretch.len()).enumerate();
        windows
            .filter(|(_, window)| *window == stretch)
            .map(|(at, _)| at)
            .collect()
    }

    /// Asserts the tape is silence for its first `head` frames and from there the twin's audio from its first sample: under both fades at the start, then bit for bit to the tape's end.
    fn assert_opens(tape: &[f32], head: usize, twin: &[f32]) {
        let (silence, audio) = tape.split_at(head);
        assert!(silent(silence), "audio before the opening");
        assert!(audio.len() > FADE && audio.len() <= twin.len());
        assert_shaped(&audio[..FADE], run_start(twin));
        assert_same(&audio[FADE..], &twin[FADE..audio.len()]);
    }

    /// Asserts the tape fades in from nothing on the audio due at `now`, its first read, as a reader moved to its target has it, and goes on from there bit for bit; the twin's first sample landed at `start`.
    fn assert_at_target(tape: &[f32], now: Duration, start: Duration, twin: &[f32]) {
        let played = now.checked_sub(start.saturating_add(ms(NODE_LATENCY_MS)));
        let due = frames_in(played.unwrap());
        let live = &twin[due.checked_sub(QUANTUM).unwrap()..][..tape.len()];
        let risen = live.iter().zip(0..).map(|(&sample, at)| sample * rise(at));
        assert_shaped(&tape[..FADE], risen);
        assert_same(&tape[FADE..], &live[FADE..]);
    }

    /// How late the resampler hands out what it is given, in ms: where the peak of one sample comes out.
    fn resampler_delay_ms() -> f64 {
        let mut resampler = Resampler::new(DICTATION_DECODE_HZ, RATE_HZ).unwrap();
        let mut pulse = vec![0.0; FRAME_SAMPLES];
        pulse[0] = 1.0;
        let mut out = Vec::new();
        resampler.push(&pulse, &mut out);
        let (at, _) = (out.iter().enumerate())
            .max_by(|one, other| one.1.total_cmp(other.1))
            .unwrap();
        f64::from(u32::try_from(at).unwrap()) * MS_PER_S / f64::from(RATE_HZ)
    }

    /// Half the length of the resampler's filter, in ms: its centre, which the peak of one sample comes out no later than.
    fn half_filter_ms() -> f64 {
        let half = u32::try_from(RESAMPLE_TAPS / 2).unwrap();
        f64::from(half) * MS_PER_S / f64::from(DICTATION_DECODE_HZ)
    }

    /// A chain on a dictation run, a recorder that reads it, and what the recorder was handed.
    struct Take {
        chain: Chain,
        reader: Reader,
        resync: Resync,
        counters: Arc<Counters>,
        /// The counter the first frame of the script carries, and the frame of the script that carries the mark.
        first: u16,
        mark: Option<usize>,
        /// The notifications still to land, and when the next read comes.
        lands: VecDeque<Landing>,
        now: Duration,
        /// What the reads gave, joined; when each came and the ratio it asked for; and the allocations made inside them.
        tape: Vec<f32>,
        times: Vec<Duration>,
        ratios: Vec<f64>,
        allocations: u64,
    }

    impl Take {
        fn new(first: u16, mark: Option<usize>) -> Self {
            let counters = Arc::new(Counters::default());
            let (mut chain, reader, resync) = chain(Arc::clone(&counters)).unwrap();
            chain.dictation_run();
            Self {
                chain,
                reader,
                resync,
                counters,
                first,
                mark,
                lands: VecDeque::new(),
                now: Duration::ZERO,
                tape: Vec::new(),
                times: Vec::new(),
                ratios: Vec::new(),
                allocations: 0,
            }
        }

        fn notify(&mut self, now: Duration, frames: &[usize]) {
            let (first, mark) = (self.first, self.mark);
            let counters = frames
                .iter()
                .map(|&frame| first.wrapping_add(u16::try_from(frame).unwrap()));
            let data = frames.iter().map(|&frame| bytes(frame, mark));
            self.chain.frames(now, counters.zip(data));
        }

        /// A recorder links at `from` and reads `reads` times while `lands` land, as `go_on` has it.
        fn play(&mut self, lands: Vec<Landing>, from: Duration, reads: usize) {
            self.lands = lands.into();
            self.resync.request();
            self.now = from;
            self.go_on(reads);
        }

        /// Plays the notifications and `reads` more reads in order of time, a notification first where the two meet; each read comes when a consumer that follows the ratio the last one asked for comes back.
        fn go_on(&mut self, reads: usize) {
            for _ in 0..reads {
                let now = self.now;
                while self.lands.front().is_some_and(|(at, _)| *at <= now) {
                    let (at, frames) = self.lands.pop_front().unwrap();
                    self.notify(at, &frames);
                }
                let mut heard = [0.0; QUANTUM];
                let mut ratio = 1.0;
                let reading =
                    allocation_counter::measure(|| ratio = self.reader.read(now, &mut heard));
                self.allocations = self.allocations.saturating_add(reading.count_total);
                self.tape.extend(heard);
                self.times.push(now);
                self.ratios.push(ratio);
                self.now = now.saturating_add(QUANTUM_TIME.div_f64(ratio));
            }
        }

        /// The stream starts again `later`: a new run, and a whole dictation of `steady` frames after its burst.
        fn start_again(&mut self, later: Duration, steady: usize) {
            self.chain.dictation_run();
            let lands = dictation(steady).into_iter();
            let lands = lands.map(|(at, frames)| (at.saturating_add(later), frames));
            self.lands = lands.collect();
        }

        /// The last opening's distance at its start and the time it took, in ms.
        fn opening(&self) -> [u32; 2] {
            [&self.counters.opening_behind_ms, &self.counters.opening_ms].map(Rig::count)
        }
    }

    /// A recorder that links `lead_ms` before the first steady notification of a dictation whose counters start at `first`, and reads `reads` times; asserts it opens on the frame its edge keeps and is handed every frame from there once and in place.
    fn opened(first: u16, lead_ms: u32, reads: usize) -> Take {
        let mut take = Take::new(first, None);
        take.play(dictation(reads), linked(STEADY, lead_ms), reads);
        let kept = first_kept(STEADY_FRAME, lead_ms);
        let twin = twin(kept..STEADY_FRAME.saturating_add(reads), None, &[]);
        assert_opens(&take.tape, head(lead_ms), &twin);
        assert_eq!(Rig::count(&take.counters.underruns), 0);
        take
    }

    #[test]
    fn the_mark_before_the_edge_is_kept_once() {
        const READS: usize = 30;
        const BEFORE_EDGE_MS: u32 = 60;
        // The frame that starts 60 ms before the edge is placed 240 ms before the steady one: twelve frames back.
        let back = usize::try_from((LEAD_MS + BEFORE_EDGE_MS) / FRAME_MS).unwrap();
        let mark = STEADY_FRAME + 1 - back;
        let mut take = Take::new(0, Some(mark));
        take.play(dictation(READS), linked(STEADY, LEAD_MS), READS);
        let kept = first_kept(STEADY_FRAME, LEAD_MS);
        let twin = twin(kept..STEADY_FRAME + READS, Some(mark), &[]);
        let head = head(LEAD_MS);
        assert_opens(&take.tape, head, &twin);

        let frame = frame_frames();
        let marked = &twin[(mark - kept) * frame..][..frame];
        let found = places(&take.tape, marked);
        // Once, and where its counter puts it: that many frames after the first one kept.
        assert_eq!(found, [head + (mark - kept) * frame]);
        // The recording's audio starts the stated latency before the edge, so the mark comes that much less its 60 ms into it, within one frame.
        let due = head + frames_in(ms(NODE_LATENCY_MS - BEFORE_EDGE_MS));
        assert!(found[0].abs_diff(due) <= frame, "{} for {due}", found[0]);
    }

    #[test]
    fn a_mark_older_than_the_edge_less_the_latency_is_dropped() {
        const READS: usize = 30;
        // The newest frame the edge does not keep: it ends 100 ms before the edge, and the latency reaches 85.
        let kept = first_kept(STEADY_FRAME, LEAD_MS);
        let mark = kept - 1;
        let mut take = Take::new(0, Some(mark));
        take.play(dictation(READS), linked(STEADY, LEAD_MS), READS);
        // The decoder never met the mark: the recording is what a fresh one makes of the frames after it.
        let after_it = twin(kept..STEADY_FRAME + READS, Some(mark), &[]);
        assert_opens(&take.tape, head(LEAD_MS), &after_it);
        let with_it = twin(mark..kept, Some(mark), &[]);
        assert_eq!(places(&take.tape, &with_it[FADE..]), []);
    }

    #[test]
    fn the_first_sample_out_is_from_the_edge_less_the_latency_within_one_frame() {
        const READS: usize = 14;
        let delay_ms = resampler_delay_ms();
        assert!(
            delay_ms > 0.0 && delay_ms <= half_filter_ms(),
            "{delay_ms} ms"
        );
        let (mut least, mut most) = (u32::MAX, 0);
        // A recorder linking at every ms of one frame's length, so the cut falls at every place inside a frame.
        for lead_ms in LEAD_MS..LEAD_MS + FRAME_MS {
            opened(0, lead_ms, READS);
            // The first kept frame starts one frame before the place of its end, which is 20 ms a frame before the steady one's.
            let kept = first_kept(STEADY_FRAME, lead_ms);
            let frames_back = u32::try_from(STEADY_FRAME + 1 - kept).unwrap();
            let before_edge_ms = frames_back * FRAME_MS - lead_ms;
            let within = NODE_LATENCY_MS..NODE_LATENCY_MS + FRAME_MS;
            assert!(within.contains(&before_edge_ms), "{before_edge_ms} ms");
            (least, most) = (least.min(before_edge_ms), most.max(before_edge_ms));
        }
        // The resampler hands its audio out that much later, so what stands at the frame's first sample is that much older.
        let (least, most) = (f64::from(least) + delay_ms, f64::from(most) + delay_ms);
        let said = format!("{least:.2} to {most:.2} ms before the edge");
        let of_it = format!("{delay_ms:.2} ms of it the resampler's delay");
        writeln!(
            io::stderr(),
            "a recording's first sample is audio from {said}, {of_it}"
        )
        .unwrap();
    }

    #[test]
    fn a_repeated_counter_in_the_burst_plays_once() {
        const READS: usize = 30;
        let mut lands = dictation(READS);
        // The notification that straddles the cut comes twice, and so does one frame of it alone.
        let kept = first_kept(STEADY_FRAME, LEAD_MS);
        let straddling = kept / BURST_FRAMES;
        let (at, again) = lands[straddling].clone();
        lands.insert(straddling + 1, (at, vec![again[0]]));
        lands.insert(straddling + 1, (at, again));
        let mut take = Take::new(0, None);
        take.play(lands, linked(STEADY, LEAD_MS), READS);
        let twin = twin(kept..STEADY_FRAME + READS, None, &[]);
        assert_opens(&take.tape, head(LEAD_MS), &twin);
        let repeats = u32::try_from(BURST_FRAMES + 1).unwrap();
        assert_eq!(Rig::count(&take.counters.dictation_repeats), repeats);
        assert_eq!(Rig::count(&take.counters.concealed), 0);
    }

    #[test]
    fn a_counter_wrap_inside_the_burst_loses_nothing() {
        // The counter passes its last value inside the frames the edge keeps.
        let kept = u16::try_from(first_kept(STEADY_FRAME, LEAD_MS)).unwrap();
        let take = opened(u16::MAX - kept - 4, LEAD_MS, 30);
        assert_eq!(Rig::count(&take.counters.dictation_repeats), 0);
        assert_eq!(Rig::count(&take.counters.concealed), 0);
    }

    #[test]
    fn a_burst_cut_by_loss_is_concealed_not_shifted() {
        const READS: usize = 30;
        // Two frames the edge would have kept never come.
        let kept = first_kept(STEADY_FRAME, LEAD_MS);
        let lost = [kept + 4, kept + 5];
        let mut lands = dictation(READS);
        for (_, frames) in &mut lands {
            frames.retain(|frame| !lost.contains(frame));
        }
        let mut take = Take::new(0, None);
        take.play(lands, linked(STEADY, LEAD_MS), READS);
        // The frames after the loss stand where their counters put them: the tape is the twin's, which concealed the two in place.
        let twin = twin(kept..STEADY_FRAME + READS, None, &lost);
        assert_opens(&take.tape, head(LEAD_MS), &twin);
        assert_eq!(Rig::count(&take.counters.concealed), 2);
    }

    #[test]
    fn a_hole_in_the_burst_past_the_gap_opens_on_what_follows_it() {
        const READS: usize = 30;
        // Five frames the edge would have kept never come, one more than a loss may be: what stands before them is of no run.
        let kept = first_kept(STEADY_FRAME, LEAD_MS);
        let hole = kept + 2..kept + 7;
        let mut lands = dictation(READS);
        for (_, frames) in &mut lands {
            frames.retain(|frame| !hole.contains(frame));
        }
        let mut take = Take::new(0, None);
        take.play(lands, linked(STEADY, LEAD_MS), READS);
        let twin = twin(hole.end..STEADY_FRAME + READS, None, &[]);
        assert_opens(&take.tape, head(LEAD_MS), &twin);
        assert_eq!(Rig::count(&take.counters.concealed), 0);
    }

    #[test]
    fn a_start_again_inside_a_recording_gets_no_opening() {
        const AGAIN: Duration = Duration::from_secs(7);
        const LAST: usize = 100;
        // The stream starts again under a recorder still catching up, and under one that has caught up.
        for (reads, more) in [(50, 340), (300, 100)] {
            let mut take = opened(0, LEAD_MS, reads);
            let first = take.opening();
            assert_eq!(first == [0, 0], reads == 50);
            take.start_again(AGAIN, LAST);
            take.go_on(more);
            // Its edge is long past, so the run holds the whole burst, and the recorder is moved to its target as any reader off it is.
            let placed = STEADY + AGAIN;
            let from = take.times.iter().position(|&at| at >= placed).unwrap();
            let twin = twin(0..STEADY_FRAME + LAST, None, &[]);
            let start = run_start_of(0) + AGAIN;
            assert_at_target(&take.tape[from * QUANTUM..], take.times[from], start, &twin);
            assert!(take.ratios[reads..].iter().all(|&ratio| steady(ratio)));
            assert_eq!(take.opening(), first);
        }
    }

    #[test]
    fn a_recorder_that_links_on_a_run_long_over_opens_on_the_next() {
        const READS: usize = 30;
        const AGAIN: Duration = Duration::from_secs(7);
        let mut take = opened(0, LEAD_MS, READS);
        // A second recorder links seven seconds on, and its first reads meet the time the run before told.
        take.resync.request();
        take.now = linked(STEADY, LEAD_MS) + AGAIN;
        take.go_on(2);
        take.start_again(AGAIN, READS);
        take.go_on(READS - 2);
        let kept = first_kept(STEADY_FRAME, LEAD_MS);
        let twin = twin(kept..STEADY_FRAME + READS, None, &[]);
        assert_opens(&take.tape[READS * QUANTUM..], head(LEAD_MS), &twin);
    }

    #[test]
    fn a_recorder_linking_after_the_burst_gets_no_opening() {
        const READS: usize = 30;
        const LATE_MS: u32 = 100;
        let mut take = Take::new(0, None);
        let from = STEADY + ms(LATE_MS);
        take.play(dictation(READS + 10), from, READS);
        // No recorder waited at the placing, so the run holds the stated latency before it and no more.
        let kept = first_kept(STEADY_FRAME, 0);
        let twin = twin(kept..STEADY_FRAME + READS + 10, None, &[]);
        // As on the uplink, the first read fades in from nothing on the audio due then.
        assert_at_target(&take.tape, from, run_start_of(kept), &twin);
        assert!(take.ratios.iter().all(|&ratio| steady(ratio)));
        assert_eq!(take.opening(), [0, 0]);
    }

    #[test]
    fn a_recorder_linking_inside_an_opening_starts_at_its_target() {
        const READS: usize = 50;
        const MORE: usize = 30;
        let mut take = Take::new(0, None);
        take.play(dictation(READS + MORE), linked(STEADY, LEAD_MS), READS);
        // A second recorder takes the reader while it is still far behind: the run was not opened against its edge.
        take.resync.request();
        take.go_on(MORE);
        let kept = first_kept(STEADY_FRAME, LEAD_MS);
        let twin = twin(kept..STEADY_FRAME + READS + MORE, None, &[]);
        let (first, second) = take.tape.split_at(READS * QUANTUM);
        assert_opens(first, head(LEAD_MS), &twin);
        assert_at_target(second, take.times[READS], run_start_of(kept), &twin);
        assert!(take.ratios[READS..].iter().all(|&ratio| steady(ratio)));
        assert_eq!(take.opening(), [0, 0]);
        assert_eq!(Rig::count(&take.counters.underruns), 0);
    }

    #[test]
    fn a_stream_with_no_burst_starts_at_its_target() {
        const READS: usize = 30;
        const EARLY_MS: u32 = 50;
        let mut take = Take::new(0, None);
        take.play(cadence(READS), linked(STEADY, EARLY_MS), READS);
        // The one frame the edge could keep landed after it: the recorder waits out the stated latency and hears the run from its first sample.
        let twin = twin(STEADY_FRAME..STEADY_FRAME + READS, None, &[]);
        assert_opens(&take.tape, frames_in(ms(EARLY_MS + NODE_LATENCY_MS)), &twin);
        assert!(take.ratios.iter().all(|&ratio| steady(ratio)));
        assert_eq!(take.opening(), [0, 0]);
        assert_eq!(Rig::count(&take.counters.underruns), 0);
    }

    #[test]
    fn an_uplink_run_after_a_placed_opening_does_not_open() {
        let mut rig = Rig::new();
        // A recorder links and a dictation is placed against its edge, but no read comes before a call takes the uplink.
        rig.resync.request();
        assert!(silent(&rig.read(ms(10), QUANTUM).0));
        rig.chain.dictation_run();
        rig.chain.frames(ms(60), iter::once((0, bytes(0, None))));
        rig.chain.reset();
        for at in on_time(0..20) {
            rig.land(at);
        }
        // The read is handed the audio due then, not the run from its start, and the consumer is not asked to hurry.
        let now = landing(19);
        assert_live(&mut rig, now);
        let (_, ratio) = rig.read(now + QUANTUM_TIME, QUANTUM);
        assert!(steady(ratio), "{ratio}");
    }

    #[test]
    fn no_realign_happens_inside_an_opening() {
        // Five seconds: the reader is behind by far more than a realign allows for most of them, and a jump to the target would leave out what the tape is asserted to hold.
        let take = opened(0, LEAD_MS, 260);
        assert_eq!(take.opening()[0], 195);
        assert_eq!(take.allocations, 0);
    }

    #[test]
    fn the_ratio_never_passes_the_opening_clamp_and_returns_to_the_steady_clamp() {
        let take = opened(0, LEAD_MS, 400);
        let clamp = 1.0 + OPENING_MAX_PPM / PER_MILLION;
        let head = head(LEAD_MS) / QUANTUM;
        let hurried = take.ratios[head..]
            .iter()
            .take_while(|&&ratio| !steady(ratio));
        let end = head + hurried.count();
        // Its own rate while nothing has come, the clamp itself from the read that opens to the one that reaches the target, and the steady clamp from there on.
        assert!(take.ratios[..head].iter().all(|&ratio| unsteered(ratio)));
        assert!(end > head && end < take.ratios.len(), "{head} to {end}");
        for ratio in &take.ratios[head..end] {
            assert_eq!(ratio.to_bits(), clamp.to_bits(), "{ratio}");
        }
        assert!(take.ratios[end..].iter().all(|&ratio| steady(ratio)));
    }

    #[test]
    fn the_opening_catches_up_200_ms_in_at_most_4_5_s() {
        const CATCH_UP_LIMIT_MS: u32 = 4500;
        /// The read that finds the target reached, and the one before it.
        const SLACK_READS: u32 = 2;
        let mut ranges = [[u32::MAX, 0]; 2];
        // The leads at which the reader opens 145, 200 and 265 ms behind.
        for lead_ms in [150, 215, 270] {
            let take = opened(0, lead_ms, 400);
            let [behind_ms, took_ms] = take.opening();
            // Not before the arithmetic of the clamp, and not after it by more than the slack's reads, each no longer than a quantum.
            let least = ms(behind_ms - REALIGN_MS).mul_f64(MS_PER_MS_CLOSED);
            let most = least + QUANTUM_TIME * SLACK_READS;
            assert!(ms(took_ms) >= least, "{took_ms} ms for {behind_ms}");
            assert!(ms(took_ms) <= most, "{took_ms} ms for {behind_ms}");
            if lead_ms == 215 {
                assert_eq!(behind_ms, 200);
                assert!(took_ms <= CATCH_UP_LIMIT_MS, "{took_ms} ms");
            }
            for (range, value) in ranges.iter_mut().zip([behind_ms, took_ms]) {
                *range = [range[0].min(value), range[1].max(value)];
            }
        }
        let [[near, far], [soon, late]] = ranges;
        let caught = format!("caught up in {soon} to {late} ms");
        writeln!(io::stderr(), "openings {near} to {far} ms behind {caught}").unwrap();
    }

    #[test]
    fn stage_full_without_a_steady_notification_places_by_arrival() {
        // Three frames a notification: the stage fills on the second frame of the last one, and its third is written behind it.
        const PER_NOTIFICATION: usize = 3;
        let script = 0..PREROLL_STAGE_FRAMES + 1;
        let lands = burst(script.clone(), PER_NOTIFICATION, STEADY);
        let full = lands.last().unwrap().0;
        // The last staged frame is placed at its arrival, so the edge keeps what it would of a steady one.
        let kept = first_kept(PREROLL_STAGE_FRAMES - 1, LEAD_MS);
        let head = head(LEAD_MS);
        let mut take = Take::new(0, None);
        take.play(
            lands,
            linked(full, LEAD_MS),
            head / QUANTUM + script.end - kept,
        );
        let twin = twin(kept..script.end, None, &[]);
        assert_eq!(take.tape.len(), head + twin.len());
        assert_opens(&take.tape, head, &twin);
        assert_eq!(Rig::count(&take.counters.underruns), 0);
    }

    #[test]
    fn a_burst_that_losses_spread_past_the_stage_opens_inside_it() {
        const END: Duration = Duration::from_secs(6);
        const WAIT_MS: u32 = 5000;
        // One frame in four never comes: the stage fills with frames that span a third more than its length, all inside the reach of a recorder that waited 5 s.
        let came = |frame: &usize| frame % 4 != 3;
        let staged: Vec<usize> = (0..).filter(came).take(PREROLL_STAGE_FRAMES).collect();
        let newest = *staged.last().unwrap();
        let mut lands = burst(0..newest + 1, BURST_FRAMES, END);
        for (_, frames) in &mut lands {
            frames.retain(came);
        }
        let full = lands.last().unwrap().0;
        // The run keeps the frames less than a stage before the last; the frame on each side of that cut came, and the mark on the first kept one tells a cut one frame off.
        let kept = newest + 1 - PREROLL_STAGE_FRAMES;
        assert!(came(&kept) && came(&(kept - 1)));
        let lost: Vec<usize> = (kept..newest).filter(|frame| !came(frame)).collect();
        let twin = twin(kept..newest + 1, Some(kept), &lost);
        let head = head(WAIT_MS);
        let mut take = Take::new(0, Some(kept));
        take.play(
            lands,
            linked(full, WAIT_MS),
            head / QUANTUM + newest + 1 - kept,
        );
        assert_eq!(take.tape.len(), head + twin.len());
        assert_opens(&take.tape, head, &twin);
    }
}
