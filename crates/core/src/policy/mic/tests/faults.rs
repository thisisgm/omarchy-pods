//! Support tests for the faults the uplink suites drive through the built daemon: the tries, the markers nobody asked for, a sleep and the failures counted.

use super::*;
use crate::policy::audio::server_after;

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
fn a_channel_end_during_an_owed_stop_still_restores() {
    for from in [Kind::Starting, Kind::Owed, Kind::Stopping, Kind::Running] {
        let (effects, after) = run(from, Event::Down, Column::Off);
        assert_eq!(
            effects,
            [Effect::Audio(AudioRequest::Restore(RestoreId(0)))],
            "{from:?}"
        );
        assert_eq!(kind(after.phase), Kind::Restoring, "{from:?}");
    }

    for (from, event) in [
        (Kind::StoppingQuiet, Event::Down),
        (Kind::Stopping, Event::Lost),
    ] {
        let (effects, after) = run(from, event, Column::Off);
        assert_eq!(effects, [], "{from:?}, {event:?}");
        assert_eq!(kind(after.phase), Kind::Stopped, "{from:?}, {event:?}");
    }

    let (down, at, state) = stepped(Event::Down, Column::Off);
    for from in [Kind::Starting, Kind::Owed, Kind::Running] {
        for (quiet, asleep) in [(true, false), (false, true)] {
            let mut mic = Mic {
                audio: Audio {
                    quiet,
                    ..Audio::default()
                },
                asleep,
                ..mic(phase(from))
            };
            assert_eq!(
                mic.step(&state, &down, at),
                [],
                "{from:?}, {quiet}, {asleep}"
            );
            assert_eq!(
                kind(mic.phase),
                Kind::Stopped,
                "{from:?}, {quiet}, {asleep}"
            );
        }
    }
}

#[test]
fn always_waits_for_the_first_card_profile() {
    let state = State {
        audio_server: AudioServer::Up,
        ..linked(Column::AlwaysKnown)
    };
    let headset = Fact::Audio(AudioFact::Headset(true));
    let playback = Fact::Audio(AudioFact::Headset(false));

    let mut mic = Mic::default();
    assert_eq!(requests(&mic.step(&state, &Fact::Changed, SENT)), NOTHING);
    assert_eq!(requests(&mic.step(&state, &playback, SENT)), START);

    let mut mic = Mic::default();
    assert_eq!(requests(&mic.step(&state, &headset, SENT)), NOTHING);
    assert_eq!(requests(&mic.step(&state, &Fact::Changed, SENT)), NOTHING);
    assert_eq!(requests(&mic.step(&state, &playback, SENT)), START);
}

fn reconnect_audio(mic: &mut Mic, state: &mut State, at: Duration) {
    for fact in [Fact::Audio(AudioFact::Down), Fact::Audio(AudioFact::Up)] {
        state.audio_server = server_after(state.audio_server, &fact);
        assert_eq!(requests(&mic.step(state, &fact, at)), NOTHING);
    }
}

#[test]
fn a_start_retry_waits_for_the_card_profile_after_audio_reconnects() {
    let mut state = State {
        audio_server: AudioServer::Up,
        ..linked(Column::AlwaysKnown)
    };
    let mut mic = Mic::default();
    let playback = Fact::Audio(AudioFact::Headset(false));
    assert_eq!(requests(&mic.step(&state, &playback, SENT)), START);
    reconnect_audio(&mut mic, &mut state, SENT.saturating_add(SOON));

    let deadline = mic.deadline().unwrap();
    let unanswered = mic.step(&state, &Fact::Tick, deadline);
    assert_eq!(requests(&unanswered), NOTHING);
    assert_eq!(kind(mic.phase), Kind::Stopped);

    let resumed = mic.step(&state, &playback, deadline.saturating_add(SOON));
    assert_eq!(requests(&resumed), START);
    assert_eq!(owned(&resumed), [start_sent_line(Try::FIRST.number())]);
}

