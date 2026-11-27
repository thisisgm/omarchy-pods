//! Support tests: the one word that holds the run's arrival or none, the step between time and frames, how far a write runs past the index it shows, and a run that starts inside a read, at either of the two places it can, which no caller can make happen on order; and what a read adds to the counters of samples no decoder gives on order.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use pods_samples::opus::tone;

use super::{
    FADE_FRAMES, Frame, Moment, NS_PER_SECOND, OPENING_MAX_PPM, PARTS_PER_MILLION, Path,
    QUIET_PEAK, REALIGN_MS, RING_SAMPLES, Reader, Ring, SILENCE_FRAMES, SILENCE_PEAK,
    STEER_MAX_PPM, UNIT_FRAMES, fade_in, ns_of, scaled,
};
use crate::{Chain, Counters, DOAP_LATENCY_MS, FRAME_MS, NODE_LATENCY_MS, RATE_HZ, chain, level};

/// One read, 20 ms at the node's rate.
const QUANTUM: usize = 1280;
const PERIOD: Duration = Duration::from_millis(20);
const SPEECH_FLOOR: f32 = 0.02;
const STALL_RATIO_LIMIT: f64 = 1.045;
/// The frames of the stream's burst; the steady frame after them has this counter and places them.
const BURST: u16 = 5;
/// A frame that landed at zero is read out the stated latency later, so a read one frame after that finds the run dry.
const LINKED_MS: u32 = DOAP_LATENCY_MS + FRAME_MS;

fn silent(heard: &[f32]) -> bool {
    heard.iter().all(|sample| sample.to_bits() == 0)
}

/// A recorder that linked to a dictation that was over and heard none of it, the chain it reads, and the time of its next read.
fn linked_after_a_dictation() -> (Chain, Reader, Duration) {
    let (mut chain, mut reader, resync) = chain(Arc::new(Counters::default())).unwrap();
    // The run's one frame told its time.
    chain.dictation_run();
    chain.frames(Duration::ZERO, (0..).zip(tone()).take(1));
    resync.request();
    let linked = Duration::from_millis(u64::from(LINKED_MS));
    let mut heard = [1.0; QUANTUM];
    reader.read(linked, &mut heard);
    assert!(silent(&heard));
    (chain, reader, linked.saturating_add(PERIOD))
}

/// The stream starts again at `now`: its burst, then the steady frame that places the opening.
fn restart(chain: &mut Chain, now: Duration) {
    let mut frames = (0..=BURST).zip(tone());
    chain.dictation_run();
    chain.frames(now, frames.by_ref().take(usize::from(BURST)));
    chain.frames(now, frames);
}

/// The read at `now` starts on the run's first frame, fading in, and asks for the opening's rate.
fn opens(reader: &mut Reader, now: Duration) {
    let mut heard = [0.0; QUANTUM];
    let ratio = reader.read(now, &mut heard);
    let clamp = 1.0 + OPENING_MAX_PPM / PARTS_PER_MILLION;
    assert_eq!(
        ratio.to_bits(),
        clamp.to_bits(),
        "the opening must catch up to live"
    );
    let run = reader.ring.run();
    let first = i64::try_from(run).unwrap();
    let written = (0..).zip(0..).take(QUANTUM);
    let mut opened: Vec<f32> = written
        .map(|(at, faded)| reader.ring.sample(run, first.saturating_add(at)) * fade_in(faded))
        .collect();
    level::apply(&mut opened);
    assert!(!silent(&heard));
    let opened: Vec<u32> = opened.into_iter().map(f32::to_bits).collect();
    assert_eq!(heard.map(f32::to_bits).to_vec(), opened);
}

