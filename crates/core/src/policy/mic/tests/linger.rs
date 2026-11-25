//! The linger and the gate: when a due STOP goes out, and whether a restore follows it.

use super::*;

#[test]
fn off_shows_stopping_until_the_owed_stop_completes() {
    let mut state = linked(Column::AlwaysKnown);
    let mut mic = mic(phase(Kind::Running));
    let playing = sink_in(true, SinkState::Running);
    assert_eq!(requests(&mic.step(&state, &playing, SENT)), NOTHING);
    mic.show(&mut state);
    assert_eq!(state.microphone.code(), MicCode::Live);

    state.microphone.set_preference(MicPref::Off);
    let written = SENT.saturating_add(SOON);
    assert_eq!(
        requests(&mic.step(&state, &Fact::Changed, written)),
        NOTHING
    );
    assert_eq!(hold(&mic).deadline(), None);
    mic.show(&mut state);
    assert_eq!(state.microphone.code(), MicCode::Stopping);

    let suspended = written.saturating_add(LATER);
    let quiet = sink_in(true, SinkState::Suspended);
    assert_eq!(requests(&mic.step(&state, &quiet, suspended)), STOP);
    mic.show(&mut state);
    assert_eq!(state.microphone.code(), MicCode::Stopping);

    let stopped = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    assert_eq!(requests(&mic.step(&state, &stopped, suspended)), NOTHING);
    mic.show(&mut state);
    assert_eq!(state.microphone.code(), MicCode::Off);
}

#[test]
fn the_gate_lets_a_due_stop_out_only_with_no_bud_worn_or_a_quiet_sink() {
    let absent = Fact::Audio(AudioFact::Sink(None));
    let down = Fact::Audio(AudioFact::Down);
    for (worn, fact, to) in [
        (true, &Fact::Changed, Kind::Waiting),
        (true, &sink_in(true, SinkState::Running), Kind::Waiting),
        (true, &sink_in(true, SinkState::Idle), Kind::Waiting),
        (true, &down, Kind::Waiting),
        (
            true,
            &sink_in(true, SinkState::Suspended),
            Kind::StoppingQuiet,
        ),
        (true, &absent, Kind::StoppingQuiet),
        (false, &Fact::Changed, Kind::Stopping),
        (false, &sink_in(true, SinkState::Running), Kind::Stopping),
    ] {
        let mut state = linked(Column::Auto);
        state.ear = EarReport::from_in_ear(worn, false);
        let unworn = (!worn).then_some(LEFT);
        let mut mic = mic(Phase::Running(
            Hold::Waiting { unworn },
            watch(),
            Ownership::Owned,
        ));
        let effects = mic.step(&state, fact, SENT);
        assert_eq!(kind(mic.phase), to, "{worn} {fact:?}");
        let sent = if to == Kind::Waiting { NOTHING } else { STOP };
        assert_eq!(requests(&effects), sent, "{worn} {fact:?}");
    }
}

#[test]
fn a_quiet_sink_ends_the_linger_in_a_stop_and_a_server_gone_holds_it() {
    let end = SENT.saturating_add(Duration::from_millis(UPLINK_LINGER_MS));
    let state = linked(Column::Auto);
    let quiet = sink_in(true, SinkState::Suspended);
    let down = Fact::Audio(AudioFact::Down);
    let playing = sink_in(true, SinkState::Running);
    for (facts, sent, to) in [
        (vec![quiet.clone()], STOP, Kind::StoppingQuiet),
        (vec![quiet.clone(), down], NOTHING, Kind::Waiting),
        (vec![quiet, playing], NOTHING, Kind::Waiting),
    ] {
        let mut mic = mic(phase(Kind::Lingering));
        for fact in &facts {
            assert_eq!(requests(&mic.step(&state, fact, SENT)), NOTHING);
        }
        assert_eq!(kind(mic.phase), Kind::Lingering, "{facts:?}");
        assert_eq!(
            requests(&mic.step(&state, &Fact::Tick, end)),
            sent,
            "{facts:?}"
        );
        assert_eq!(kind(mic.phase), to, "{facts:?}");
    }
}

