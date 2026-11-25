//! The stall check of a running uplink: the count of audio packets read at each tick, a count that did not move sending START again while a try is left, and the first audio after a START.

use super::*;

const STALL: Duration = Duration::from_millis(UPLINK_STALL_MS);
const COUNT: u64 = 5;

fn heard(packets: u64) -> Fact {
    Fact::Mic(MicFact::Heard { packets })
}

/// `windows` stall windows after `SENT`.
fn after(windows: u32) -> Duration {
    SENT.saturating_add(STALL.saturating_mul(windows))
}

/// No line and no request, the node's own effects apart.
fn quiet(effects: &[Effect]) -> bool {
    lines(effects).is_empty() && requests(effects).is_empty()
}

fn watch_of(mic: &Mic) -> Watch {
    let Phase::Running(_, watch) = mic.phase else {
        panic!("not running: {:?}", mic.phase);
    };
    watch
}

/// A running uplink with both buds worn, its first count read at `SENT`.
fn counted() -> (State, Mic) {
    let state = linked(Column::AlwaysKnown);
    let mut mic = mic(phase(Kind::Running));
    assert!(quiet(&mic.step(&state, &heard(COUNT), SENT)));
    (state, mic)
}

#[test]
fn a_count_that_moved_reads_again_one_window_later_and_sends_nothing() {
    let (state, mut mic) = counted();
    assert_eq!(mic.deadline(), Some(after(1)));
    let more = COUNT.saturating_add(1);
    assert!(quiet(&mic.step(&state, &heard(more), after(1))));
    assert_eq!(mic.deadline(), Some(after(2)));
    assert_eq!(mic.start_failures, 0);
}

#[test]
fn a_count_read_before_its_window_ends_changes_nothing() {
    let (state, mut mic) = counted();
    let before = mic;
    let early = after(1).saturating_sub(Duration::from_millis(1));
    assert!(quiet(&mic.step(&state, &heard(COUNT), early)));
    assert_eq!(mic, before);
}

#[test]
fn a_still_count_logs_the_stall_counts_a_failure_and_sends_the_next_try() {
    let (state, mut mic) = counted();
    let effects = mic.step(&state, &heard(COUNT), after(1));
    let stalled = stalled_line(STALL.as_millis());
    let sent = start_sent_line(2);
    assert_eq!(lines(&effects), [stalled.as_str(), sent.as_str()]);
    assert_eq!(requests(&effects), [UplinkRequest::Start]);
    assert_eq!(mic.start_failures, 1);
    assert_eq!(mic.armed, Some(after(1)));
    assert_eq!(kind(mic.phase), Kind::Running);
    assert_eq!(mic.deadline(), Some(after(2)));
}

#[test]
fn a_stall_after_the_last_try_stops_the_uplink_restores_the_sink_and_spends_the_tries() {
    let (state, mut mic) = counted();
    for windows in 1..START_TRIES {
        let effects = mic.step(&state, &heard(COUNT), after(windows));
        let stalled = stalled_line(STALL.saturating_mul(windows).as_millis());
        let sent = start_sent_line(windows.saturating_add(1));
        assert_eq!(lines(&effects), [stalled.as_str(), sent.as_str()]);
    }
    let effects = mic.step(&state, &heard(COUNT), after(START_TRIES));
    let stalled = stalled_line(STALL.saturating_mul(START_TRIES).as_millis());
    assert_eq!(lines(&effects), [stalled.as_str(), FAILED_LINE]);
    assert_eq!(requests(&effects), [UplinkRequest::Stop]);
    assert_eq!(kind(mic.phase), Kind::Stopping);
    assert_eq!(mic.start_failures, START_TRIES);
    let stopped = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    let effects = mic.step(&state, &stopped, after(START_TRIES).saturating_add(SOON));
    assert!(effects.contains(&Effect::Audio(AudioRequest::Restore(RestoreId(0)))));
    let restored = Fact::Audio(AudioFact::Restored(RestoreId(0)));
    let effects = mic.step(&state, &restored, after(START_TRIES).saturating_add(SOON));
    assert_eq!(requests(&effects), NOTHING);
    assert_eq!(kind(mic.phase), Kind::Failed);
}