#[test]
fn time_and_frames_convert_at_a_rate_that_does_not_divide_a_second() {
    // A unit is 7.5 ms at either rate, and a frame at 48 kHz is 20 833 ns and a third.
    for (rate, frames, ns) in [
        (64_000, 480, 7_500_000),
        (48_000, 360, 7_500_000),
        (48_000, 1, 20_833),
    ] {
        assert_eq!(
            scaled(frames, NS_PER_SECOND, rate),
            ns,
            "{frames} at {rate}"
        );
    }
    for (rate, ns, frames) in [
        (64_000, 7_500_000, 480),
        (48_000, 7_500_000, 360),
        (48_000, 7_499_999, 359),
        // A time before the run's arrival rounds down, not toward zero.
        (48_000, -1, -1),
    ] {
        assert_eq!(scaled(ns, rate, NS_PER_SECOND), frames, "{ns} ns at {rate}");
    }
    // What no word holds stays at the end it left by.
    assert_eq!(scaled(i64::MAX, NS_PER_SECOND, 48_000), i64::MAX);
    assert_eq!(scaled(i64::MIN, NS_PER_SECOND, 48_000), i64::MIN);
}

#[test]
fn an_arrival_is_never_read_back_as_none() {
    let word = Moment::none();
    assert_eq!(word.load(), None);
    for ns in [i64::MAX, 0, -1, i64::MIN + 1] {
        word.store(Some(ns));
        assert_eq!(word.load(), Some(ns));
    }
    // The one value the word keeps for none is still an arrival when it is stored: one ns later.
    word.store(Some(i64::MIN));
    assert_eq!(word.load(), Some(i64::MIN + 1));
    word.store(None);
    assert_eq!(word.load(), None);
}

#[test]
fn a_write_longer_than_a_unit_shows_its_index_every_unit() {
    let ring = Ring::new();
    let unit = i64::from(UNIT_FRAMES);
    let long = 3 * unit + 7;
    // The reader's lap check allows the writer one unit past the index it shows, so no sample may be stored further on than that.
    let mut furthest = 0;
    ring.write((0..long).map(|stored| {
        furthest = furthest.max(stored - ring.end());
        0.0
    }));
    assert_eq!(furthest, unit - 1);
    assert_eq!(ring.end(), long);
}

#[test]
fn a_quiet_output_waits_for_the_whole_window_and_any_unplayed_signal() {
    let ring = Arc::new(Ring::new());
    let (mut reader, _) = Reader::new(Arc::clone(&ring), Arc::default());
    let mut quiet = vec![0.0; usize::try_from(SILENCE_FRAMES - 1).unwrap()];
    reader.read(Duration::ZERO, &mut quiet);
    assert!(!ring.output_silent());
    reader.read(Duration::ZERO, &mut [0.0]);
    assert!(ring.output_silent());

    let signal = SILENCE_PEAK * 2.0;
    ring.write(std::iter::once(signal));
    assert!(!ring.output_silent());
    ring.note_output(1, &[signal], false);
    assert!(!ring.output_silent());
    ring.note_output(1, &quiet, false);
    assert!(!ring.output_silent());
    ring.note_output(1, &[0.0], false);
    assert!(ring.output_silent());
    ring.note_output(1, &[0.0], true);
    assert!(!ring.output_silent());
}

#[test]
fn a_resync_read_restarts_the_quiet_window() {
    let ring = Arc::new(Ring::new());
    let (mut reader, resync) = Reader::new(Arc::clone(&ring), Arc::default());
    let mut quiet = vec![0.0; usize::try_from(SILENCE_FRAMES).unwrap()];
    reader.read(Duration::ZERO, &mut quiet);
    assert!(ring.output_silent());

    resync.request();
    reader.read(Duration::ZERO, &mut [0.0]);
    assert!(!ring.output_silent());
}

#[test]
fn a_dry_read_leaves_the_next_signal_unplayed() {
    let ring = Arc::new(Ring::new());
    let (mut reader, _) = Reader::new(Arc::clone(&ring), Arc::default());
    ring.write(std::iter::repeat_n(0.0, usize::from(UNIT_FRAMES)));
    ring.publish(ring.arrival_at(Duration::ZERO));

    let now = Duration::from_millis(u64::from(NODE_LATENCY_MS)).saturating_add(PERIOD);
    let mut quiet = vec![0.0; usize::try_from(SILENCE_FRAMES).unwrap()];
    reader.read(now, &mut quiet);
    assert!(reader.next > ring.end());
    assert!(ring.output_silent());

    ring.write(std::iter::once(SILENCE_PEAK * 2.0));
    assert!(!ring.output_silent());
}

