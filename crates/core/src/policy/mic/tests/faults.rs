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
fn a_sleep_stops_a_running_uplink_with_no_restore_and_the_wake_starts_it_again() {
    let state = linked(Column::AlwaysKnown);
    let mut mic = mic(phase(Kind::Running));
    let effects = mic.step(&state, &Fact::Sleep(SleepEdge::Sleeping), SENT);
    assert_eq!(requests(&effects), STOP);
    assert_eq!(lines(&effects), [Word::Sleep.line()]);
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Audio(_)))
    );
    assert_eq!((kind_of(&mic), mic.deadline()), (Kind::Asleep, None));
    let marker = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    assert_eq!(mic.step(&state, &marker, SENT), []);
    assert_eq!(mic.step(&state, &Fact::Changed, SENT), []);
    let woke = mic.step(&state, &Fact::Sleep(SleepEdge::Woke), SENT);
    assert_eq!(requests(&woke), START);
}

#[test]
fn a_sleep_drops_an_unanswered_start_and_counts_no_failure() {
    let state = linked(Column::AlwaysKnown);
    let mut mic = mic(phase(Kind::Starting));
    assert_eq!(
        mic.step(&state, &Fact::Sleep(SleepEdge::Sleeping), SENT),
        []
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