/// A running uplink whose last stall has just spent the tries.
fn spent_by_a_stall() -> (State, Mic) {
    let (state, mut mic) = counted();
    for windows in 1..=START_TRIES {
        drop(mic.step(&state, &heard(COUNT), after(windows)));
    }
    assert_eq!(kind(mic.phase), Kind::Stopping);
    (state, mic)
}

#[test]
fn a_stop_and_a_restore_unanswered_after_the_last_stall_still_read_failed() {
    let (state, mut mic) = spent_by_a_stall();
    let early = Duration::from_millis(1);
    let unconfirmed = after(START_TRIES).saturating_add(Duration::from_millis(STOP_ANSWER_MS));
    let before = unconfirmed.saturating_sub(early);
    assert!(quiet(&mic.step(&state, &Fact::Tick, before)));
    assert_eq!(kind(mic.phase), Kind::Stopping);
    let effects = mic.step(&state, &Fact::Tick, unconfirmed);
    assert!(lines(&effects).contains(&STOP_UNCONFIRMED_LINE));
    assert!(effects.contains(&Effect::Audio(AudioRequest::Restore(RestoreId(0)))));
    assert_eq!(kind(mic.phase), Kind::Restoring);
    let unanswered = unconfirmed.saturating_add(Duration::from_millis(RESTORE_WAIT_MS));
    let almost = unanswered.saturating_sub(early);
    assert!(quiet(&mic.step(&state, &Fact::Tick, almost)));
    assert_eq!(kind(mic.phase), Kind::Restoring);
    drop(mic.step(&state, &Fact::Tick, unanswered));
    assert_eq!(kind(mic.phase), Kind::Failed);
}

/// A running uplink one stall into its tries, the START of try 2 out since `after(1)`.
fn retried() -> (State, Mic) {
    let (state, mut mic) = counted();
    let effects = mic.step(&state, &heard(COUNT), after(1));
    assert_eq!(requests(&effects), [UplinkRequest::Start]);
    (state, mic)
}

#[test]
fn a_stall_start_answered_during_restore_tracks_the_next_unasked_stop() {
    let (state, mut mic) = retried();
    let at = after(1).saturating_add(SOON);
    let stopped = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    let running = Fact::Link(Signal::Uplink(UplinkState::Running));
    let first = mic.step(&state, &stopped, at);
    assert!(first.contains(&Effect::Audio(AudioRequest::Restore(RestoreId(0)))));
    assert_eq!(kind(mic.phase), Kind::RestoringUnasked);
    let mut expired = mic;
    let answered = mic.step(&state, &running, at);
    assert_eq!(kind(mic.phase), Kind::Running);
    assert!(answered.contains(&Effect::Audio(AudioRequest::CancelRestore)));
    let second = mic.step(&state, &stopped, at);
    assert!(second.contains(&Effect::Audio(AudioRequest::Restore(RestoreId(1)))));
    assert_eq!(kind(mic.phase), Kind::RestoringUnasked);

    drop(expired.step(&state, &running, after(2)));
    let after_window = expired.step(&state, &stopped, after(2).saturating_add(SOON));
    assert_eq!(restore_ids(&after_window), []);
}

