use super::{
    Arc, DOAP_LATENCY_MS, Duration, NODE_LATENCY_MS, NS_PER_SECOND, PARTS_PER_MILLION, PERIOD,
    Path, QUANTUM, RATE_HZ, REALIGN_MS, Reader, Ring, SPEECH_FLOOR, STALL_RATIO_LIMIT,
    STEER_MAX_PPM, ns_of, scaled,
};
use crate::NS_PER_MS;

#[test]
fn stalled_speech_gets_at_most_one_second_of_fast_source_frames_per_stall() {
    const LATE_PERIOD: Duration = Duration::from_millis(200);
    const FAST_AUDIO: Duration = Duration::from_secs(1);
    let max_fast_frames = scaled(ns_of(FAST_AUDIO), RATE_HZ, NS_PER_SECOND);
    let drift = STEER_MAX_PPM / PARTS_PER_MILLION;
    let bound = i64::from(REALIGN_MS) * i64::from(crate::FRAMES_PER_MS);
    for (path, latency, converter) in [
        (Path::Dictation, DOAP_LATENCY_MS, false),
        (Path::Uplink, NODE_LATENCY_MS, false),
        (Path::Dictation, DOAP_LATENCY_MS, true),
        (Path::Uplink, NODE_LATENCY_MS, true),
    ] {
        let ring = Arc::new(Ring::new());
        let (mut reader, resync) = Reader::new(Arc::clone(&ring), Arc::default());
        ring.start_run(path);
        ring.publish(0);
        resync.request();
        let mut now = Duration::from_millis(u64::from(latency)) + PERIOD;
        let mut heard = [0.0; QUANTUM];
        let mut read = |now: Duration| {
            let end = scaled(ns_of(now), RATE_HZ, NS_PER_SECOND);
            ring.write((ring.end()..end).map(|_| SPEECH_FLOOR));
            let ratio = if converter {
                reader.read_for_converter(now, &mut heard, QUANTUM)
            } else {
                reader.read(now, &mut heard)
            };
            let (target, _) = ring.target(now).unwrap();
            (ratio, target - reader.next)
        };
        read(now);
        let quantum = i64::try_from(QUANTUM).unwrap();
        for stall in 1..=3 {
            now += LATE_PERIOD;
            for spent in (0..max_fast_frames).step_by(QUANTUM) {
                let (ratio, lag) = read(now);
                assert!(
                    lag > bound,
                    "{path:?}, converter {converter}, stall {stall}, lag {lag}"
                );
                assert!(
                    ratio > 1.0 + drift && ratio <= STALL_RATIO_LIMIT,
                    "{path:?}, converter {converter}, stall {stall}, ratio {ratio} after {spent} fast frames"
                );
                now += PERIOD.div_f64(ratio);
            }
            // The third stall lands while the second's last fast read is still charged, and must start a full budget.
            if stall == 2 {
                continue;
            }
            for _ in 0..max_fast_frames / quantum {
                let (ratio, lag) = read(now);
                assert!(
                    lag > bound,
                    "{path:?}, converter {converter}, stall {stall}, lag {lag}"
                );
                assert!(
                    ratio <= 1.0 + drift,
                    "{path:?}, converter {converter}, stall {stall}, ratio {ratio} after {max_fast_frames} fast frames"
                );
                now += PERIOD.div_f64(ratio);
            }
        }
    }
}

#[test]
fn an_anchor_advance_does_not_extend_an_active_stall_budget() {
    const STALL: Duration = Duration::from_millis(200);
    const SMALL_ADVANCE_MS: i64 = 2;
    let first = Duration::from_millis(u64::from(DOAP_LATENCY_MS)) + PERIOD;
    let stalled = first + STALL;
    let quantum = i64::try_from(QUANTUM).unwrap();
    for advance_ms in [SMALL_ADVANCE_MS, i64::from(REALIGN_MS) + 1] {
        let ring = Arc::new(Ring::new());
        let (mut reader, resync) = Reader::new(Arc::clone(&ring), Arc::default());
        ring.start_run(Path::Dictation);
        ring.publish(0);
        let written = scaled(ns_of(stalled + PERIOD), RATE_HZ, NS_PER_SECOND);
        ring.write((0..written).map(|_| SPEECH_FLOOR));
        let mut heard = [0.0; QUANTUM];

        resync.request();
        reader.read(first, &mut heard);
        let ratio = reader.read(stalled, &mut heard);
        assert!(ratio > 1.0 + STEER_MAX_PPM / PARTS_PER_MILLION);
        let remaining = reader.catch_up_left;
        assert!(remaining > quantum);
        assert_eq!(reader.catch_up_floor, REALIGN_MS);

        ring.publish(-advance_ms * NS_PER_MS);
        reader.read(stalled + PERIOD.div_f64(ratio), &mut heard);
        assert_eq!(
            reader.catch_up_left,
            remaining - quantum,
            "a {advance_ms} ms advance refreshed the stall budget"
        );
        assert_eq!(reader.catch_up_floor, REALIGN_MS);
    }
}

