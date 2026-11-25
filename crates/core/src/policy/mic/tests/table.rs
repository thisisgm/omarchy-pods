//! Support: the table of every phase against every fact, and the cell each end to end test drives.

use super::*;

/// Outcomes in the order of `COLUMNS`.
static CELLS: [(Kind, Event, [Outcome; 5]); 165] = {
    use Event::{
        Changed, Deadline, Down, Exit, Lost, Quiet, Restored, RunningMarker, Sleep, StoppedMarker,
        Wake,
    };
    use Kind::{
        Asleep, Failed, LastTry, Lingering, Owed, Restoring, RestoringUnasked, Running, Starting,
        Stopped, Stopping, StoppingQuiet, Unasked, Unsupported, Waiting,
    };
    const fn same(kind: Kind) -> [Outcome; 5] {
        [(NOTHING, kind); 5]
    }
    const RUN: [Outcome; 5] = [
        (NOTHING, Running),
        (NOTHING, Running),
        (NOTHING, Lingering),
        (NOTHING, Waiting),
        (NOTHING, Waiting),
    ];
    const QUIET_RUN: [Outcome; 5] = [
        (NOTHING, Running),
        (NOTHING, Running),
        (NOTHING, Lingering),
        (STOP, StoppingQuiet),
        (STOP, StoppingQuiet),
    ];
    const WAIT: [Outcome; 5] = [
        (NOTHING, Running),
        (NOTHING, Running),
        (NOTHING, Waiting),
        (NOTHING, Waiting),
        (NOTHING, Waiting),
    ];
    const QUIET_WAIT: [Outcome; 5] = [
        (NOTHING, Running),
        (NOTHING, Running),
        (STOP, StoppingQuiet),
        (STOP, StoppingQuiet),
        (STOP, StoppingQuiet),
    ];
    const RESTART: [Outcome; 5] = [
        (START, Starting),
        (START, Starting),
        (NOTHING, Stopped),
        (NOTHING, Stopped),
        (NOTHING, Stopped),
    ];
    const RESET: [Outcome; 5] = same(Stopped);
    const RETRY: [Outcome; 5] = [
        (START, Starting),
        (START, Starting),
        (NOTHING, Stopped),
        (NOTHING, Stopped),
        (NOTHING, Stopped),
    ];
    const UNANSWERED: [Outcome; 5] = [
        (NOTHING, Unsupported),
        (NOTHING, Failed),
        (NOTHING, Stopped),
        (NOTHING, Stopped),
        (NOTHING, Stopped),
    ];
    const STARTING: [Outcome; 5] = [
        (NOTHING, Starting),
        (NOTHING, Starting),
        (NOTHING, Starting),
        (NOTHING, Owed),
        (NOTHING, Owed),
    ];
    const LAST_TRY: [Outcome; 5] = [
        (NOTHING, LastTry),
        (NOTHING, LastTry),
        (NOTHING, LastTry),
        (NOTHING, Owed),
        (NOTHING, Owed),
    ];
    const UNASKED: [Outcome; 5] = [
        (NOTHING, Unasked),
        (NOTHING, Unasked),
        (NOTHING, Stopped),
        (NOTHING, Stopped),
        (NOTHING, Unasked),
    ];
    const RESTORING_UNASKED: [Outcome; 5] = [
        (NOTHING, RestoringUnasked),
        (NOTHING, RestoringUnasked),
        (NOTHING, Restoring),
        (NOTHING, Restoring),
        (NOTHING, RestoringUnasked),
    ];
    [
        (Stopped, RunningMarker, WAIT),
        (Stopped, StoppedMarker, RESTART),
        (Stopped, Deadline, RESTART),
        (Stopped, Down, RESET),
        (Stopped, Lost, RESET),
        (Stopped, Changed, RESTART),
        (Stopped, Restored, RESTART),
        (Stopped, Quiet, RESTART),
        (Stopped, Exit, same(Stopped)),
        (Stopped, Sleep, same(Asleep)),
        (Stopped, Wake, RESTART),
        (Failed, RunningMarker, WAIT),
        (Failed, StoppedMarker, same(Failed)),
        (Failed, Deadline, same(Failed)),
        (Failed, Down, RESET),
        (Failed, Lost, RESET),
        (Failed, Changed, same(Failed)),
        (Failed, Restored, same(Failed)),
        (Failed, Quiet, same(Failed)),
        (Failed, Exit, same(Failed)),
        (Failed, Sleep, same(Failed)),
        (Failed, Wake, same(Failed)),
        (Unsupported, RunningMarker, WAIT),
        (Unsupported, StoppedMarker, same(Unsupported)),
        (Unsupported, Deadline, same(Unsupported)),
        (Unsupported, Down, RESET),
        (Unsupported, Lost, RESET),
        (Unsupported, Changed, same(Unsupported)),
        (Unsupported, Restored, same(Unsupported)),
        (Unsupported, Quiet, same(Unsupported)),
        (Unsupported, Exit, same(Unsupported)),
        (Unsupported, Sleep, same(Unsupported)),
        (Unsupported, Wake, same(Unsupported)),
        (Unasked, RunningMarker, WAIT),
        (Unasked, StoppedMarker, UNASKED),
        (Unasked, Deadline, UNASKED),
        (Unasked, Down, RESET),
        (Unasked, Lost, RESET),
        (Unasked, Changed, UNASKED),
        (Unasked, Restored, UNASKED),
        (Unasked, Quiet, UNASKED),
        (Unasked, Exit, same(Unasked)),
        (
            Unasked,
            Sleep,
            [
                (NOTHING, Unasked),
                (NOTHING, Unasked),
                (NOTHING, Asleep),
                (NOTHING, Asleep),
                (NOTHING, Unasked),
            ],
        ),
        (Unasked, Wake, UNASKED),
        (Starting, RunningMarker, RUN),
        (Starting, StoppedMarker, STARTING),
        (Starting, Deadline, RETRY),
        (Starting, Down, same(Restoring)),
        (Starting, Lost, RESET),
        (Starting, Changed, STARTING),
        (Starting, Restored, STARTING),
        (Starting, Quiet, STARTING),
        (Starting, Exit, same(Owed)),
        (Starting, Sleep, same(Asleep)),
        (Starting, Wake, STARTING),
        (LastTry, RunningMarker, RUN),
        (LastTry, StoppedMarker, LAST_TRY),
        (LastTry, Deadline, UNANSWERED),
        (LastTry, Down, same(Restoring)),
        (LastTry, Lost, RESET),
        (LastTry, Changed, LAST_TRY),
        (LastTry, Restored, LAST_TRY),
        (LastTry, Quiet, LAST_TRY),
        (LastTry, Exit, same(Owed)),
        (LastTry, Sleep, same(Asleep)),
        (LastTry, Wake, LAST_TRY),
        (Owed, RunningMarker, RUN),
        (Owed, StoppedMarker, same(Owed)),
        (Owed, Deadline, RETRY),
        (Owed, Down, same(Restoring)),
        (Owed, Lost, RESET),
        (Owed, Changed, same(Owed)),
        (Owed, Restored, same(Owed)),
        (Owed, Quiet, same(Owed)),
        (Owed, Exit, same(Owed)),
        (Owed, Sleep, same(Asleep)),
        (Owed, Wake, same(Owed)),
        (Running, RunningMarker, RUN),
        (Running, StoppedMarker, RESTORING_UNASKED),
        (Running, Deadline, RUN),
        (Running, Down, same(Restoring)),
        (Running, Lost, RESET),
        (Running, Changed, RUN),
        (Running, Restored, RUN),
        (Running, Quiet, QUIET_RUN),
        (Running, Exit, same(Waiting)),
        (Running, Sleep, RUN),
        (Running, Wake, RUN),
        (Lingering, RunningMarker, RUN),
        (Lingering, StoppedMarker, RESTORING_UNASKED),
        (Lingering, Deadline, WAIT),
        (Lingering, Down, same(Restoring)),
        (Lingering, Lost, RESET),
        (Lingering, Changed, RUN),
        (Lingering, Restored, RUN),
        (Lingering, Quiet, QUIET_RUN),
        (Lingering, Exit, same(Waiting)),
        (Lingering, Sleep, RUN),
        (Lingering, Wake, RUN),
        (Waiting, RunningMarker, WAIT),
        (Waiting, StoppedMarker, RESTORING_UNASKED),
        (Waiting, Deadline, WAIT),
        (Waiting, Down, same(Restoring)),
        (Waiting, Lost, RESET),
        (Waiting, Changed, WAIT),
        (Waiting, Restored, WAIT),
        (Waiting, Quiet, QUIET_WAIT),
        (Waiting, Exit, same(Waiting)),
        (Waiting, Sleep, WAIT),
        (Waiting, Wake, WAIT),
        (Stopping, RunningMarker, same(Stopping)),
        (Stopping, StoppedMarker, same(Restoring)),
        (Stopping, Deadline, same(Restoring)),
        (Stopping, Down, same(Restoring)),
        (Stopping, Lost, RESET),
        (Stopping, Changed, same(Stopping)),
        (Stopping, Restored, same(Stopping)),
        (Stopping, Quiet, same(Stopping)),
        (Stopping, Exit, same(Stopping)),
        (Stopping, Sleep, same(Stopping)),
        (Stopping, Wake, same(Stopping)),
        (StoppingQuiet, RunningMarker, same(StoppingQuiet)),
        (StoppingQuiet, StoppedMarker, RESTART),
        (StoppingQuiet, Deadline, RESTART),
        (StoppingQuiet, Down, RESET),
        (StoppingQuiet, Lost, RESET),
        (StoppingQuiet, Changed, same(StoppingQuiet)),
        (StoppingQuiet, Restored, same(StoppingQuiet)),
        (StoppingQuiet, Quiet, same(StoppingQuiet)),
        (StoppingQuiet, Exit, same(StoppingQuiet)),
        (StoppingQuiet, Sleep, same(StoppingQuiet)),
        (StoppingQuiet, Wake, same(StoppingQuiet)),
        (Restoring, RunningMarker, WAIT),
        (Restoring, StoppedMarker, same(Restoring)),
        (Restoring, Deadline, RESTART),
        (Restoring, Down, same(Restoring)),
        (Restoring, Lost, RESET),
        (Restoring, Changed, same(Restoring)),
        (Restoring, Restored, RESTART),
        (Restoring, Quiet, same(Restoring)),
        (Restoring, Exit, same(Restoring)),
        (Restoring, Sleep, same(Restoring)),
        (Restoring, Wake, same(Restoring)),
        (RestoringUnasked, RunningMarker, WAIT),
        (RestoringUnasked, StoppedMarker, RESTORING_UNASKED),
        (RestoringUnasked, Deadline, UNASKED),
        (RestoringUnasked, Down, same(Restoring)),
        (RestoringUnasked, Lost, RESET),
        (RestoringUnasked, Changed, RESTORING_UNASKED),
        (RestoringUnasked, Restored, UNASKED),
        (RestoringUnasked, Quiet, RESTORING_UNASKED),
        (RestoringUnasked, Exit, RESTORING_UNASKED),
        (RestoringUnasked, Sleep, RESTORING_UNASKED),
        (RestoringUnasked, Wake, RESTORING_UNASKED),
        (Asleep, RunningMarker, WAIT),
        (Asleep, StoppedMarker, same(Asleep)),
        (Asleep, Deadline, same(Asleep)),
        (Asleep, Down, same(Asleep)),
        (Asleep, Lost, same(Asleep)),
        (Asleep, Changed, same(Asleep)),
        (Asleep, Restored, same(Asleep)),
        (Asleep, Quiet, same(Asleep)),
        (Asleep, Exit, same(Asleep)),
        (Asleep, Sleep, same(Asleep)),
        (Asleep, Wake, RESTART),
    ]
};