#[test]
fn off_while_a_stall_start_is_out_owes_the_stop_until_its_answer_window_ends() {
    for worn in [false, true] {
        let (_, mut mic) = retried();
        // A quiet sink, so the gate is open and only the START unanswered holds the STOP.
        mic.audio.quiet = true;
        let mut off = linked(Column::Off);
        let owed = after(1).saturating_add(SOON);
        let effects = mic.step(&off, &Fact::Changed, owed);
        assert_eq!(requests(&effects), NOTHING);
        assert_eq!(lines(&effects), [STOP_OWED_LINE]);
        assert_eq!(kind(mic.phase), Kind::Running);
        // Worn or not, the owed STOP waits for the answer window, which ends with the next read.
        off.ear = EarReport::from_in_ear(worn, worn);
        assert!(quiet(&mic.step(&off, &Fact::Changed, owed)), "{worn}");
        assert_eq!(watch_of(&mic).deadline(), Some(after(2)), "{worn}");
        // A worn bud's read can come early and holds the STOP; the one on the window's end sends it.
        let wake = || if worn { heard(COUNT) } else { Fact::Tick };
        let before = after(2).saturating_sub(Duration::from_millis(1));
        assert!(quiet(&mic.step(&off, &wake(), before)), "{worn}");
        assert_eq!(
            requests(&mic.step(&off, &wake(), after(2))),
            [UplinkRequest::Stop],
            "{worn}"
        );
        let stopping = if worn {
            Kind::StoppingQuiet
        } else {
            Kind::Stopping
        };
        assert_eq!(kind(mic.phase), stopping, "{worn}");
    }
}

#[test]
fn off_while_a_stall_start_is_out_over_a_playing_sink_waits_for_the_gate() {
    let (_, mut mic) = retried();
    let off = linked(Column::Off);
    let effects = mic.step(&off, &Fact::Changed, after(1).saturating_add(SOON));
    assert_eq!(requests(&effects), NOTHING);
    assert_eq!(lines(&effects), [STOP_OWED_PLAYING_LINE]);
    // The answer window ends with the gate still shut, so the STOP stays owed.
    assert!(quiet(&mic.step(&off, &heard(COUNT), after(2))));
    assert_eq!(kind(mic.phase), Kind::Waiting);
    let suspended = sink_in(true, SinkState::Suspended);
    let effects = mic.step(&off, &suspended, after(2).saturating_add(SOON));
    assert_eq!(requests(&effects), [UplinkRequest::Stop]);
    assert_eq!(kind(mic.phase), Kind::StoppingQuiet);
}

#[test]
fn a_sleep_while_a_stall_start_is_out_keeps_running_until_its_window_ends() {
    let (state, mut mic) = retried();
    // A quiet sink, so the gate is open and only the START unanswered holds the STOP.
    mic.audio.quiet = true;
    let owed = after(1).saturating_add(SOON);
    let effects = mic.step(&state, &Fact::Sleep(SleepEdge::Sleeping), owed);
    assert_eq!(requests(&effects), NOTHING);
    assert_eq!(kind(mic.phase), Kind::Running);
    assert_eq!(lines(&effects), [STOP_OWED_LINE]);

    let mut woke = mic;
    let effects = mic.step(&state, &Fact::Tick, after(2));
    assert_eq!(requests(&effects), STOP);
    assert_eq!(lines(&effects), [Word::Sleep.line()]);
    assert_eq!(kind_of(&mic), Kind::Asleep);

    let effects = woke.step(&state, &Fact::Sleep(SleepEdge::Woke), owed);
    assert_eq!(requests(&effects), NOTHING);
    assert_eq!(kind(woke.phase), Kind::Running);
    let auto = linked(Column::Auto);
    assert_eq!(
        requests(&woke.step(&auto, &Fact::Changed, after(2))),
        NOTHING
    );
    assert_eq!(kind(woke.phase), Kind::Lingering);
    let linger_ends = after(2).saturating_add(Duration::from_millis(UPLINK_LINGER_MS));
    assert_eq!(requests(&woke.step(&auto, &Fact::Tick, linger_ends)), STOP);
    assert_eq!(kind(woke.phase), Kind::StoppingQuiet);
}

#[test]
fn a_running_uplink_stops_at_the_sleep_edge_so_no_count_is_read_asleep() {
    let (state, mut mic) = counted();
    // A quiet sink, so the gate lets the STOP of the sleep out.
    mic.audio.quiet = true;
    let sleeping = Fact::Sleep(SleepEdge::Sleeping);
    let effects = mic.step(&state, &sleeping, SENT.saturating_add(SOON));
    assert_eq!(requests(&effects), [UplinkRequest::Stop]);
    assert!(quiet(&mic.step(&state, &heard(COUNT), after(1))));
    assert_eq!(mic.start_failures, 0);
}