#[test]
fn a_staged_dictation_burst_keeps_the_chain_from_reporting_silence() {
    let (mut chain, mut reader, _) = chain(Arc::new(Counters::default())).unwrap();
    let mut quiet = vec![0.0; usize::try_from(SILENCE_FRAMES.saturating_add(1)).unwrap()];
    reader.read(Duration::ZERO, &mut quiet);
    assert!(chain.output_silent());

    chain.dictation_run();
    chain.frames(PERIOD, (0..BURST).zip(tone()));
    assert!(chain.dictation.opening_pending());
    assert!(chain.ring.output_silent());
    assert!(!chain.output_silent());
}

#[test]
fn a_read_adds_what_the_level_met_to_the_counters_once() {
    /// Past the reader's fade in and before the last unit, which plays on backwards when the ring runs dry.
    const LOUD: [usize; 3] = [400, 401, 700];
    const NOT_FINITE: [(usize, f32); 2] = [(500, f32::NAN), (600, f32::INFINITY)];
    let counters = Arc::new(Counters::default());
    let ring = Arc::new(Ring::new());
    let (mut reader, resync) = Reader::new(Arc::clone(&ring), Arc::clone(&counters));
    let mut written = [0.01_f32; QUANTUM];
    for at in LOUD {
        written[at] = 0.5;
    }
    for (at, sample) in NOT_FINITE {
        written[at] = sample;
    }
    ring.write(written.iter().copied());
    ring.publish(ring.arrival_at(Duration::ZERO));
    resync.request();
    let counted =
        || [&counters.limited, &counters.nonfinite].map(|count| count.load(Ordering::Relaxed));
    let mut heard = [0.0; QUANTUM];
    let now = Duration::from_millis(u64::from(NODE_LATENCY_MS));
    reader.read(now, &mut heard);
    assert_eq!(counted(), [3, 2]);
    for (at, _) in NOT_FINITE {
        assert_eq!(heard[at].to_bits(), 0, "sample {at}");
    }
    assert!(heard[LOUD[0]] > heard[LOUD[0] - 1] && heard.iter().all(|sample| sample.is_finite()));
    // The next read hands out the fall of quiet audio and silence, which adds to neither count.
    reader.read(now.saturating_add(PERIOD), &mut heard);
    assert!(!silent(&heard));
    assert_eq!(counted(), [3, 2]);
}