#[test]
fn a_stall_start_waits_for_the_card_profile_after_audio_reconnects() {
    let mut state = State {
        audio_server: AudioServer::Up,
        ..linked(Column::AlwaysKnown)
    };
    let mut mic = Mic::default();
    let playback = Fact::Audio(AudioFact::Headset(false));
    let running = Fact::Link(Signal::Uplink(UplinkState::Running));
    let no_packets = Fact::Mic(MicFact::Heard { packets: 0 });
    assert_eq!(requests(&mic.step(&state, &playback, SENT)), START);
    assert_eq!(requests(&mic.step(&state, &running, SENT)), NOTHING);
    assert_eq!(requests(&mic.step(&state, &no_packets, SENT)), NOTHING);
    reconnect_audio(&mut mic, &mut state, SENT.saturating_add(SOON));

    let deadline = mic.deadline().unwrap();
    assert_eq!(requests(&mic.step(&state, &no_packets, deadline)), NOTHING);
    assert_eq!(mic.start_failures, 0);

    assert_eq!(requests(&mic.step(&state, &playback, deadline)), NOTHING);
    let next_read = mic.deadline().unwrap();
    assert_eq!(requests(&mic.step(&state, &no_packets, next_read)), START);
    assert_eq!(mic.start_failures, 1);
}

#[test]
fn link_loss_and_audio_down_discard_the_last_card_profile() {
    let (lost, at, _) = stepped(Event::Lost, Column::AlwaysKnown);
    let relink = Fact::Link(Signal::Up { device: device() });
    let down = Fact::Audio(AudioFact::Down);
    let up = Fact::Audio(AudioFact::Up);
    let playback = Fact::Audio(AudioFact::Headset(false));
    let worn = linked(Column::AlwaysKnown);

    for (case, outage) in [
        ("link loss", &[(lost, false), (relink, true)][..]),
        ("audio down", &[(down, true)][..]),
    ] {
        let mut state = State {
            audio_server: AudioServer::Up,
            ear: EarReport::default(),
            ..worn.clone()
        };
        let mut mic = Mic::default();
        assert_eq!(requests(&mic.step(&state, &playback, SENT)), NOTHING);
        for (fact, connected) in outage {
            state.connected = *connected;
            state.audio_server = server_after(state.audio_server, fact);
            assert_eq!(requests(&mic.step(&state, fact, at)), NOTHING, "{case}");
        }
        state.audio_server = server_after(state.audio_server, &up);
        assert_eq!(requests(&mic.step(&state, &up, at)), NOTHING, "{case}");
        state.ear = worn.ear;
        assert_eq!(
            requests(&mic.step(&state, &Fact::Changed, at)),
            NOTHING,
            "{case}"
        );
        assert_eq!(requests(&mic.step(&state, &playback, at)), START, "{case}");
    }
}

#[test]
fn auto_clears_an_unasked_stop_when_capture_ends_with_the_card_profile_unread() {
    let state = State {
        audio_server: AudioServer::Up,
        ..linked(Column::Auto)
    };
    let capture = Fact::Audio(AudioFact::Hint(true));
    let no_capture = Fact::Audio(AudioFact::Hint(false));
    for (from, to) in [
        (Kind::Unasked, Kind::Stopped),
        (Kind::RestoringUnasked, Kind::Restoring),
    ] {
        let mut mic = mic(phase(from));
        assert_eq!(requests(&mic.step(&state, &capture, SENT)), NOTHING);
        assert_eq!(mic.phase, phase(from), "{from:?}");
        assert_eq!(requests(&mic.step(&state, &no_capture, SENT)), NOTHING);
        assert_eq!(mic.phase, phase(to), "{from:?}");
    }
}