#[test]
fn a_still_count_read_asleep_sends_no_start_and_spends_no_try() {
    let (state, mut mic) = counted();
    let sleeping = Fact::Sleep(SleepEdge::Sleeping);
    assert!(quiet(&mic.step(
        &state,
        &sleeping,
        SENT.saturating_add(SOON)
    )));
    assert_eq!(kind(mic.phase), Kind::Running);
    // Over a playing sink the sleep keeps the uplink, and its stall check sends nothing until the wake.
    assert!(quiet(&mic.step(&state, &heard(COUNT), after(1))));
    assert_eq!(mic.start_failures, 0);
    drop(mic.step(&state, &Fact::Sleep(SleepEdge::Woke), after(1)));
    let effects = mic.step(&state, &heard(COUNT), after(2));
    assert_eq!(requests(&effects), [UplinkRequest::Start]);
}

#[test]
fn a_still_count_while_the_uplink_is_not_wanted_sends_nothing_and_spends_no_try() {
    let unwanted = linked(Column::Auto);
    let wanted = linked(Column::AlwaysKnown);
    for from in [Kind::Lingering, Kind::Waiting] {
        let mut mic = mic(phase(from));
        drop(mic.step(&unwanted, &heard(COUNT), SENT));
        assert_eq!(watch_of(&mic).deadline(), Some(after(1)), "{from:?}");
        let effects = mic.step(&unwanted, &heard(COUNT), after(1));
        assert!(quiet(&effects), "{from:?}");
        assert_eq!((kind(mic.phase), mic.start_failures), (from, 0), "{from:?}");
        assert_eq!(watch_of(&mic).deadline(), Some(after(2)), "{from:?}");
        // Wanted again, the count still has not moved since `SENT`, and the stall sends try 2.
        let effects = mic.step(&wanted, &heard(COUNT), after(2));
        let stalled = stalled_line(STALL.saturating_mul(2).as_millis());
        let sent = start_sent_line(2);
        assert_eq!(
            lines(&effects),
            [stalled.as_str(), sent.as_str()],
            "{from:?}"
        );
    }
}

#[test]
fn a_stall_during_exit_leaves_the_stop_owed_to_the_gate() {
    for stall in [1, START_TRIES] {
        let (state, mut mic) = counted();
        for window in 1..stall {
            assert_eq!(
                requests(&mic.step(&state, &heard(COUNT), after(window))),
                START
            );
        }
        let before = after(stall.saturating_sub(1)).saturating_add(SOON);
        let effects = mic.step(&state, &Fact::Exit, before);
        assert_eq!(requests(&effects), NOTHING, "stall {stall}");
        assert_eq!(lines(&effects), [STOP_OWED_PLAYING_LINE], "stall {stall}");

        let effects = mic.step(&state, &heard(COUNT), after(stall));
        assert_eq!(requests(&effects), NOTHING, "stall {stall}");
        assert!(quiet(&effects), "stall {stall}: {effects:?}");
        assert_eq!(kind(mic.phase), Kind::Waiting, "stall {stall}");
        assert_eq!(mic.start_failures, stall - 1, "stall {stall}");

        let suspended = sink_in(true, SinkState::Suspended);
        let effects = mic.step(&state, &suspended, after(stall).saturating_add(SOON));
        assert_eq!(requests(&effects), STOP, "stall {stall}");
        assert_eq!(lines(&effects), [STOP_QUIET_LINE], "stall {stall}");
        assert_eq!(kind(mic.phase), Kind::StoppingQuiet, "stall {stall}");
    }
}