fn a_late_read_keeps_every_frame(late_period: Duration) {
    const NORMAL_READS: usize = 6;
    const SAMPLE_STEP: f32 = 0.000_001;
    let late_quanta = usize::try_from(
        late_period
            .as_nanos()
            .checked_div(PERIOD.as_nanos())
            .unwrap(),
    )
    .unwrap();
    let sample_count = NORMAL_READS
        .saturating_add(late_quanta)
        .saturating_mul(QUANTUM);
    let samples: Vec<_> = (1_u16..)
        .take(sample_count)
        .map(|frame| SPEECH_FLOOR + f32::from(frame) * SAMPLE_STEP)
        .collect();
    for (path, latency) in [
        (Path::Uplink, NODE_LATENCY_MS),
        (Path::Dictation, DOAP_LATENCY_MS),
    ] {
        let counters = Arc::new(Counters::default());
        let ring = Arc::new(Ring::new());
        let (mut reader, resync) = Reader::new(Arc::clone(&ring), Arc::clone(&counters));
        ring.start_run(path);
        ring.publish(0);
        resync.request();
        let mut now = Duration::from_millis(u64::from(latency));
        let mut written = 0;
        let mut elapsed_quanta = 0_usize;
        let mut expected = samples.clone();
        for (sample, fade) in expected.iter_mut().zip(0_u16..) {
            *sample *= fade_in(fade);
        }
        level::apply(&mut expected);
        for (read, expected) in expected.as_chunks::<QUANTUM>().0.iter().enumerate() {
            let (interval, quanta) = if read == 1 {
                (late_period, late_quanta)
            } else {
                (PERIOD, 1)
            };
            now = now.saturating_add(interval);
            elapsed_quanta = elapsed_quanta.saturating_add(quanta);
            let end = elapsed_quanta.saturating_mul(QUANTUM).min(samples.len());
            ring.write(samples[written..end].iter().copied());
            written = end;
            let mut heard = [0.0; QUANTUM];
            reader.read(now, &mut heard);
            assert!(
                heard
                    .iter()
                    .zip(expected)
                    .all(|(heard, expected)| heard.to_bits() == expected.to_bits()),
                "{path:?} read {read} at {now:?} after a {late_period:?} interval: cursor {}",
                reader.next
            );
        }
        assert_eq!(written, samples.len());
        assert!(reader.read_all());
        assert_eq!(counters.span_gone_frames.load(Ordering::Relaxed), 0);
        assert_eq!(counters.underruns.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn a_read_after_60_ms_hands_out_every_frame_once_in_order() {
    a_late_read_keeps_every_frame(Duration::from_millis(60));
}

#[test]
fn a_read_after_200_ms_hands_out_every_frame_once_in_order() {
    a_late_read_keeps_every_frame(Duration::from_millis(200));
}

#[test]
fn gone_frames_count_only_after_the_recorder_heard_audio() {
    let counters = Arc::new(Counters::default());
    let ring = Arc::new(Ring::new());
    let (mut reader, resync) = Reader::new(Arc::clone(&ring), Arc::clone(&counters));
    let mut heard = [1.0; QUANTUM];
    let latency = Duration::from_millis(u64::from(NODE_LATENCY_MS));
    let gone = || counters.span_gone_frames.load(Ordering::Relaxed);

    resync.request();
    reader.read(Duration::ZERO, &mut heard);
    assert!(silent(&heard));
    assert_eq!(gone(), 0);

    ring.write((0..2 * QUANTUM).map(|_| SPEECH_FLOOR));
    ring.publish(ns_of(PERIOD));
    reader.read(PERIOD + latency, &mut heard);
    assert!(silent(&heard));
    assert_eq!(gone(), 0, "the first audio was not yet due");

    reader.read(2 * PERIOD + latency, &mut heard);
    assert!(!silent(&heard));
    ring.start_run(Path::Uplink);
    reader.read(3 * PERIOD + latency, &mut heard);
    assert_eq!(gone(), u64::try_from(QUANTUM).unwrap());

    counters.reset_span();
    resync.request();
    ring.write((0..QUANTUM).map(|_| SPEECH_FLOOR));
    ring.start_run(Path::Uplink);
    reader.read(4 * PERIOD + latency, &mut heard);
    assert!(silent(&heard));
    assert_eq!(gone(), 0, "the next recorder had yet to hear audio");
}

#[test]
fn a_lapped_reader_counts_only_the_frames_the_writer_overwrote() {
    let counters = Arc::new(Counters::default());
    let ring = Arc::new(Ring::new());
    let (mut reader, resync) = Reader::new(Arc::clone(&ring), Arc::clone(&counters));
    let quantum = i64::try_from(QUANTUM).unwrap();
    let mut heard = [0.0; QUANTUM];
    ring.write((0..quantum).map(|_| 0.0));
    ring.publish(0);
    resync.request();
    let first = Duration::from_millis(u64::from(NODE_LATENCY_MS)) + PERIOD;
    reader.read(first, &mut heard);

    let lost = quantum;
    let end = quantum + lost + i64::try_from(RING_SAMPLES).unwrap() - i64::from(UNIT_FRAMES);
    ring.write((quantum..end).map(|_| 0.0));
    let now = Duration::from_millis(u64::from(NODE_LATENCY_MS))
        + Duration::from_nanos(u64::try_from(scaled(end, NS_PER_SECOND, RATE_HZ)).unwrap());
    reader.read(now, &mut heard);
    assert_eq!(reader.next, quantum + lost + quantum);
    assert_eq!(
        counters.span_gone_frames.load(Ordering::Relaxed),
        u64::try_from(lost).unwrap()
    );
}

#[test]
fn steering_brings_a_late_reader_back_within_the_realign_bound() {
    // Ruling A gives one second of fast audio, enough to recover from this 60 ms stall.
    const LATE_PERIOD: Duration = Duration::from_millis(60);
    const RECOVERY: Duration = Duration::from_secs(5);
    let ring = Arc::new(Ring::new());
    let (mut reader, resync) = Reader::new(Arc::clone(&ring), Arc::default());
    ring.publish(0);
    resync.request();
    let mut now = Duration::from_millis(u64::from(NODE_LATENCY_MS)) + PERIOD;
    let mut heard = [0.0; QUANTUM];
    let mut read = |now: Duration| {
        let end = scaled(ns_of(now), RATE_HZ, NS_PER_SECOND);
        ring.write((ring.end()..end).map(|_| SPEECH_FLOOR));
        let ratio = reader.read(now, &mut heard);
        let (target, _) = ring.target(now).unwrap();
        let lag = Duration::from_nanos(
            u64::try_from(scaled((target - reader.next).abs(), NS_PER_SECOND, RATE_HZ)).unwrap(),
        );
        (ratio, lag)
    };
    read(now);
    now += LATE_PERIOD;
    let (mut ratio, mut lag) = read(now);
    let bound = Duration::from_millis(u64::from(REALIGN_MS));
    assert!(lag > bound, "late read left only {lag:?} of backlog");
    let deadline = now + RECOVERY;
    while lag > bound && now < deadline {
        now += PERIOD.div_f64(ratio);
        (ratio, lag) = read(now);
    }
    assert!(lag <= bound && now <= deadline, "{lag:?} of lag at {now:?}");
}

/// Follows the converter's request size per 20 ms pull and returns queued frames at release and at the tail deadline.
fn stopped_recording(
    path: Path,
    latency_ms: u32,
    late_period: Duration,
    stop_after_stall: Duration,
) -> (usize, usize) {
    const STALL_AT: Duration = Duration::from_millis(400);
    const RECORDER_TAIL: Duration = Duration::from_millis(100);
    const SAMPLE_STEP: f32 = 0.000_001;
    let first = Duration::from_millis(u64::from(latency_ms)).saturating_add(PERIOD);
    let stalled = first.saturating_add(STALL_AT);
    let stop = stalled
        .saturating_add(late_period)
        .saturating_add(stop_after_stall);
    let frames_at = |now| usize::try_from(scaled(ns_of(now), RATE_HZ, NS_PER_SECOND)).unwrap();
    let samples: Vec<_> = (1_u16..=u16::MAX)
        .cycle()
        .take(frames_at(stop))
        .map(|frame| SPEECH_FLOOR + f32::from(frame) * SAMPLE_STEP)
        .collect();
    let ring = Arc::new(Ring::new());
    let (mut reader, resync) = Reader::new(Arc::clone(&ring), Arc::default());
    ring.start_run(path);
    ring.publish(0);
    resync.request();

    let mut tape = Vec::new();
    let mut now = first;
    let mut written = 0;
    let (mut ratio, mut fraction) = (1.0, 0.0);
    let mut fast_frames = 0_usize;
    let drift = STEER_MAX_PPM / PARTS_PER_MILLION;
    let bound = i64::from(REALIGN_MS.saturating_mul(crate::FRAMES_PER_MS));
    let mut read = |reader: &mut Reader, now, tape: &mut Vec<f32>| {
        let due = f64::from(u16::try_from(QUANTUM).unwrap()) * ratio + fraction;
        let whole = due.floor();
        fraction = due - whole;
        #[expect(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a nonnegative converter request below twice the quantum"
        )]
        let count = whole as usize;
        let mut heard = [0.0; 2 * QUANTUM];
        if ratio > 1.0 + drift {
            fast_frames = fast_frames.saturating_add(count);
        }
        let capacity = heard.len();
        ratio = reader.read_for_converter(now, &mut heard[..count], capacity);
        tape.extend_from_slice(&heard[..count]);
        assert!(
            (1.0 - drift..=STALL_RATIO_LIMIT).contains(&ratio),
            "{ratio}"
        );
        let (target, _) = ring.target(now).unwrap();
        let lag = target.saturating_sub(reader.next);
        if lag <= bound {
            assert!(ratio <= 1.0 + drift, "{ratio} at {lag} frames of lag");
        }
    };
    while now <= stop {
        let end = frames_at(now);
        ring.write(samples[written..end].iter().copied());
        written = end;
        read(&mut reader, now, &mut tape);
        let interval = if now == stalled { late_period } else { PERIOD };
        now = now.saturating_add(interval);
    }

    assert_eq!(written, samples.len());
    let queued_at_stop = samples.len().checked_sub(tape.len()).unwrap();

    ring.finish();
    while !reader.read_all() && now <= stop.saturating_add(RECORDER_TAIL) {
        read(&mut reader, now, &mut tape);
        now = now.saturating_add(PERIOD);
    }
    let queued_at_deadline = samples.len().saturating_sub(tape.len());
    while !reader.read_all() && tape.len() < samples.len() {
        read(&mut reader, now, &mut tape);
        now = now.saturating_add(PERIOD);
    }
    assert!(reader.read_all());
    assert!(
        fast_frames <= usize::try_from(RATE_HZ).unwrap(),
        "{fast_frames} fast frames"
    );
    let mut expected = samples;
    for (sample, fade) in expected.iter_mut().zip(0_u16..=u16::MAX) {
        *sample *= fade_in(fade);
    }
    level::apply(&mut expected);
    assert!(
        tape[..expected.len()]
            .iter()
            .zip(&expected)
            .all(|(heard, expected)| heard.to_bits() == expected.to_bits())
    );
    (queued_at_stop, queued_at_deadline)
}

#[test]
fn a_60_ms_stall_preserves_speech_through_the_recorder_deadline() {
    use std::io::{self, Write};

    for (path, latency) in [
        (Path::Dictation, DOAP_LATENCY_MS),
        (Path::Uplink, NODE_LATENCY_MS),
    ] {
        for seconds in [1, 3] {
            let (queued, missed) = stopped_recording(
                path,
                latency,
                Duration::from_millis(60),
                Duration::from_secs(seconds),
            );
            writeln!(
                io::stderr(),
                "{path:?} after 60 ms: at {seconds} s queued {queued}, missed {missed}"
            )
            .unwrap();
            assert_eq!(missed, 0, "{path:?}, {queued} queued at release");
        }
    }
}

#[test]
fn a_200_ms_stall_preserves_speech_and_bounds_the_remaining_tail() {
    use std::io::{self, Write};

    // Ruling A caps fast audio at 1 s per stall. DoAP ceilings allow 1 ms above the measured frame counts.
    const QUEUED_AT_ONE_SECOND: usize = 13_725;
    const MISSED_AT_ONE_SECOND: usize = 7_312;
    const QUEUED_AT_THREE_SECONDS: usize = 13_469;
    const MISSED_AT_THREE_SECONDS: usize = 7_056;
    let stall = Duration::from_millis(200);
    for (path, latency) in [
        (Path::Dictation, DOAP_LATENCY_MS),
        (Path::Uplink, NODE_LATENCY_MS),
    ] {
        let (after_one, missed_one) =
            stopped_recording(path, latency, stall, Duration::from_secs(1));
        let (after_three, missed_three) =
            stopped_recording(path, latency, stall, Duration::from_secs(3));
        let extra =
            usize::try_from(latency - DOAP_LATENCY_MS).unwrap() * usize::from(crate::FRAMES_PER_MS);
        assert!(
            after_one <= QUEUED_AT_ONE_SECOND + extra
                && missed_one <= MISSED_AT_ONE_SECOND + extra
                && after_three <= QUEUED_AT_THREE_SECONDS + extra
                && missed_three <= MISSED_AT_THREE_SECONDS + extra,
            "{path:?} after 1 s: queued {after_one}, missed {missed_one}, after 3 s: queued {after_three}, missed {missed_three}"
        );
        writeln!(
            io::stderr(),
            "{path:?} after 200 ms: at 1 s queued {after_one}, missed {missed_one}, at 3 s queued {after_three}, missed {missed_three}"
        ).unwrap();
    }
}

#[test]
fn a_quiet_span_clears_the_stall_without_losing_speech_at_stop() {
    const STALL_AT: Duration = Duration::from_millis(400);
    const LATE_PERIOD: Duration = Duration::from_millis(200);
    const QUIET_START: Duration = Duration::from_millis(680);
    const QUIET_END: Duration = Duration::from_millis(980);
    const RELEASE: Duration = Duration::from_millis(1300);
    const RECORDER_TAIL: Duration = Duration::from_millis(100);
    const QUIET: f32 = 0.003;
    const SAMPLE_STEP: f32 = 0.000_001;
    let first = Duration::from_millis(u64::from(DOAP_LATENCY_MS)) + PERIOD;
    let release = first + RELEASE;
    let frames_at = |now| usize::try_from(scaled(ns_of(now), RATE_HZ, NS_PER_SECOND)).unwrap();
    let quiet = frames_at(QUIET_START)..frames_at(QUIET_END);
    let mut samples: Vec<_> = (1_u16..=u16::MAX)
        .cycle()
        .take(frames_at(release))
        .map(|frame| SPEECH_FLOOR + f32::from(frame) * SAMPLE_STEP)
        .collect();
    let quiet_frames = f32::from(u16::try_from(quiet.len()).unwrap());
    for (sample, at) in samples[quiet].iter_mut().zip(0_u16..) {
        *sample = QUIET - 2.0 * QUIET * f32::from(at) / quiet_frames;
    }
    let ring = Arc::new(Ring::new());
    let (mut reader, resync) = Reader::new(Arc::clone(&ring), Arc::default());
    ring.start_run(Path::Dictation);
    ring.publish(0);
    resync.request();

    let mut tape = Vec::new();
    let mut heard = [0.0; QUANTUM];
    let mut now = first;
    let mut written = 0;
    let bound = i64::from(REALIGN_MS) * i64::from(crate::FRAMES_PER_MS);
    while now <= release {
        let end = frames_at(now);
        ring.write(samples[written..end].iter().copied());
        written = end;
        reader.read(now, &mut heard);
        tape.extend(heard);
        if now == first + QUIET_END || now == release {
            let (target, _) = ring.target(now).unwrap();
            let lag = target.abs_diff(reader.next);
            assert!(
                lag <= bound.unsigned_abs(),
                "{lag} frames behind at {now:?}"
            );
        }
        let interval = if now == first + STALL_AT {
            LATE_PERIOD
        } else {
            PERIOD
        };
        now = now.saturating_add(interval);
    }
    assert_eq!(written, samples.len());
    ring.finish();
    let stated_latency = Duration::from_millis(u64::from(NODE_LATENCY_MS));
    let deadline = release + stated_latency.min(RECORDER_TAIL);
    while now <= deadline && !reader.read_all() {
        reader.read(now, &mut heard);
        tape.extend(heard);
        now = now.saturating_add(PERIOD);
    }
    assert!(reader.read_all(), "the recorder ran out of tail time");
    let trailing = usize::try_from(reader.next.saturating_sub(ring.end())).unwrap();
    tape.truncate(tape.len().saturating_sub(trailing));

    for (sample, fade) in samples.iter_mut().zip(0_u16..=FADE_FRAMES) {
        *sample *= fade_in(fade);
    }
    let mut threshold = [QUIET_PEAK];
    level::apply(&mut threshold);
    level::apply(&mut samples);
    let speech = |samples: &[f32]| {
        samples
            .iter()
            .filter(|sample| sample.abs() >= threshold[0])
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>()
    };
    assert!(
        speech(&tape) == speech(&samples),
        "speech was lost or replayed"
    );
    let largest_step = |samples: &[f32], limit: f32| {
        samples
            .windows(2)
            .filter(|pair| pair.iter().all(|sample| sample.abs() < limit))
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0_f32, f32::max)
    };
    assert!(
        largest_step(&tape, f32::INFINITY) <= largest_step(&samples, f32::INFINITY),
        "a step"
    );
    assert!(
        largest_step(&tape, threshold[0]) <= largest_step(&samples, threshold[0]),
        "a step in the quiet span"
    );
}

#[test]
fn a_quiet_cut_leaves_a_fade_before_the_first_nonquiet_frame() {
    let ring = Arc::new(Ring::new());
    let (mut reader, _) = Reader::new(Arc::clone(&ring), Arc::default());
    let fade = i64::from(FADE_FRAMES);
    let peak_at = 4 * fade;
    let place = 6 * fade;
    ring.write((0..place + fade).map(|at| if at == peak_at { QUIET_PEAK } else { 0.0 }));
    reader.next = fade;
    assert_eq!(reader.quiet_destination(place, 0), Some(peak_at - fade));
}

mod catch_up;
mod opening;
