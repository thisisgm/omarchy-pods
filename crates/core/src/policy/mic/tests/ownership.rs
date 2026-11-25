//! Playback restores require an uplink started by the daemon.

use super::*;

#[test]
fn an_unsolicited_uplink_marker_leaves_later_sink_edges_alone() {
    let state = linked(Column::Auto);
    let running = Fact::Link(Signal::Uplink(UplinkState::Running));
    let stopped = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    let mut mic = Mic::default();
    drop(mic.step(&state, &Fact::Changed, LEFT));
    let mut now = SENT;
    for fact in [
        running,
        sink_in(true, SinkState::Suspended),
        sink_in(true, SinkState::Running),
        stopped,
    ] {
        assert_eq!(mic.step(&state, &fact, now), [], "{fact:?}");
        assert_eq!(mic.phase, Phase::Stopped(Stopped::default()));
        now = now.saturating_add(SOON);
    }
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
fn a_running_marker_at_the_start_window_boundary_is_ignored() {
    let state = linked(Column::Off);
    let running = Fact::Link(Signal::Uplink(UplinkState::Running));
    let stopped = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    let mut mic = Mic {
        phase: Phase::Stopped(Stopped {
            spent: None,
            pending: PendingMarker::StartUntil(SENT),
        }),
        ..Mic::default()
    };
    let phase = mic.phase;
    assert_eq!(mic.step(&state, &running, SENT), []);
    assert_eq!(mic.step(&state, &stopped, SENT.saturating_add(SOON)), []);
    assert_eq!(mic.phase, phase);
}

#[test]
fn an_unsolicited_uplink_preserves_a_pending_playback_restore() {
    let always = linked(Column::AlwaysKnown);
    let auto = linked(Column::Auto);
    let running = Fact::Link(Signal::Uplink(UplinkState::Running));
    let stopped = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    let mut mic = Mic::default();
    assert_eq!(requests(&mic.step(&always, &Fact::Changed, LEFT)), START);
    drop(mic.step(&always, &running, LEFT.saturating_add(SOON)));
    let initial = mic.step(&always, &stopped, SENT);
    assert_eq!(restore_ids(&initial), [RestoreId(0)]);

    let id = RestoreId(0);
    let began = mic.step(&auto, &running, SENT.saturating_add(SOON));
    let ended = mic.step(&auto, &stopped, SENT.saturating_add(SOON.saturating_mul(2)));
    assert_eq!(began, []);
    assert_eq!(ended, []);
    assert!(matches!(mic.phase, Phase::Restoring { id: held, .. } if held == id));
}

#[test]
fn unsolicited_uplink_marker_pairs_do_not_repeat_a_playback_restore() {
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

    let mut now = SENT.saturating_add(SOON);
    for pair in 0..MARKER_PAIRS {
        let began = mic.step(&state, &running, now);
        assert_eq!(began, []);
        if pair == 0 {
            let resumed = Fact::Audio(AudioFact::Restored(RestoreId(0)));
            assert_eq!(mic.step(&state, &resumed, now), []);
        }
        now = now.saturating_add(SOON);
        let ended = mic.step(&state, &stopped, now);
        assert_eq!(ended, []);
        assert_eq!(kind(mic.phase), Kind::Stopped);
        now = now.saturating_add(SOON);
    }
}
