//! Playback restores require an uplink started by the daemon.

use super::*;

#[test]
fn unowned_uplink_quiet_stop_skips_restore_when_the_sink_wakes() {
    let state = linked(Column::Auto);
    let running = Fact::Link(Signal::Uplink(UplinkState::Running));
    let stopped = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    let mut mic = Mic::default();
    assert_eq!(requests(&mic.step(&state, &running, SENT)), NOTHING);

    let quiet = sink_in(true, SinkState::Suspended);
    let effects = mic.step(&state, &quiet, SENT.saturating_add(SOON));
    assert_eq!(requests(&effects), STOP);
    assert_eq!(lines(&effects), [STOP_QUIET_LINE]);
    let awake = sink_in(true, SinkState::Running);
    let now = SENT.saturating_add(SOON.saturating_mul(2));
    assert_no_restore(&mic.step(&state, &awake, now));
    let mut confirmed = mic;
    assert_no_restore(&confirmed.step(&state, &stopped, now.saturating_add(SOON)));

    let (down, _, disconnected) = stepped(Event::Down, Column::Auto);
    let mut dropped = mic;
    assert_no_restore(&dropped.step(&disconnected, &down, now));

    let overdue = SENT
        .saturating_add(SOON)
        .saturating_add(Duration::from_millis(STOP_ANSWER_MS));
    let expired = mic.step(&state, &Fact::Tick, overdue);
    assert_eq!(lines(&expired), [STOP_UNCONFIRMED_LINE]);
    assert_no_restore(&expired);
    assert_eq!(mic.phase, Phase::Stopped(Stopped::default()));
    assert_no_restore(&mic.step(&state, &stopped, overdue.saturating_add(SOON)));
}

#[test]
fn a_late_first_start_answer_owns_the_uplink_only_inside_its_window() {
    let always = linked(Column::AlwaysKnown);
    let auto = linked(Column::Auto);
    let running = Fact::Link(Signal::Uplink(UplinkState::Running));
    let stopped = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    let deadline = SENT.saturating_add(Duration::from_millis(START_ANSWER_MS));
    let expires = deadline.saturating_add(Duration::from_millis(UPLINK_STALL_MS));
    let late = deadline.saturating_add(SOON);

    for (fact, at) in [
        (Fact::Tick, deadline),
        (Fact::Sleep(SleepEdge::Sleeping), SENT.saturating_add(SOON)),
    ] {
        let mut waiting = Mic::default();
        assert_eq!(
            requests(&waiting.step(&always, &Fact::Changed, SENT)),
            START
        );
        assert_eq!(requests(&waiting.step(&auto, &fact, at)), NOTHING);
        assert!(matches!(waiting.phase, Phase::Stopped(_)));
        if waiting.asleep {
            drop(waiting.step(&auto, &Fact::Sleep(SleepEdge::Woke), late));
        }
        for (answer, expected) in [
            (late, &[RestoreId(0)][..]),
            (expires, &[]),
            (expires.saturating_add(SOON), &[]),
        ] {
            let mut mic = waiting;
            let began = mic.step(&auto, &running, answer);
            assert_eq!(requests(&began), NOTHING);
            let ended = mic.step(&auto, &stopped, answer.saturating_add(SOON));
            assert_eq!(restore_ids(&ended), expected, "{fact:?} at {answer:?}");
        }
    }
}

#[test]
fn unowned_uplink_stopped_from_late_skips_restore() {
    let state = linked(Column::Auto);
    let stopped = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    let mut mic = Mic {
        phase: Phase::Running(Hold::Late, watch(), Ownership::Unowned),
        ..Mic::default()
    };
    let effects = mic.step(&state, &stopped, SENT.saturating_add(SOON));
    assert_eq!(lines(&effects), [STOPPED_BY_THE_PODS_LINE]);
    assert_no_restore(&effects);
}

#[test]
fn unowned_uplink_marker_pairs_do_not_repeat_a_playback_restore() {
    const MARKER_PAIRS: u32 = 3;
    let state = linked(Column::Auto);
    let always = linked(Column::AlwaysKnown);
    let running = Fact::Link(Signal::Uplink(UplinkState::Running));
    let stopped = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    let mut mic = Mic::default();
    assert_eq!(requests(&mic.step(&always, &Fact::Changed, LEFT)), START);
    drop(mic.step(&always, &running, LEFT.saturating_add(SOON)));
    let initial = mic.step(&always, &stopped, SENT);
    assert!(initial.contains(&Effect::Audio(AudioRequest::Restore(RestoreId(0)))));

    let mut restores = Vec::new();
    let mut now = SENT.saturating_add(SOON);
    for id in 0..MARKER_PAIRS {
        let began = mic.step(&state, &running, now);
        assert_eq!(requests(&began), NOTHING);
        assert_no_restore(&began);
        let resumed = Fact::Audio(AudioFact::Restored(RestoreId(id)));
        assert_no_restore(&mic.step(&state, &resumed, now));
        now = now.saturating_add(SOON);
        let ended = mic.step(&state, &stopped, now);
        assert_eq!(requests(&ended), NOTHING);
        restores.extend(restore_ids(&ended));
        now = now.saturating_add(SOON);
    }
    assert_eq!(restores, []);
}

fn assert_no_restore(effects: &[Effect]) {
    assert_eq!(restore_ids(effects), []);
}