#[test]
fn the_linger_counts_from_the_want_and_the_grace_from_the_last_bud() {
    let linger = Duration::from_millis(UPLINK_LINGER_MS);
    let grace = Duration::from_millis(BUD_GRACE_MS);
    let mut mic = mic(phase(Kind::Running));
    let mut state = linked(Column::Auto);
    drop(mic.step(&state, &Fact::Changed, SENT));
    assert_eq!(hold(&mic).deadline(), Some(SENT.saturating_add(linger)));
    let left = SENT.saturating_add(SOON);
    state.ear = EarReport::default();
    drop(mic.step(&state, &Fact::Changed, left));
    assert_eq!(mic.deadline(), Some(left.saturating_add(grace)));
    let early = left
        .saturating_add(grace)
        .saturating_sub(Duration::from_millis(1));
    assert_eq!(requests(&mic.step(&state, &Fact::Tick, early)), NOTHING);
    assert_eq!(kind(mic.phase), Kind::Lingering);
    state.ear = EarReport::from_in_ear(true, false);
    drop(mic.step(&state, &Fact::Changed, left.saturating_add(SOON)));
    assert_eq!(hold(&mic).deadline(), Some(SENT.saturating_add(linger)));
    let early = SENT
        .saturating_add(linger)
        .saturating_sub(Duration::from_millis(1));
    assert_eq!(
        lines(&mic.step(&state, &Fact::Tick, early)),
        Vec::<&str>::new()
    );
    assert_eq!(kind(mic.phase), Kind::Lingering);
    let effects = mic.step(&state, &Fact::Tick, SENT.saturating_add(linger));
    assert_eq!(lines(&effects), [STOP_WAITS_LINE]);
    assert_eq!(
        lines(&mic.step(&state, &Fact::Changed, SENT)),
        Vec::<&str>::new()
    );
}

#[test]
fn a_bud_taken_out_while_the_stop_waits_gets_the_grace() {
    let grace = Duration::from_millis(BUD_GRACE_MS);
    for column in [Column::Auto, Column::Off, Column::Exit] {
        let mut mic = Mic {
            exiting: column == Column::Exit,
            ..mic(phase(Kind::Waiting))
        };
        let mut state = linked(column);
        state.ear = EarReport::default();
        let effects = mic.step(&state, &Fact::Changed, SENT);
        assert_eq!(requests(&effects), NOTHING, "{column:?}");
        assert_eq!(kind(mic.phase), Kind::Waiting, "{column:?}");
        let due = SENT.saturating_add(grace);
        assert_eq!(mic.deadline(), Some(due), "{column:?}");
        let early = due.saturating_sub(Duration::from_millis(1));
        let effects = mic.step(&state, &Fact::Tick, early);
        assert_eq!(requests(&effects), NOTHING, "{column:?}");
        assert_eq!(kind(mic.phase), Kind::Waiting, "{column:?}");
        let effects = mic.step(&state, &Fact::Tick, due);
        assert_eq!(requests(&effects), STOP, "{column:?}");
        assert_eq!(kind(mic.phase), Kind::Stopping, "{column:?}");
    }
}

#[test]
fn each_stop_logs_the_word_of_the_gate_and_only_a_quiet_one_skips_the_restore() {
    for column in [Column::Auto, Column::Off, Column::Exit] {
        for word in [Word::Quiet, Word::NotWorn] {
            let mut state = linked(column);
            let (fact, unworn) = if word == Word::Quiet {
                (sink_in(true, SinkState::Suspended), None)
            } else {
                state.ear = EarReport::default();
                (Fact::Changed, Some(LEFT))
            };
            let mut mic = Mic {
                exiting: column == Column::Exit,
                ..mic(Phase::Running(
                    Hold::Waiting { unworn },
                    watch(),
                    Ownership::Owned,
                ))
            };
            assert_eq!(
                lines(&mic.step(&state, &fact, SENT)),
                [word.line()],
                "{column:?} {word:?}"
            );
            let marker = Fact::Link(Signal::Uplink(UplinkState::Stopped));
            let effects = mic.step(&state, &marker, SENT);
            let restore = effects.contains(&Effect::Audio(AudioRequest::Restore(RestoreId(0))));
            assert_eq!(restore, word != Word::Quiet, "{column:?} {word:?}");
        }
    }
}

#[test]
fn off_and_the_exit_owe_the_stop_with_no_linger_and_only_through_the_gate() {
    let lingering = Hold::Lingering {
        since: SENT,
        unworn: None,
    };
    for column in [Column::Off, Column::Exit] {
        for from in [Hold::Wanted, Hold::Late, lingering] {
            for (worn, quiet, sent, to, line) in [
                (true, false, NOTHING, Kind::Waiting, STOP_OWED_PLAYING_LINE),
                (true, true, STOP, Kind::StoppingQuiet, Word::Quiet.line()),
                (false, false, STOP, Kind::Stopping, Word::NotWorn.line()),
            ] {
                let mut state = linked(column);
                state.ear = EarReport::from_in_ear(worn, false);
                let mut mic = Mic {
                    exiting: column == Column::Exit,
                    audio: Audio {
                        quiet,
                        ..Audio::default()
                    },
                    ..mic(Phase::Running(from, watch(), Ownership::Owned))
                };
                let effects = mic.step(&state, &Fact::Changed, SENT);
                let cell = format!("{column:?} {from:?} {worn} {quiet}");
                assert_eq!(requests(&effects), sent, "{cell}");
                assert_eq!(kind(mic.phase), to, "{cell}");
                assert_eq!(lines(&effects), [line], "{cell}");
                assert!(!effects.contains(&Effect::Exited), "{cell}");
            }
        }
    }
}