#[test]
fn a_running_uplink_wakes_at_the_earlier_of_its_hold_and_its_check() {
    let state = linked(Column::Auto);
    let linger = SENT.saturating_add(Duration::from_millis(UPLINK_LINGER_MS));
    let mut mic = mic(phase(Kind::Lingering));
    drop(mic.step(&state, &heard(COUNT), SENT));
    assert_eq!(mic.deadline(), Some(after(1)));
    let late = linger.saturating_sub(SOON);
    drop(mic.step(&state, &heard(COUNT.saturating_add(1)), late));
    assert_eq!(kind(mic.phase), Kind::Lingering);
    assert_eq!(mic.deadline(), Some(linger));
}

#[test]
fn a_late_uplink_logs_no_first_audio_for_an_old_start() {
    let state = linked(Column::AlwaysKnown);
    let mut mic = Mic {
        armed: Some(SENT),
        ..mic(phase(Kind::Failed))
    };
    let marker = Fact::Link(Signal::Uplink(UplinkState::Running));
    drop(mic.step(&state, &marker, LATER));
    assert_eq!(mic.armed, None);
    let first = Fact::Mic(MicFact::FirstAudio);
    assert!(quiet(&mic.step(&state, &first, LATER.saturating_add(SOON))));
}

#[test]
fn a_count_that_moved_after_a_stall_gives_the_tries_back() {
    let (state, mut mic) = counted();
    drop(mic.step(&state, &heard(COUNT), after(1)));
    let more = COUNT.saturating_add(1);
    assert!(quiet(&mic.step(&state, &heard(more), after(2))));
    let effects = mic.step(&state, &heard(more), after(3));
    let stalled = stalled_line(STALL.as_millis());
    let sent = start_sent_line(2);
    assert_eq!(lines(&effects), [stalled.as_str(), sent.as_str()]);
}

#[test]
fn no_bud_worn_has_no_check_and_a_bud_worn_again_reads_at_once() {
    let (mut state, mut mic) = counted();
    state.ear = EarReport::from_in_ear(false, false);
    let out = SENT.saturating_add(SOON);
    drop(mic.step(&state, &Fact::Changed, out));
    assert_eq!(watch_of(&mic).deadline(), None);
    assert!(quiet(&mic.step(&state, &heard(COUNT), after(2))));
    state.ear = EarReport::from_in_ear(true, true);
    let back = after(2).saturating_add(SOON);
    drop(mic.step(&state, &Fact::Changed, back));
    assert_eq!(watch_of(&mic).deadline(), Some(back));
    // The count before the buds came out is not the baseline of the new check.
    assert!(quiet(&mic.step(&state, &heard(COUNT), back)));
    let effects = mic.step(&state, &heard(COUNT), back.saturating_add(STALL));
    assert_eq!(requests(&effects), [UplinkRequest::Start]);
}

#[test]
fn a_count_outside_a_running_uplink_steps_as_a_change() {
    let state = linked(Column::AlwaysKnown);
    for from in [
        Kind::Stopped,
        Kind::Starting,
        Kind::Stopping,
        Kind::Restoring,
    ] {
        let mut read = mic(phase(from));
        let mut changed = read;
        let effects = read.step(&state, &heard(COUNT), LATER);
        assert_eq!(
            effects,
            changed.step(&state, &Fact::Changed, LATER),
            "{from:?}"
        );
        assert_eq!(read, changed, "{from:?}");
    }
}

#[test]
fn the_first_audio_after_a_start_logs_its_time_once() {
    let state = linked(Column::AlwaysKnown);
    let mut mic = Mic::default();
    assert_eq!(
        requests(&mic.step(&state, &Fact::Changed, SENT)),
        [UplinkRequest::Start]
    );
    let first = Fact::Mic(MicFact::FirstAudio);
    let heard_at = SENT.saturating_add(SOON);
    let effects = mic.step(&state, &first, heard_at);
    let line = format!("{FIRST_AUDIO_PREFIX}{} ms", SOON.as_millis());
    assert_eq!(lines(&effects), [line.as_str()]);
    assert!(quiet(&mic.step(
        &state,
        &first,
        heard_at.saturating_add(SOON)
    )));
}
