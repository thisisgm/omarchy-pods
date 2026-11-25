//! Support tests for the faults the uplink suites drive through the built daemon: the tries, the markers nobody asked for, a sleep and the failures counted.

use super::*;

fn owned(effects: &[Effect]) -> Vec<String> {
    lines(effects).into_iter().map(str::to_owned).collect()
}

#[test]
fn an_unanswered_start_is_sent_three_times_and_each_try_is_counted() {
    let answer = Duration::from_millis(START_ANSWER_MS);
    for (column, spent, line) in [
        (Column::AlwaysNew, Kind::Unsupported, NO_UPLINK_LINE),
        (Column::AlwaysKnown, Kind::Failed, FAILED_LINE),
    ] {
        let state = linked(column);
        let mut mic = Mic::default();
        let mut effects = mic.step(&state, &Fact::Changed, SENT);
        let mut at = SENT;
        for _ in 0..START_TRIES {
            at = at.saturating_add(answer);
            assert_eq!(mic.deadline(), Some(at), "{column:?}");
            let early = at.saturating_sub(Duration::from_millis(1));
            assert_eq!(mic.step(&state, &Fact::Tick, early), [], "{column:?}");
            effects.extend(mic.step(&state, &Fact::Tick, at));
        }
        let mut wanted = Vec::new();
        let mut sent = Vec::new();
        for tried in 1..=START_TRIES {
            wanted.extend([start_sent_line(tried), unanswered_line(tried)]);
            sent.push(UplinkRequest::Start);
        }
        wanted.push(line.to_owned());
        assert_eq!(owned(&effects), wanted, "{column:?}");
        assert_eq!(requests(&effects), sent, "{column:?}");
        assert_eq!(kind(mic.phase), spent, "{column:?}");
        assert_eq!(mic.start_failures, START_TRIES, "{column:?}");
        assert_eq!(mic.deadline(), None, "{column:?}");
    }
}

#[test]
fn the_failures_outlive_the_link_and_reach_the_state() {
    let mut mic = Mic {
        start_failures: 2,
        ..mic(phase(Kind::Failed))
    };
    let (fact, now, mut state) = stepped(Event::Lost, Column::AlwaysKnown);
    drop(mic.step(&state, &fact, now));
    assert_eq!(kind(mic.phase), Kind::Stopped);
    mic.show(&mut state);
    assert_eq!(state.microphone.start_failures(), 2);
}

#[test]
fn a_running_marker_with_no_want_meets_the_gate_at_once() {
    let marker = Fact::Link(Signal::Uplink(UplinkState::Running));
    for (worn, quiet, sent, to, line) in [
        (true, false, NOTHING, Kind::Waiting, STOP_WAITS_LINE),
        (true, true, STOP, Kind::StoppingQuiet, Word::Quiet.line()),
        (false, false, STOP, Kind::Stopping, Word::NotWorn.line()),
    ] {
        let mut state = linked(Column::Auto);
        state.ear = EarReport::from_in_ear(worn, false);
        let mut mic = Mic {
            quiet,
            ..Mic::default()
        };
        let effects = mic.step(&state, &marker, SENT);
        assert_eq!(requests(&effects), sent, "{worn} {quiet}");
        assert_eq!(lines(&effects), [line], "{worn} {quiet}");
        assert_eq!(kind(mic.phase), to, "{worn} {quiet}");
    }
}