/// The cell each test of the uplink suites drives through the built daemon: the test, the phase, the fact and the column.
const COVERED: &str = "
always_and_a_bud_worn_starts_the_uplink_once Stopped Changed AlwaysNew
the_preference_off_sends_no_start_and_reads_off Stopped Changed Off
auto_with_a_confirmed_model_sends_nothing Stopped Changed Auto
an_unconfirmed_model_is_started_once_and_stored Starting RunningMarker AlwaysNew
the_running_marker_not_the_start_moves_the_state Starting RunningMarker AlwaysNew
setting_off_with_no_bud_worn_sends_stop_at_once Stopping StoppedMarker Off
setting_off_while_starting_sends_no_stop_until_the_start_is_answered Starting Changed Off
setting_off_while_starting_sends_no_stop_until_the_start_is_answered Owed RunningMarker Off
setting_off_while_starting_and_never_answered_sends_nothing Owed Deadline Off
a_late_answer_after_off_is_stopped_at_once Stopped RunningMarker Off
a_link_lost_while_live_sends_nothing Running Down AlwaysNew
a_model_that_never_answers_is_unsupported_and_not_stored Starting Deadline AlwaysNew
a_model_that_never_answers_is_unsupported_and_not_stored LastTry Deadline AlwaysNew
a_model_that_never_answers_is_unsupported_and_not_stored Unsupported Down AlwaysNew
a_confirmed_model_that_never_answers_reads_failed Starting Deadline AlwaysKnown
a_confirmed_model_that_never_answers_reads_failed LastTry Deadline AlwaysKnown
a_stop_never_answered_is_logged_and_ends_stopping Stopping Deadline Off
a_stop_never_answered_is_logged_and_ends_stopping Restoring Restored Off
a_stop_never_answered_is_logged_and_ends_stopping Stopped Changed AlwaysKnown
a_stop_never_answered_is_logged_and_ends_stopping Starting StoppedMarker AlwaysKnown
two_dictations_inside_the_linger_send_one_start_and_no_stop Running Changed Auto
two_dictations_inside_the_linger_send_one_start_and_no_stop Lingering Changed AlwaysKnown
setting_off_with_no_bud_worn_sends_stop_and_one_restore_inside_the_bound Stopping StoppedMarker Off
setting_off_with_no_bud_worn_sends_stop_and_one_restore_inside_the_bound Restoring Restored Off
setting_off_with_a_suspended_sink_stops_at_once_and_touches_no_sink StoppingQuiet StoppedMarker Off
a_sink_that_wakes_during_a_quiet_stop_is_restored Stopping StoppedMarker Off
setting_off_while_starting_restores_after_the_owed_stop Owed RunningMarker Off
setting_off_while_starting_restores_after_the_owed_stop Stopping StoppedMarker Off
the_restore_waits_for_the_stopped_marker Stopping Changed Off
the_restore_waits_for_the_stopped_marker Stopping StoppedMarker Off
a_stop_confirmed_after_the_deadline_restores_once_more Stopping Deadline Off
a_stop_confirmed_after_the_deadline_restores_once_more Stopped StoppedMarker Off
a_suspended_sink_is_left_alone_by_the_restore Stopping Quiet Off
an_idle_sink_is_suspended_and_resumed_with_no_hold Restoring Restored Off
a_restore_with_no_audio_server_is_done_when_it_returns Restoring Restored Off
a_restore_owed_when_the_channel_closes_is_done_after_it_reopens Stopped Down Off
a_resume_the_server_never_took_is_sent_when_it_returns Restoring Down Off
a_suspend_left_by_a_lost_resume_is_undone_after_a_newer_start Stopped Changed AlwaysKnown
an_event_cut_by_the_restore_is_still_read_whole Restoring Restored Off
exit_with_no_bud_worn_stops_and_restores_inside_the_bound Stopping StoppedMarker Exit
exit_with_no_bud_worn_stops_and_restores_inside_the_bound Restoring Restored Exit
exit_while_starting_waits_for_the_marker_then_stops_and_restores Starting Exit AlwaysNew
exit_while_starting_waits_for_the_marker_then_stops_and_restores Owed RunningMarker Exit
exit_while_starting_and_never_answered_sends_nothing_and_ends_at_the_start_deadline Owed Deadline Exit
exit_with_the_stop_and_the_restore_stalled_ends_at_the_bound Stopping Deadline Exit
exit_with_the_server_lost_inside_the_restore_resumes_the_sink_before_it_ends Restoring Restored Exit
a_restore_owed_while_the_server_was_down_is_dropped_by_a_newer_start Stopped Changed AlwaysKnown
exit_with_the_uplink_stopped_is_answered_at_once Stopped Exit Auto
a_device_removed_during_a_stop_writes_nothing_to_its_gone_sink Stopping Down Off
a_channel_end_in_stopping_restores_the_running_sink Stopping Down Off
a_channel_end_while_live_restores_the_running_sink Running Down AlwaysKnown
a_channel_end_before_the_running_marker_restores_the_running_sink Starting Down AlwaysKnown
a_sink_removed_during_a_channel_end_restore_keeps_the_audio_server_up Running Down AlwaysKnown
a_sink_removed_during_a_channel_end_restore_keeps_the_audio_server_up Restoring Restored AlwaysKnown
the_stop_waits_while_the_sink_is_not_suspended_and_a_bud_is_worn Lingering Deadline Auto
an_idle_sink_still_holds_the_stop Waiting Changed Auto
the_stop_goes_out_when_the_sink_is_suspended_and_no_restore_follows Waiting Quiet Auto
the_stop_goes_out_when_the_sink_is_suspended_and_no_restore_follows StoppingQuiet StoppedMarker Auto
the_linger_with_a_suspended_sink_ends_in_a_stop_and_no_restore Lingering Quiet Auto
the_linger_with_a_suspended_sink_ends_in_a_stop_and_no_restore StoppingQuiet StoppedMarker Auto
a_want_that_returns_while_the_stop_waits_sends_nothing Waiting Changed AlwaysKnown
an_unknown_sink_state_holds_the_stop Lingering Deadline Auto
the_stop_goes_out_with_a_restore_when_the_last_bud_leaves Stopping StoppedMarker AlwaysKnown
the_stop_goes_out_with_a_restore_when_the_last_bud_leaves Restoring Restored AlwaysKnown
a_bud_back_in_during_the_restore_starts_after_it Restoring Changed AlwaysKnown
a_bud_back_in_during_the_restore_starts_after_it Restoring Restored AlwaysKnown
the_second_try_answered_goes_live_and_counts_one_failure Starting Deadline AlwaysKnown
the_second_try_answered_goes_live_and_counts_one_failure Starting RunningMarker AlwaysKnown
no_stop_is_sent_for_a_start_never_answered LastTry Deadline AlwaysKnown
setting_off_during_the_second_try_sends_no_third_start Starting Changed Off
setting_off_during_the_second_try_sends_no_third_start Owed Deadline Off
a_late_running_marker_with_no_want_is_stopped_through_the_same_gate Starting Deadline Auto
a_late_running_marker_with_no_want_is_stopped_through_the_same_gate Stopped RunningMarker Auto
a_late_running_marker_with_no_want_is_stopped_through_the_same_gate Waiting Quiet Auto
a_stop_nobody_asked_for_restores_and_does_not_start_again Running StoppedMarker AlwaysKnown
a_stop_nobody_asked_for_restores_and_does_not_start_again RestoringUnasked Restored AlwaysKnown
a_stop_nobody_asked_for_restores_and_does_not_start_again Unasked Changed AlwaysKnown
a_held_start_after_an_unasked_stop_cancels_the_restore Running StoppedMarker AlwaysKnown
a_held_start_after_an_unasked_stop_cancels_the_restore RestoringUnasked RunningMarker AlwaysKnown
the_bud_put_back_after_that_starts_again Unasked Changed Auto
a_want_gone_and_back_during_the_unasked_restore_starts_after_it Running StoppedMarker AlwaysKnown
a_want_gone_and_back_during_the_unasked_restore_starts_after_it RestoringUnasked Changed Off
a_want_gone_and_back_during_the_unasked_restore_starts_after_it Restoring Changed AlwaysKnown
a_want_gone_and_back_during_the_unasked_restore_starts_after_it Restoring Restored AlwaysKnown
both_buds_out_and_back_during_the_unasked_restore_start_after_it Running StoppedMarker AlwaysKnown
both_buds_out_and_back_during_the_unasked_restore_start_after_it RestoringUnasked Changed Auto
both_buds_out_and_back_during_the_unasked_restore_start_after_it Restoring Changed AlwaysKnown
both_buds_out_and_back_during_the_unasked_restore_start_after_it Restoring Restored AlwaysKnown
sleep_over_a_suspended_sink_sends_stop_before_the_suspend_and_no_restore Running Sleep AlwaysKnown
sleep_while_starting_sends_nothing Starting Sleep AlwaysKnown
resume_starts_again_when_the_want_still_holds Asleep Wake AlwaysKnown
the_link_dropping_in_starting_sends_nothing_more Starting Lost AlwaysKnown
failed_is_forgotten_at_a_reconnect Failed Lost AlwaysKnown
an_unanswered_start_is_tried_three_times_then_failed LastTry Deadline AlwaysKnown
a_stop_never_confirmed_after_off_still_restores Stopping Deadline Off
a_stop_never_confirmed_through_the_gate_restores_nothing StoppingQuiet Deadline Auto
a_start_is_still_sent_after_a_stop_never_confirmed Stopped Changed AlwaysKnown
a_start_is_still_sent_after_a_stop_never_confirmed Starting StoppedMarker AlwaysKnown
a_start_is_still_sent_after_a_stop_never_confirmed Starting Deadline AlwaysKnown
off_during_a_call_with_music_withdraws_the_node_and_owes_the_stop Running Changed Off
the_owed_stop_goes_out_when_the_sink_suspends Waiting Quiet Off
the_owed_stop_goes_out_when_the_sink_suspends StoppingQuiet StoppedMarker Off
exit_during_a_call_with_music_sends_no_stop_and_no_sink_call Running Exit AlwaysKnown
exit_during_a_call_with_music_sends_no_stop_and_no_sink_call Waiting Deadline Exit
exit_with_a_stalled_uplink_waits_for_the_quiet_sink Running Exit AlwaysKnown
exit_with_a_stalled_uplink_waits_for_the_quiet_sink Waiting Deadline Exit
exit_with_a_stalled_uplink_waits_for_the_quiet_sink Waiting Quiet Exit
exit_when_the_sink_goes_quiet_inside_the_bound_sends_the_stop Waiting Quiet Exit
exit_when_the_sink_goes_quiet_inside_the_bound_sends_the_stop StoppingQuiet StoppedMarker Exit
exit_with_the_sink_suspended_sends_stop_and_no_sink_call StoppingQuiet StoppedMarker Exit
sleep_during_a_call_with_music_sends_nothing Running Sleep AlwaysKnown
";