#[test]
fn an_anchor_advance_during_opening_uses_only_the_opening_rate() {
    const ADVANCE_MS: i64 = 2;
    let ring = Arc::new(Ring::new());
    let (mut reader, resync) = Reader::new(Arc::clone(&ring), Arc::default());
    let mut heard = [0.0; QUANTUM];
    resync.request();
    reader.read(Duration::ZERO, &mut heard);

    ring.start_run(Path::Dictation);
    let now = Duration::from_millis(u64::from(DOAP_LATENCY_MS)) + 5 * PERIOD;
    ring.open(now);
    ring.publish(0);
    let written = scaled(ns_of(now + PERIOD), RATE_HZ, NS_PER_SECOND);
    ring.write((0..written).map(|_| SPEECH_FLOOR));
    let ratio = reader.read(now, &mut heard);
    assert!(ratio > STALL_RATIO_LIMIT);
    assert!(reader.opening.is_some());

    ring.publish(-ADVANCE_MS * NS_PER_MS);
    let ratio = reader.read(now + PERIOD.div_f64(ratio), &mut heard);
    assert!(ratio > STALL_RATIO_LIMIT);
    assert_eq!(
        reader.catch_up_left, 0,
        "the anchor advance armed a second rate budget during opening"
    );
}

#[test]
fn an_anchor_advance_does_not_renew_a_spent_stall_budget() {
    const STALL: Duration = Duration::from_millis(200);
    const ADVANCE_MS: i64 = 2;
    const READS_AFTER: usize = 100;
    let max_fast_frames = scaled(ns_of(Duration::from_secs(1)), RATE_HZ, NS_PER_SECOND);
    let drift = STEER_MAX_PPM / PARTS_PER_MILLION;
    let bound = i64::from(REALIGN_MS) * i64::from(crate::FRAMES_PER_MS);
    let quantum = i64::try_from(QUANTUM).unwrap();
    for converter in [false, true] {
        let ring = Arc::new(Ring::new());
        let (mut reader, resync) = Reader::new(Arc::clone(&ring), Arc::default());
        ring.start_run(Path::Dictation);
        ring.publish(0);
        resync.request();
        let mut now = Duration::from_millis(u64::from(DOAP_LATENCY_MS)) + PERIOD;
        let mut heard = [0.0; QUANTUM];
        let mut read = |now: Duration| {
            let end = scaled(ns_of(now), RATE_HZ, NS_PER_SECOND);
            ring.write((ring.end()..end).map(|_| SPEECH_FLOOR));
            let ratio = if converter {
                reader.read_for_converter(now, &mut heard, QUANTUM)
            } else {
                reader.read(now, &mut heard)
            };
            let (target, _) = ring.target(now).unwrap();
            (ratio, target - reader.next)
        };
        read(now);
        now += STALL;
        let mut fast = 0;
        for _ in (0..max_fast_frames).step_by(QUANTUM) {
            let (ratio, _) = read(now);
            if ratio > 1.0 + drift {
                fast += quantum;
            }
            now += PERIOD.div_f64(ratio);
        }
        assert_eq!(fast, max_fast_frames, "converter {converter}");
        ring.publish(-ADVANCE_MS * NS_PER_MS);
        for _ in 0..READS_AFTER {
            let (ratio, lag) = read(now);
            assert!(lag > bound, "converter {converter}, lag {lag}");
            assert!(
                ratio <= 1.0 + drift,
                "converter {converter}: a {ADVANCE_MS} ms anchor advance renewed a spent stall budget at ratio {ratio}"
            );
            now += PERIOD.div_f64(ratio);
        }
    }
}