#[test]
fn a_stop_nobody_asked_for_restores_and_starts_again_only_after_the_want_went() {
    let marker = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    let state = linked(Column::AlwaysKnown);
    let mut unworn = state.clone();
    unworn.ear = EarReport::default();
    let mut cut = mic(phase(Kind::RestoringUnasked));
    let mut mic = mic(phase(Kind::Running));
    let effects = mic.step(&state, &marker, SENT);
    assert_eq!(lines(&effects), [STOPPED_BY_THE_PODS_LINE]);
    assert!(effects.contains(&Effect::Audio(AudioRequest::Restore(RestoreId(0)))));
    assert_eq!(requests(&effects), NOTHING);
    assert_eq!(kind(mic.phase), Kind::RestoringUnasked);
    let restored = Fact::Audio(AudioFact::Restored(RestoreId(0)));
    assert_eq!(requests(&mic.step(&state, &restored, SENT)), NOTHING);
    assert_eq!(kind(mic.phase), Kind::Unasked);
    assert_eq!(requests(&mic.step(&state, &Fact::Changed, SENT)), NOTHING);
    assert_eq!(kind(mic.phase), Kind::Unasked);
    assert_eq!(requests(&mic.step(&unworn, &Fact::Changed, SENT)), NOTHING);
    assert_eq!(kind(mic.phase), Kind::Stopped);
    assert_eq!(requests(&mic.step(&state, &Fact::Changed, SENT)), START);
    let deadline = SENT.saturating_add(Duration::from_millis(RESTORE_WAIT_MS));
    assert_eq!(cut.deadline(), Some(deadline));
    drop(cut.step(
        &state,
        &Fact::Tick,
        deadline.saturating_sub(Duration::from_millis(1)),
    ));
    assert_eq!(kind(cut.phase), Kind::RestoringUnasked);
    assert_eq!(requests(&cut.step(&state, &Fact::Tick, deadline)), NOTHING);
    assert_eq!(kind(cut.phase), Kind::Unasked);
}

#[test]
fn a_recorder_kept_through_a_stop_nobody_asked_for_starts_again_only_once_it_left() {
    let state = linked(Column::Auto);
    let id = OpenId(0);
    let mut mic = Mic::default();
    assert_eq!(requests(&mic.step(&state, &Fact::Changed, SENT)), NOTHING);
    assert_eq!(
        requests(&mic.step(&state, &Fact::Mic(MicFact::Up(id)), SENT)),
        NOTHING
    );
    let linked = Fact::Mic(MicFact::Recording(id, true));
    assert_eq!(requests(&mic.step(&state, &linked, SENT)), START);
    let running = Fact::Link(Signal::Uplink(UplinkState::Running));
    assert_eq!(requests(&mic.step(&state, &running, SENT)), NOTHING);
    let stopped = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    assert_eq!(requests(&mic.step(&state, &stopped, SENT)), NOTHING);
    assert_eq!(kind(mic.phase), Kind::RestoringUnasked);
    let mut left_early = mic;
    let restored = Fact::Audio(AudioFact::Restored(RestoreId(0)));
    assert_eq!(requests(&mic.step(&state, &restored, SENT)), NOTHING);
    assert_eq!(kind(mic.phase), Kind::Unasked);
    let left = Fact::Mic(MicFact::Recording(id, false));
    assert_eq!(requests(&mic.step(&state, &left, SENT)), NOTHING);
    assert_eq!(kind(mic.phase), Kind::Stopped);
    assert_eq!(requests(&mic.step(&state, &linked, SENT)), START);
    // The recorder gone before the restore ends spends the mark there instead.
    assert_eq!(requests(&left_early.step(&state, &left, SENT)), NOTHING);
    assert_eq!(kind(left_early.phase), Kind::Restoring);
    assert_eq!(requests(&left_early.step(&state, &restored, SENT)), NOTHING);
    assert_eq!(kind(left_early.phase), Kind::Stopped);
    assert_eq!(requests(&left_early.step(&state, &linked, SENT)), START);
}

