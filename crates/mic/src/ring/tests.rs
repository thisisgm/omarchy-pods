//! Support tests: the one word that holds the run's arrival or none, the step between time and frames, how far a write runs past the index it shows, and a run that starts inside a read, at either of the two places it can, which no caller can make happen on order; and what a read adds to the counters of samples no decoder gives on order.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use pods_samples::opus::tone;

use super::{
    Frame, Moment, NS_PER_SECOND, OPENING_MAX_PPM, PARTS_PER_MILLION, Reader, Ring, UNIT_FRAMES,
    fade_in, scaled,
};
use crate::{Chain, Counters, FRAME_MS, NODE_LATENCY_MS, chain, level};

/// One read, 20 ms at the node's rate.
const QUANTUM: usize = 1280;
const PERIOD: Duration = Duration::from_millis(20);
/// The frames of the stream's burst; the steady frame after them has this counter and places them.
const BURST: u16 = 5;
/// A frame that landed at zero is read out the stated latency later, so a read one frame after that finds the run dry.
const LINKED_MS: u32 = NODE_LATENCY_MS + FRAME_MS;

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
    let clamp = 1.0 + OPENING_MAX_PPM / PARTS_PER_MILLION;
    assert_eq!(ratio.to_bits(), clamp.to_bits());
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
fn a_dictation_placed_inside_a_read_is_not_played_by_it_and_opens_on_the_next() {
    let (mut chain, mut reader, now) = linked_after_a_dictation();
    let frames = i64::try_from(QUANTUM).unwrap();
    // The read stands by the time of the run that is over, and before it hands out a frame the stream starts again and its opening is placed.
    let over = reader.ring.run();
    let aim = reader.aim(over, now, frames).unwrap();
    restart(&mut chain, now);
    // Every frame the read is about to hand out is now a sample of the run that started.
    let started = reader.ring.run();
    assert_ne!(started, over);
    let mut due = reader.next..reader.next + frames;
    assert!(
        due.all(|at| matches!(reader.ring.frame(at), Frame::Sample { run, .. } if run == started))
    );
    let mut heard = [1.0; QUANTUM];
    reader.fill(Some(aim), frames, &mut heard);
    // The read stood by the time of the run before, so it has no place for a frame of this one.
    assert!(silent(&heard));
    opens(&mut reader, now + PERIOD);
}

#[test]
fn a_dictation_placed_as_a_read_takes_its_time_is_not_stood_by_and_opens_on_the_next() {
    let (mut chain, mut reader, now) = linked_after_a_dictation();
    let frames = i64::try_from(QUANTUM).unwrap();
    // The read notes the run that is over, and before it reads that run's time the stream starts again and tells its own.
    let over = reader.ring.run();
    restart(&mut chain, now);
    assert!(reader.ring.target(now).is_some());
    let aim = reader.aim(over, now, frames);
    // The time is not of the run the read noted, so the read stands by none and counts no opening.
    assert!(aim.is_none());
    let counted = &reader.counters.opening_behind_ms;
    assert_eq!(counted.load(Ordering::Relaxed), 0);
    let mut heard = [1.0; QUANTUM];
    reader.fill(aim, frames, &mut heard);
    assert!(silent(&heard));
    opens(&mut reader, now + PERIOD);
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