/// The tests of the uplink suites whose case no column holds: no bud worn, no model number, the settings file, a check across a whole suite.
const OUTSIDE_THE_TABLE: &[&str] = &[
    "no_bud_worn_sends_no_start",
    "a_bud_taken_out_while_the_stop_waits_gets_the_grace",
    "the_owed_stop_goes_out_when_the_last_bud_leaves",
    "the_owed_stop_goes_out_when_the_sink_suspends_during_the_bud_grace",
    "no_uplink_stop_and_no_sink_call_while_the_sink_runs",
    "always_sends_no_start_without_a_model_number",
    "a_settings_file_with_a_bad_model_entry_is_refused",
    "a_seventeenth_model_drops_the_oldest",
    // The table does not track ownership across unsolicited and late START markers.
    "unsolicited_uplink_markers_leave_a_playing_sink_alone",
    "the_first_start_answered_after_timeout_still_owns_the_restore",
];

#[test]
fn every_phase_against_every_fact() {
    for (from, event, outcomes) in CELLS {
        for (column, (sent, to)) in COLUMNS.into_iter().zip(outcomes) {
            let (effects, mic) = run(from, event, column);
            let cell = format!("{from:?}, {event:?}, {column:?}");
            assert_eq!(requests(&effects), sent, "{cell}");
            assert_eq!(kind_of(&mic), to, "{cell}");
            let asleep = event == Event::Sleep || (from == Kind::Asleep && event != Event::Wake);
            assert_eq!(mic.asleep, asleep, "{cell}");
            let exiting = column == Column::Exit || event == Event::Exit;
            let stopped = matches!(
                to,
                Kind::Stopped | Kind::Failed | Kind::Unsupported | Kind::Unasked | Kind::Asleep
            );
            let answered = exiting && stopped;
            assert_eq!(effects.contains(&Effect::Exited), answered, "{cell}");
        }
    }
}