#[test]
fn a_fast_capture_hint_starts_the_uplink_while_dictation_arms() {
    let state = State {
        audio_server: AudioServer::Up,
        ..linked(Column::Auto)
    };
    let mut mic = Mic::default();
    let block = Fact::Audio(AudioFact::Headset(true));
    let hint = Fact::Audio(AudioFact::Hint(true));
    let unblock = Fact::Audio(AudioFact::Headset(false));
    let mut dictation = Dictation::default();
    drop(dictation.step(&state, &Fact::Link(Signal::Up { device: device() }), SENT));
    assert_eq!(dictation.code(&state), MicDictation::Arming);

    let blocked = mic.step_with_dictation(&state, &block, SENT, &dictation);
    let hinted = mic.step_with_dictation(&state, &hint, SENT, &dictation);
    let unblocked = mic.step_with_dictation(&state, &unblock, SENT, &dictation);

    assert_eq!(requests(&blocked), NOTHING);
    assert_eq!(requests(&hinted), NOTHING);
    assert_eq!(requests(&unblocked), START);
}

#[test]
fn headset_and_hint_facts_are_ignored_while_the_audio_server_is_idle() {
    for server in [AudioServer::Idle, AudioServer::Up] {
        let state = State {
            audio_server: server,
            ..linked(Column::Auto)
        };
        let mut mic = Mic::default();

        let hinted = mic.step(&state, &Fact::Audio(AudioFact::Hint(true)), SENT);
        let blocked = mic.step(&state, &Fact::Audio(AudioFact::Headset(true)), SENT);

        let kept = server == AudioServer::Up;
        assert_eq!(mic.audio.hint, kept, "{server:?}");
        assert_eq!(mic.node.blocked(), kept, "{server:?}");
        if !kept {
            assert_eq!(
                requests(&hinted),
                NOTHING,
                "a hint while idle sends nothing"
            );
            assert_eq!(
                requests(&blocked),
                NOTHING,
                "a block while idle sends nothing"
            );
        }
    }
}

#[test]
fn a_channel_end_while_headset_is_active_skips_the_restore() {
    let (down, at, mut state) = stepped(Event::Down, Column::Off);
    state.audio_server = AudioServer::Up;
    let connected = State {
        connected: true,
        ..state.clone()
    };
    let headset = Fact::Audio(AudioFact::Headset(true));
    for from in [Kind::Starting, Kind::Stopping, Kind::Running] {
        let mut mic = mic(phase(from));
        drop(mic.step(&connected, &headset, SENT));
        assert_eq!(mic.step(&state, &down, at), [], "{from:?}");
        assert_eq!(kind(mic.phase), Kind::Stopped, "{from:?}");
    }
}

#[test]
fn a_channel_end_keeps_the_pending_restore_until_it_answers() {
    let id = RestoreId(1);
    let mut mic = Mic {
        phase: Phase::Restoring {
            since: SENT,
            id,
            spent: Some(Spent::Unasked),
        },
        unconfirmed_stop: true,
        exiting: true,
        ..Mic::default()
    };
    let (down, at, state) = stepped(Event::Down, Column::Exit);
    let deadline = mic.deadline();
    assert_eq!(mic.step(&state, &down, at), []);
    assert!(!mic.unconfirmed_stop);
    assert_eq!(
        mic.phase,
        Phase::Restoring {
            since: SENT,
            id,
            spent: None,
        }
    );
    assert_eq!(mic.deadline(), deadline);

    let earlier = Fact::Audio(AudioFact::Restored(RestoreId(0)));
    assert_eq!(mic.step(&state, &earlier, at), []);
    let restored = Fact::Audio(AudioFact::Restored(id));
    assert_eq!(mic.step(&state, &restored, at), [Effect::Exited]);
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
            audio: Audio {
                quiet,
                ..Audio::default()
            },
            ..Mic::default()
        };
        let effects = mic.step(&state, &marker, SENT);
        assert_eq!(requests(&effects), sent, "{worn} {quiet}");
        assert_eq!(lines(&effects), [line], "{worn} {quiet}");
        assert_eq!(kind(mic.phase), to, "{worn} {quiet}");
    }
}

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
    assert_no_restore(&mic.step(&state, &Fact::Tick, overdue));
    assert_no_restore(&mic.step(&state, &stopped, overdue.saturating_add(SOON)));
}

