//! Where a dictation span opens: a run placed inside or as a read, the previous run's tail, a burst before the resync read, and a stray uplink SDU.

use super::*;

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
    reader.fill(Some(aim), frames, &mut heard, None);
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
    reader.fill(aim, frames, &mut heard, None);
    assert!(silent(&heard));
    opens(&mut reader, now + PERIOD);
}

#[test]
fn the_previous_runs_tail_does_not_spend_the_new_spans_opening() {
    let (mut chain, mut reader, resync) = chain(Arc::new(Counters::default())).unwrap();
    resync.request();
    let mut heard = [0.0; QUANTUM];
    reader.read(Duration::ZERO, &mut heard);
    chain.dictation_run();
    chain.frames(Duration::ZERO, (0..).zip(tone()).take(1));
    chain.flush_dictation(Duration::ZERO);

    resync.request();
    let linked = Duration::from_millis(u64::from(DOAP_LATENCY_MS));
    reader.read(linked, &mut heard);
    assert!(
        !silent(&heard),
        "the previous span still had a tail to play"
    );

    let next = linked + PERIOD;
    restart(&mut chain, next);
    reader.read(next, &mut heard);
    assert!(
        reader.opening.is_some(),
        "the new span must open on its pre-roll"
    );
}

#[test]
fn an_empty_previous_run_does_not_spend_the_new_spans_opening() {
    let (mut chain, mut reader, resync) = chain(Arc::new(Counters::default())).unwrap();
    let mut heard = [1.0; QUANTUM];
    resync.request();
    reader.read(Duration::ZERO, &mut heard);
    chain.dictation_run();
    chain.flush_dictation(Duration::ZERO);
    let previous = reader.ring.run();
    let generation = reader.ring.generation.load(Ordering::Acquire);

    let linked = Duration::from_millis(u64::from(DOAP_LATENCY_MS));
    resync.request();
    reader.read(linked, &mut heard);
    assert!(silent(&heard));

    let next = linked + PERIOD;
    restart(&mut chain, next);
    assert_eq!(reader.ring.run(), previous);
    assert_ne!(reader.ring.generation.load(Ordering::Acquire), generation);
    opens(&mut reader, next);
}

#[test]
fn a_burst_before_the_resync_read_keeps_its_opening() {
    let (mut chain, mut reader, resync) = chain(Arc::new(Counters::default())).unwrap();
    let mut heard = [0.0; QUANTUM];
    resync.request();
    reader.read(Duration::ZERO, &mut heard);
    chain.dictation_run();
    chain.flush_dictation(Duration::ZERO);

    let linked = Duration::from_millis(u64::from(DOAP_LATENCY_MS));
    resync.request();
    chain.dictation_run();
    let mut frames = (0..=BURST).zip(tone());
    chain.frames(linked, frames.by_ref().take(usize::from(BURST)));
    reader.read(linked, &mut heard);
    assert!(silent(&heard));

    let steady = linked + PERIOD;
    chain.frames(steady, frames);
    opens(&mut reader, steady);
}

#[test]
fn a_stray_uplink_sdu_does_not_spend_the_dictation_opening() {
    use pods_samples::uplink::{STAMP_STEP, tone_unit};

    const TONE_LINE: u16 = 15;
    const TONE_LEVEL: u8 = 176;
    const UNITS_PER_SDU: u32 = 4;
    let (mut chain, mut reader, resync) = chain(Arc::new(Counters::default())).unwrap();
    let mut heard = [0.0; QUANTUM];
    resync.request();
    reader.read(Duration::ZERO, &mut heard);
    let unit = tone_unit(TONE_LINE, TONE_LEVEL);
    let units = (0..UNITS_PER_SDU).map(|i| (i * STAMP_STEP, unit.as_slice()));
    chain.sdu(Duration::ZERO, units);
    let uplink = Duration::from_millis(u64::from(NODE_LATENCY_MS));
    reader.read(uplink, &mut heard);
    assert!(!silent(&heard));
    assert_eq!(reader.heard, Some(reader.ring.run()));
    for quanta in 1..=2 {
        reader.read(uplink + PERIOD * quanta, &mut heard);
    }
    assert!(silent(&heard));

    let now = uplink + PERIOD * 3;
    restart(&mut chain, now);
    opens(&mut reader, now);
    let next = reader.next;
    reader.read(now + PERIOD, &mut heard);
    assert_eq!(reader.next, next + i64::try_from(QUANTUM).unwrap());
}