#[test]
fn the_table_holds_each_phase_and_fact_once() {
    for from in KINDS {
        for event in EVENTS {
            let rows = CELLS
                .iter()
                .filter(|(kind, fact, _)| *kind == from && *fact == event);
            assert_eq!(rows.count(), 1, "{from:?}, {event:?}");
        }
    }
}

/// The name of every `#[test]` in `suite`.
// Sample input: "    #[test]\n    #[ignore = \"slow\"]\n    fn no_bud_worn_sends_no_start() {".
pub(in crate::policy::mic) fn test_names(suite: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut lines = suite.lines();
    while let Some(line) = lines.next() {
        if line.trim() != "#[test]" {
            continue;
        }
        let item = lines
            .find(|line| line.trim().starts_with("fn "))
            .unwrap()
            .trim();
        let name = item.strip_prefix("fn ").unwrap().split_once('(').unwrap().0;
        names.push(name);
    }
    names
}

#[test]
fn every_end_to_end_test_names_its_cell_or_stands_outside_the_table() {
    let suites = [
        include_str!("../../../../../daemon/tests/uplink.rs"),
        include_str!("../../../../../daemon/tests/uplink_stop.rs"),
        include_str!("../../../../../daemon/tests/uplink_slow.rs"),
        include_str!("../../../../../daemon/tests/uplink_faults.rs"),
        include_str!("../../../../../daemon/tests/uplink_quiet.rs"),
    ];
    let cells: Vec<String> = CELLS
        .iter()
        .flat_map(|(from, event, _)| COLUMNS.map(|column| format!("{from:?} {event:?} {column:?}")))
        .collect();
    let mut named = Vec::from(OUTSIDE_THE_TABLE);
    // Sample input: `a_link_lost_while_live_sends_nothing Running Down AlwaysNew`.
    for line in COVERED.lines().filter(|line| !line.is_empty()) {
        let (test, cell) = line.split_once(' ').unwrap();
        assert!(cells.iter().any(|known| known == cell), "{line}");
        assert!(!OUTSIDE_THE_TABLE.contains(&test), "{test}");
        named.push(test);
    }
    named.sort_unstable();
    named.dedup();
    let mut tests: Vec<&str> = suites.into_iter().flat_map(test_names).collect();
    tests.sort_unstable();
    assert_eq!(named, tests);
}