#[test]
fn unowned_uplink_stopped_from_late_skips_restore() {
    let state = linked(Column::Auto);
    let stopped = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    let mut mic = Mic {
        phase: Phase::Running(Hold::Late, watch()),
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

#[test]
fn a_sink_that_wakes_under_a_quiet_stop_owes_a_restore() {
    let (stopped, at, state) = stepped(Event::StoppedMarker, Column::Off);
    let quiet_stop = Mic {
        audio: Audio {
            quiet: true,
            ..Audio::default()
        },
        ..mic(phase(Kind::StoppingQuiet))
    };
    for (name, fact) in [
        ("running", sink_in(true, SinkState::Running)),
        ("idle", sink_in(true, SinkState::Idle)),
        ("audio server down", Fact::Audio(AudioFact::Down)),
    ] {
        let mut promoted = quiet_stop;
        assert_eq!(promoted.step(&state, &fact, at), [], "{name}");
        assert_eq!(kind(promoted.phase), Kind::Stopping, "{name}");
        for answer in [Event::StoppedMarker, Event::Deadline] {
            let mut mic = promoted;
            let (fact, at, _) = stepped(answer, Column::Off);
            let effects = mic.step(&state, &fact, at);
            assert!(
                effects.contains(&Effect::Audio(AudioRequest::Restore(RestoreId(0)))),
                "{name}, {answer:?}: {effects:?}"
            );
            assert_eq!(kind(mic.phase), Kind::Restoring, "{name}, {answer:?}");
        }
    }

    let mut still_quiet = quiet_stop;
    let quiet = sink_in(true, SinkState::Suspended);
    assert_eq!(still_quiet.step(&state, &quiet, at), []);
    assert_eq!(kind(still_quiet.phase), Kind::StoppingQuiet);
    let effects = still_quiet.step(&state, &stopped, at);
    assert!(
        matches!(effects.as_slice(), [Effect::Log(_)]),
        "{effects:?}"
    );
    assert_eq!(kind(still_quiet.phase), Kind::Stopped);
}

#[test]
fn a_running_marker_while_restoring_cancels_the_restore() {
    let (running, at, state) = stepped(Event::RunningMarker, Column::AlwaysKnown);
    let mut mic = Mic {
        armed: Some(SENT),
        ..mic(phase(Kind::RestoringUnasked))
    };
    let effects = mic.step(&state, &running, at);
    assert_eq!(kind(mic.phase), Kind::Running);
    assert!(effects.contains(&Effect::Audio(AudioRequest::CancelRestore)));
    assert_eq!(requests(&effects), NOTHING);
    assert_eq!(mic.armed, None);

    let restored = Fact::Audio(AudioFact::Restored(RestoreId(0)));
    assert_eq!(mic.step(&state, &restored, at), []);
    assert_eq!(kind(mic.phase), Kind::Running);
    let quiet = sink_in(true, SinkState::Suspended);
    assert_eq!(requests(&mic.step(&state, &quiet, at)), NOTHING);
    let off = linked(Column::Off);
    assert_eq!(requests(&mic.step(&off, &Fact::Changed, at)), STOP);
    assert_eq!(kind(mic.phase), Kind::StoppingQuiet);
}

#[test]
fn a_sleep_wake_start_answered_during_restore_tracks_the_uplink() {
    let (_, mut mic) = run(Kind::Starting, Event::Sleep, Column::AlwaysKnown);
    assert_eq!(kind_of(&mic), Kind::Asleep);
    let (wake, at, state) = stepped(Event::Wake, Column::AlwaysKnown);
    assert_eq!(requests(&mic.step(&state, &wake, at)), START);
    assert_eq!(kind(mic.phase), Kind::Starting);

    let running = Fact::Link(Signal::Uplink(UplinkState::Running));
    let stopped = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    drop(mic.step(&state, &running, at));
    assert_eq!(kind(mic.phase), Kind::Running);
    drop(mic.step(&state, &stopped, at));
    assert_eq!(kind(mic.phase), Kind::RestoringUnasked);
    let answered = mic.step(&state, &running, at);
    assert_eq!(kind(mic.phase), Kind::Running);
    assert!(answered.contains(&Effect::Audio(AudioRequest::CancelRestore)));
}

#[test]
fn a_stop_confirmed_after_its_deadline_restores_once_more() {
    let (stopped, connected_at, state) = stepped(Event::StoppedMarker, Column::Off);
    let mut fresh = Mic::default();
    assert_eq!(
        fresh.step(&state, &stopped, connected_at),
        [],
        "initial marker"
    );

    let (first, restoring) = run(Kind::Stopping, Event::Deadline, Column::Off);
    let first_id = RestoreId(0);
    let next_id = RestoreId(1);
    assert!(first.contains(&Effect::Audio(AudioRequest::Restore(first_id))));
    assert_eq!(kind(restoring.phase), Kind::Restoring);
    let late = SENT.saturating_add(LATER).saturating_add(SOON);
    let first_restored = Fact::Audio(AudioFact::Restored(first_id));
    let next_restored = Fact::Audio(AudioFact::Restored(next_id));
    for from in [Kind::Stopped, Kind::Restoring] {
        let mut mic = restoring;
        if from == Kind::Stopped {
            assert_eq!(mic.step(&state, &first_restored, late), []);
        }
        assert_eq!(kind(mic.phase), from);
        assert_eq!(
            mic.step(&state, &stopped, late),
            [Effect::Audio(AudioRequest::Restore(next_id))],
            "{from:?}"
        );
        assert_eq!(mic.step(&state, &stopped, late), [], "duplicate marker");
        assert_eq!(mic.step(&state, &first_restored, late), []);
        assert_eq!(kind(mic.phase), Kind::Restoring);
        assert_eq!(mic.step(&state, &next_restored, late), []);
        assert_eq!(kind(mic.phase), Kind::Stopped);
    }

    let mut quiet = Mic {
        audio: Audio {
            quiet: true,
            ..Audio::default()
        },
        ..mic(phase(Kind::StoppingQuiet))
    };
    let due = SENT.saturating_add(Duration::from_millis(STOP_ANSWER_MS));
    let effects = quiet.step(&state, &Fact::Tick, due);
    assert_eq!(effects, [log(STOP_UNCONFIRMED_LINE)]);
    assert_eq!(quiet.step(&state, &stopped, late), [], "quiet sink");
    assert_eq!(kind(quiet.phase), Kind::Stopped);

    let mut asleep = restoring;
    drop(asleep.step(&state, &Fact::Sleep(SleepEdge::Sleeping), late));
    assert_eq!(asleep.step(&state, &stopped, late), [], "asleep");
    assert_eq!(kind(asleep.phase), Kind::Restoring);
}

#[test]
fn a_late_stop_while_headset_is_active_keeps_the_existing_restore() {
    let (mut mic, at) = stop_past_its_deadline();
    let (stopped, _, mut state) = stepped(Event::StoppedMarker, Column::Off);
    state.audio_server = AudioServer::Up;
    let headset = Fact::Audio(AudioFact::Headset(true));
    drop(mic.step(&state, &headset, at));
    assert_eq!(mic.step(&state, &stopped, at), []);
    assert!(!mic.unconfirmed_stop);
    assert_eq!(kind(mic.phase), Kind::Restoring);
    let restored = Fact::Audio(AudioFact::Restored(RestoreId(0)));
    assert_eq!(mic.step(&state, &restored, at), []);
    assert_eq!(kind(mic.phase), Kind::Stopped);
}

#[test]
fn a_sink_that_wakes_under_a_quiet_stop_while_headset_is_active_skips_the_restore() {
    let (stopped, at, mut state) = stepped(Event::StoppedMarker, Column::Off);
    state.audio_server = AudioServer::Up;
    let mut mic = Mic {
        audio: Audio {
            quiet: true,
            ..Audio::default()
        },
        ..mic(phase(Kind::StoppingQuiet))
    };
    let headset = Fact::Audio(AudioFact::Headset(true));
    drop(mic.step(&state, &headset, SENT));
    let awake = sink_in(true, SinkState::Running);
    assert_eq!(mic.step(&state, &awake, at), []);
    assert_eq!(kind(mic.phase), Kind::Stopping);
    let effects = mic.step(&state, &stopped, at);
    assert!(matches!(effects.as_slice(), [Effect::Log(_)]));
    assert_eq!(kind(mic.phase), Kind::Stopped);
}

fn stop_past_its_deadline() -> (Mic, Duration) {
    let (effects, mic) = run(Kind::Stopping, Event::Deadline, Column::Off);
    assert_eq!(
        effects,
        [
            log(STOP_UNCONFIRMED_LINE),
            Effect::Audio(AudioRequest::Restore(RestoreId(0))),
        ]
    );
    assert!(mic.unconfirmed_stop);
    (mic, SENT.saturating_add(LATER).saturating_add(SOON))
}

#[test]
fn losing_the_link_discards_an_unconfirmed_stop() {
    let (stopped, _, state) = stepped(Event::StoppedMarker, Column::Off);
    let restored = Fact::Audio(AudioFact::Restored(RestoreId(0)));
    let (mut mic, at) = stop_past_its_deadline();
    assert_eq!(mic.step(&state, &restored, at), []);

    for event in [Event::Down, Event::Lost] {
        let mut mic = mic;
        let (lost, _, disconnected) = stepped(event, Column::Off);
        assert_eq!(mic.step(&disconnected, &lost, at), [], "{event:?}");
        assert_eq!(mic.step(&disconnected, &stopped, at), [], "{event:?}");
    }
}

#[test]
fn a_new_start_discards_an_unconfirmed_stop() {
    let (stopped, _, off) = stepped(Event::StoppedMarker, Column::Off);
    let restored = Fact::Audio(AudioFact::Restored(RestoreId(0)));
    let (mut mic, at) = stop_past_its_deadline();
    assert_eq!(mic.step(&off, &restored, at), []);

    let always = linked(Column::AlwaysKnown);
    assert_eq!(requests(&mic.step(&always, &Fact::Changed, at)), START);
    let deadline = at.saturating_add(Duration::from_millis(START_ANSWER_MS));
    drop(mic.step(&off, &Fact::Tick, deadline));
    assert_eq!(kind(mic.phase), Kind::Stopped);
    assert_eq!(mic.step(&off, &stopped, deadline), []);
}

#[test]
fn a_running_marker_discards_an_unconfirmed_stop() {
    let (running, _, always) = stepped(Event::RunningMarker, Column::AlwaysKnown);
    let (stopped_marker, _, off) = stepped(Event::StoppedMarker, Column::Off);
    let restored = Fact::Audio(AudioFact::Restored(RestoreId(0)));
    let (restoring, at) = stop_past_its_deadline();
    let mut stopped = restoring;
    assert_eq!(stopped.step(&off, &restored, at), []);
    // A normal START clears the flag first, so seed Starting to test the marker's own clear.
    let starting = Mic {
        phase: Phase::Starting {
            since: at,
            tried: Try::FIRST,
            owed: false,
        },
        ..stopped
    };

    for mut mic in [starting, stopped, restoring] {
        let from = kind(mic.phase);
        drop(mic.step(&always, &running, at));
        assert_eq!(kind(mic.phase), Kind::Running, "{from:?}");
        let effects = mic.step(&always, &stopped_marker, at);
        assert_eq!(lines(&effects), [STOPPED_BY_THE_PODS_LINE], "{from:?}");
        if from == Kind::Starting {
            assert!(effects.contains(&Effect::Audio(AudioRequest::Restore(RestoreId(1)))));
        } else {
            assert_no_restore(&effects);
        }
        assert_eq!(mic.step(&always, &stopped_marker, at), [], "{from:?}");
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
            audio: Audio {
                quiet,
                ..Audio::default()
            },
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