#[test]
fn a_sleep_stops_a_running_uplink_only_through_an_open_gate_and_the_wake_starts_it_again() {
    for (worn, quiet) in [(true, true), (false, false)] {
        let mut state = linked(Column::AlwaysKnown);
        state.ear = EarReport::from_in_ear(worn, worn);
        let mut mic = Mic {
            quiet,
            ..mic(phase(Kind::Running))
        };
        let effects = mic.step(&state, &Fact::Sleep(SleepEdge::Sleeping), SENT);
        assert_eq!(requests(&effects), STOP, "{worn}");
        assert_eq!(lines(&effects), [Word::Sleep.line()], "{worn}");
        let audio = |effect: &Effect| matches!(effect, Effect::Audio(_));
        assert!(!effects.iter().any(audio), "{worn}");
        assert_eq!((kind_of(&mic), mic.deadline()), (Kind::Asleep, None));
        let marker = Fact::Link(Signal::Uplink(UplinkState::Stopped));
        assert_eq!(mic.step(&state, &marker, SENT), [], "{worn}");
        assert_eq!(mic.step(&state, &Fact::Changed, SENT), [], "{worn}");
        let worn = linked(Column::AlwaysKnown);
        let woke = mic.step(&worn, &Fact::Sleep(SleepEdge::Woke), SENT);
        assert_eq!(requests(&woke), START);
    }
}

#[test]
fn a_sleep_over_a_worn_buds_playing_sink_sends_nothing_and_keeps_the_uplink() {
    let state = linked(Column::AlwaysKnown);
    let mut mic = mic(phase(Kind::Running));
    let effects = mic.step(&state, &Fact::Sleep(SleepEdge::Sleeping), SENT);
    assert_eq!((requests(&effects), lines(&effects)), (vec![], vec![]));
    assert_eq!(kind_of(&mic), Kind::Running);
    // The sink going quiet before the suspend opens the gate, and the STOP of the sleep goes out.
    let quiet = sink_in(true, SinkState::Suspended);
    let mut suspended = mic;
    let effects = suspended.step(&state, &quiet, SENT);
    assert_eq!(requests(&effects), STOP);
    assert_eq!(lines(&effects), [Word::Sleep.line()]);
    let audio = |effect: &Effect| matches!(effect, Effect::Audio(_));
    assert!(!effects.iter().any(audio));
    assert_eq!(kind_of(&suspended), Kind::Asleep);
    let woke = mic.step(&state, &Fact::Sleep(SleepEdge::Woke), SENT);
    assert_eq!(woke, []);
    assert_eq!(kind_of(&mic), Kind::Running);
    // Awake again, a sink going quiet sends no STOP of a sleep: the want holds the uplink.
    let effects = mic.step(&state, &quiet, SENT);
    assert_eq!((requests(&effects), lines(&effects)), (vec![], vec![]));
    assert_eq!(kind_of(&mic), Kind::Running);
}

#[test]
fn a_sleep_drops_an_unanswered_start_and_counts_no_failure() {
    let state = linked(Column::AlwaysKnown);
    let mut mic = mic(phase(Kind::Starting));
    let effects = mic.step(&state, &Fact::Sleep(SleepEdge::Sleeping), SENT);
    // The fresh node's own open is the only effect.
    assert!(
        matches!(effects.as_slice(), [Effect::Mic(MicRequest::Open { .. })]),
        "{effects:?}"
    );
    assert_eq!((kind_of(&mic), mic.deadline()), (Kind::Asleep, None));
    assert_eq!(
        mic.step(&state, &Fact::Tick, SENT.saturating_add(LATER)),
        []
    );
    assert_eq!(mic.start_failures, 0);
}

#[test]
fn a_sleep_holds_through_a_stop_and_a_restore_until_the_wake() {
    let state = linked(Column::AlwaysKnown);
    let marker = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    let restored = Fact::Audio(AudioFact::Restored(RestoreId(0)));
    for (from, fact) in [(Kind::StoppingQuiet, &marker), (Kind::Restoring, &restored)] {
        let mut mic = Mic {
            asleep: true,
            ..mic(phase(from))
        };
        assert_eq!(requests(&mic.step(&state, fact, SENT)), NOTHING, "{from:?}");
        assert_eq!(kind_of(&mic), Kind::Asleep, "{from:?}");
        let woke = mic.step(&state, &Fact::Sleep(SleepEdge::Woke), SENT);
        assert_eq!(requests(&woke), START, "{from:?}");
    }
}
