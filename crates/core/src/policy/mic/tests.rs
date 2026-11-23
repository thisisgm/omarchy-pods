//! Support tests for the machine the uplink suites drive through the built daemon: every phase against every fact, the lines, the deadline and the code `mic` shows.

use std::time::Duration;

use pods_proto::ear::EarReport;
use pods_samples::MODEL_NUMBER;

use super::*;
use crate::policy::tests::device;
use crate::prefs::MicModels;
use crate::state::Microphone;

/// When the START or STOP of every phase under test went out.
const SENT: Duration = Duration::from_secs(10);
/// Inside every window.
const SOON: Duration = Duration::from_millis(100);
/// Past every window, the restore's being the longest.
const LATER: Duration = Duration::from_millis(RESTORE_WAIT_MS);
const _: () = assert!(RESTORE_WAIT_MS > START_ANSWER_MS && RESTORE_WAIT_MS > STOP_ANSWER_MS);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    RunningMarker,
    StoppedMarker,
    /// The clock past every deadline the phases under test keep.
    Deadline,
    Down,
    Lost,
    Changed,
    Restored,
    Exit,
}

/// The five states a cell is stepped in, a bud worn in each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    /// `always`, a model never confirmed.
    AlwaysNew,
    /// `always`, a confirmed model.
    AlwaysKnown,
    /// `auto`, a confirmed model: nothing wanted.
    Auto,
    Off,
    /// `always`, a confirmed model, and the daemon stopping.
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Stopped,
    Failed,
    Unsupported,
    Starting,
    Owed,
    Running,
    Stopping,
    Restoring,
}

type Outcome = (&'static [UplinkRequest], Kind);

const NOTHING: &[UplinkRequest] = &[];
const START: &[UplinkRequest] = &[UplinkRequest::Start];
const STOP: &[UplinkRequest] = &[UplinkRequest::Stop];

const COLUMNS: [Column; 5] = [
    Column::AlwaysNew,
    Column::AlwaysKnown,
    Column::Auto,
    Column::Off,
    Column::Exit,
];

const EVENTS: [Event; 8] = [
    Event::RunningMarker,
    Event::StoppedMarker,
    Event::Deadline,
    Event::Down,
    Event::Lost,
    Event::Changed,
    Event::Restored,
    Event::Exit,
];

const KINDS: [Kind; 8] = [
    Kind::Stopped,
    Kind::Failed,
    Kind::Unsupported,
    Kind::Starting,
    Kind::Owed,
    Kind::Running,
    Kind::Stopping,
    Kind::Restoring,
];

/// Outcomes in the order of `COLUMNS`.
const CELLS: [(Kind, Event, [Outcome; 5]); 64] = {
    use Event::{Changed, Deadline, Down, Exit, Lost, Restored, RunningMarker, StoppedMarker};
    use Kind::{Failed, Owed, Restoring, Running, Starting, Stopped, Stopping, Unsupported};
    const fn same(kind: Kind) -> [Outcome; 5] {
        [(NOTHING, kind); 5]
    }
    const RUN: [Outcome; 5] = [
        (NOTHING, Running),
        (NOTHING, Running),
        (NOTHING, Running),
        (STOP, Stopping),
        (STOP, Stopping),
    ];
    const RESTART: [Outcome; 5] = [
        (START, Starting),
        (START, Starting),
        (NOTHING, Stopped),
        (NOTHING, Stopped),
        (NOTHING, Stopped),
    ];
    const RESET: [Outcome; 5] = same(Stopped);
    const STOPS: [Outcome; 5] = [(STOP, Stopping); 5];
    const UNANSWERED: [Outcome; 5] = [
        (NOTHING, Unsupported),
        (NOTHING, Failed),
        (NOTHING, Failed),
        (NOTHING, Stopped),
        (NOTHING, Failed),
    ];
    const STARTING: [Outcome; 5] = [
        (NOTHING, Starting),
        (NOTHING, Starting),
        (NOTHING, Starting),
        (NOTHING, Owed),
        (NOTHING, Owed),
    ];
    [
        (Stopped, RunningMarker, RUN),
        (Stopped, StoppedMarker, RESTART),
        (Stopped, Deadline, RESTART),
        (Stopped, Down, RESET),
        (Stopped, Lost, RESET),
        (Stopped, Changed, RESTART),
        (Stopped, Restored, RESTART),
        (Stopped, Exit, same(Stopped)),
        (Failed, RunningMarker, RUN),
        (Failed, StoppedMarker, same(Failed)),
        (Failed, Deadline, same(Failed)),
        (Failed, Down, RESET),
        (Failed, Lost, RESET),
        (Failed, Changed, same(Failed)),
        (Failed, Restored, same(Failed)),
        (Failed, Exit, same(Failed)),
        (Unsupported, RunningMarker, RUN),
        (Unsupported, StoppedMarker, same(Unsupported)),
        (Unsupported, Deadline, same(Unsupported)),
        (Unsupported, Down, RESET),
        (Unsupported, Lost, RESET),
        (Unsupported, Changed, same(Unsupported)),
        (Unsupported, Restored, same(Unsupported)),
        (Unsupported, Exit, same(Unsupported)),
        (Starting, RunningMarker, RUN),
        (Starting, StoppedMarker, STARTING),
        (Starting, Deadline, UNANSWERED),
        (Starting, Down, RESET),
        (Starting, Lost, RESET),
        (Starting, Changed, STARTING),
        (Starting, Restored, STARTING),
        (Starting, Exit, same(Owed)),
        (Owed, RunningMarker, RUN),
        (Owed, StoppedMarker, same(Owed)),
        (Owed, Deadline, UNANSWERED),
        (Owed, Down, RESET),
        (Owed, Lost, RESET),
        (Owed, Changed, same(Owed)),
        (Owed, Restored, same(Owed)),
        (Owed, Exit, same(Owed)),
        (Running, RunningMarker, RUN),
        (Running, StoppedMarker, RESTART),
        (Running, Deadline, RUN),
        (Running, Down, RESET),
        (Running, Lost, RESET),
        (Running, Changed, RUN),
        (Running, Restored, RUN),
        (Running, Exit, STOPS),
        (Stopping, RunningMarker, same(Stopping)),
        (Stopping, StoppedMarker, same(Restoring)),
        (Stopping, Deadline, same(Restoring)),
        (Stopping, Down, RESET),
        (Stopping, Lost, RESET),
        (Stopping, Changed, same(Stopping)),
        (Stopping, Restored, same(Stopping)),
        (Stopping, Exit, same(Stopping)),
        (Restoring, RunningMarker, same(Restoring)),
        (Restoring, StoppedMarker, same(Restoring)),
        (Restoring, Deadline, RESTART),
        (Restoring, Down, RESET),
        (Restoring, Lost, RESET),
        (Restoring, Changed, same(Restoring)),
        (Restoring, Restored, RESTART),
        (Restoring, Exit, same(Restoring)),
    ]
};

/// The cell each test of the uplink suites drives through the built daemon: the test, the phase, the fact and the column.
const COVERED: &str = "
always_and_a_bud_worn_starts_the_uplink_once Stopped Changed AlwaysNew
the_preference_off_sends_no_start_and_reads_off Stopped Changed Off
auto_with_a_confirmed_model_sends_nothing Stopped Changed Auto
an_unconfirmed_model_is_started_once_and_stored Starting RunningMarker AlwaysNew
the_running_marker_not_the_start_moves_the_state Starting RunningMarker AlwaysNew
setting_off_while_live_sends_stop_at_once Running Changed Off
setting_off_while_live_sends_stop_at_once Stopping StoppedMarker Off
setting_off_while_starting_sends_no_stop_until_the_start_is_answered Starting Changed Off
setting_off_while_starting_sends_no_stop_until_the_start_is_answered Owed RunningMarker Off
setting_off_while_starting_and_never_answered_sends_nothing Owed Deadline Off
a_late_answer_after_off_is_stopped_at_once Stopped RunningMarker Off
a_link_lost_while_live_sends_nothing Running Down AlwaysNew
a_model_that_never_answers_is_unsupported_and_not_stored Starting Deadline AlwaysNew
a_model_that_never_answers_is_unsupported_and_not_stored Unsupported Down AlwaysNew
a_confirmed_model_that_never_answers_reads_failed Starting Deadline AlwaysKnown
a_stop_never_answered_is_logged_and_ends_stopping Stopping Deadline Off
a_stop_never_answered_is_logged_and_ends_stopping Restoring Restored Off
a_stop_never_answered_is_logged_and_ends_stopping Stopped Changed AlwaysKnown
a_stop_never_answered_is_logged_and_ends_stopping Starting StoppedMarker AlwaysKnown
setting_off_while_music_plays_sends_stop_and_one_restore_inside_the_bound Running Changed Off
setting_off_while_music_plays_sends_stop_and_one_restore_inside_the_bound Stopping StoppedMarker Off
setting_off_while_music_plays_sends_stop_and_one_restore_inside_the_bound Restoring Restored Off
setting_off_with_a_suspended_sink_stops_at_once_and_touches_no_sink Restoring Restored Off
setting_off_while_starting_restores_after_the_owed_stop Owed RunningMarker Off
setting_off_while_starting_restores_after_the_owed_stop Stopping StoppedMarker Off
the_restore_waits_for_the_stopped_marker Stopping Changed Off
the_restore_waits_for_the_stopped_marker Stopping StoppedMarker Off
a_suspended_sink_is_left_alone_by_the_restore Stopping StoppedMarker Off
an_idle_sink_is_suspended_and_resumed_with_no_hold Restoring Restored Off
a_restore_with_no_audio_server_is_done_when_it_returns Restoring Restored Off
a_resume_the_server_never_took_is_sent_when_it_returns Restoring Down Off
a_suspend_left_by_a_lost_resume_is_undone_after_a_newer_start Stopped Changed AlwaysKnown
an_event_cut_by_the_restore_is_still_read_whole Restoring Restored Off
exit_while_running_stops_and_restores_inside_the_bound Running Exit AlwaysKnown
exit_while_running_stops_and_restores_inside_the_bound Stopping StoppedMarker Exit
exit_while_running_stops_and_restores_inside_the_bound Restoring Restored Exit
exit_while_starting_waits_for_the_marker_then_stops_and_restores Starting Exit AlwaysNew
exit_while_starting_waits_for_the_marker_then_stops_and_restores Owed RunningMarker Exit
exit_while_starting_and_never_answered_sends_nothing_and_ends_at_the_start_deadline Owed Deadline Exit
exit_with_the_stop_and_the_restore_stalled_ends_at_the_bound Running Exit AlwaysKnown
exit_with_the_stop_and_the_restore_stalled_ends_at_the_bound Stopping Deadline Exit
exit_with_the_server_lost_inside_the_restore_resumes_the_sink_before_it_ends Restoring Restored Exit
a_restore_owed_while_the_server_was_down_is_dropped_by_a_newer_start Stopped Changed AlwaysKnown
exit_with_the_uplink_stopped_is_answered_at_once Stopped Exit Auto
a_link_lost_while_stopping_orders_no_restore Stopping Lost Off
";

/// The tests of the uplink suites whose case no column holds: no bud worn, no model number, the settings file.
const OUTSIDE_THE_TABLE: [&str; 4] = [
    "no_bud_worn_sends_no_start",
    "always_sends_no_start_without_a_model_number",
    "a_settings_file_with_a_bad_model_entry_is_refused",
    "a_seventeenth_model_drops_the_oldest",
];

fn phase(kind: Kind) -> Phase {
    match kind {
        Kind::Stopped => Phase::Stopped { spent: None },
        Kind::Failed => Phase::Stopped {
            spent: Some(Spent::Failed),
        },
        Kind::Unsupported => Phase::Stopped {
            spent: Some(Spent::Unsupported),
        },
        Kind::Starting => Phase::Starting {
            since: SENT,
            owed: false,
        },
        Kind::Owed => Phase::Starting {
            since: SENT,
            owed: true,
        },
        Kind::Running => Phase::Running,
        Kind::Stopping => Phase::Stopping { since: SENT },
        Kind::Restoring => Phase::Restoring { since: SENT },
    }
}

fn kind(phase: Phase) -> Kind {
    match phase {
        Phase::Stopped { spent: None } => Kind::Stopped,
        Phase::Stopped {
            spent: Some(Spent::Failed),
        } => Kind::Failed,
        Phase::Stopped {
            spent: Some(Spent::Unsupported),
        } => Kind::Unsupported,
        Phase::Starting { owed: false, .. } => Kind::Starting,
        Phase::Starting { owed: true, .. } => Kind::Owed,
        Phase::Running => Kind::Running,
        Phase::Stopping { .. } => Kind::Stopping,
        Phase::Restoring { .. } => Kind::Restoring,
    }
}

fn confirmed() -> MicModels {
    let mut models = MicModels::default();
    models.confirm(MODEL_NUMBER);
    models
}

/// The pods linked with both buds worn, as `column` says.
fn linked(column: Column) -> State {
    let (preference, mic_models) = match column {
        Column::AlwaysNew => (MicPref::Always, MicModels::default()),
        Column::AlwaysKnown | Column::Exit => (MicPref::Always, confirmed()),
        Column::Auto => (MicPref::Auto, confirmed()),
        Column::Off => (MicPref::Off, confirmed()),
    };
    State {
        connected: true,
        model_number: MODEL_NUMBER.to_owned(),
        ear: EarReport::from_in_ear(true, true),
        microphone: Microphone::new(preference),
        mic_models,
        ..State::default()
    }
}

/// The fact, the clock it comes at and the state after its step.
fn stepped(event: Event, column: Column) -> (Fact, Duration, State) {
    let mut state = linked(column);
    let (fact, at) = match event {
        Event::RunningMarker => (Fact::Link(Signal::Uplink(UplinkState::Running)), SOON),
        Event::StoppedMarker => (Fact::Link(Signal::Uplink(UplinkState::Stopped)), SOON),
        Event::Deadline => (Fact::Tick, LATER),
        Event::Down => (Fact::Link(Signal::Down), SOON),
        Event::Lost => (
            Fact::Link(Signal::Lost {
                device: device(),
                down_at: None,
            }),
            SOON,
        ),
        Event::Changed => (Fact::Changed, SOON),
        Event::Restored => (Fact::Audio(AudioFact::Restored), SOON),
        Event::Exit => (Fact::Exit, SOON),
    };
    if matches!(event, Event::Down | Event::Lost) {
        state.connected = false;
    }
    (fact, SENT.saturating_add(at), state)
}

fn requests(effects: &[Effect]) -> Vec<UplinkRequest> {
    effects
        .iter()
        .filter_map(|effect| {
            if let Effect::Uplink(request) = effect {
                Some(*request)
            } else {
                None
            }
        })
        .collect()
}

fn lines(effects: &[Effect]) -> Vec<&str> {
    effects
        .iter()
        .filter_map(|effect| {
            if let Effect::Log(line) = effect {
                Some(line.as_str())
            } else {
                None
            }
        })
        .collect()
}

fn mic(phase: Phase) -> Mic {
    Mic {
        phase,
        ..Mic::default()
    }
}

fn run(from: Phase, event: Event, column: Column) -> (Vec<Effect>, Mic) {
    let mut mic = Mic {
        exiting: column == Column::Exit,
        ..mic(from)
    };
    let (fact, now, state) = stepped(event, column);
    (mic.step(&state, &fact, now), mic)
}

#[test]
fn every_phase_against_every_fact() {
    for (from, event, outcomes) in CELLS {
        for (column, (sent, to)) in COLUMNS.into_iter().zip(outcomes) {
            let (effects, mic) = run(phase(from), event, column);
            let cell = format!("{from:?}, {event:?}, {column:?}");
            assert_eq!(requests(&effects), sent, "{cell}");
            assert_eq!(kind(mic.phase), to, "{cell}");
            let exiting = column == Column::Exit || event == Event::Exit;
            let stopped = matches!(to, Kind::Stopped | Kind::Failed | Kind::Unsupported);
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
fn test_names(suite: &str) -> Vec<&str> {
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
        include_str!("../../../../daemon/tests/uplink.rs"),
        include_str!("../../../../daemon/tests/uplink_stop.rs"),
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

#[test]
fn a_start_and_its_answer_log_their_lines_and_the_time_it_took() {
    let mut mic = Mic::default();
    let (fact, _, state) = stepped(Event::Changed, Column::AlwaysNew);
    assert_eq!(lines(&mic.step(&state, &fact, SENT)), [START_SENT_LINE]);
    let (fact, _, state) = stepped(Event::RunningMarker, Column::AlwaysNew);
    let effects = mic.step(&state, &fact, SENT.saturating_add(SOON));
    assert_eq!(lines(&effects), ["mic: uplink running after 100 ms"]);
}

#[test]
fn off_while_live_logs_the_stop_and_its_answer() {
    let mut mic = mic(phase(Kind::Running));
    let (fact, _, state) = stepped(Event::Changed, Column::Off);
    assert_eq!(lines(&mic.step(&state, &fact, SENT)), [Word::Off.line()]);
    let (fact, _, state) = stepped(Event::StoppedMarker, Column::Off);
    let effects = mic.step(&state, &fact, SENT.saturating_add(SOON));
    assert_eq!(lines(&effects), ["mic: uplink stopped after 100 ms"]);
}

#[test]
fn the_owed_stop_is_logged_once_per_start() {
    let mut mic = mic(phase(Kind::Starting));
    let (fact, now, state) = stepped(Event::Changed, Column::Off);
    assert_eq!(lines(&mic.step(&state, &fact, now)), [STOP_OWED_LINE]);
    assert_eq!(lines(&mic.step(&state, &fact, now)), Vec::<&str>::new());
}

#[test]
fn an_unanswered_start_logs_why_it_is_spent() {
    for (column, wanted) in [
        (Column::AlwaysNew, vec![UNANSWERED_LINE, NO_UPLINK_LINE]),
        (Column::AlwaysKnown, vec![UNANSWERED_LINE, FAILED_LINE]),
        (Column::Off, vec![UNANSWERED_LINE]),
    ] {
        let (effects, _) = run(phase(Kind::Starting), Event::Deadline, column);
        assert_eq!(lines(&effects), wanted, "{column:?}");
    }
}

#[test]
fn an_unanswered_stop_logs_that_it_is_not_confirmed() {
    let (effects, _) = run(phase(Kind::Stopping), Event::Deadline, Column::Off);
    assert_eq!(lines(&effects), [STOP_UNCONFIRMED_LINE]);
}

#[test]
fn each_answer_window_ends_at_its_deadline_and_not_before() {
    for (from, window, to) in [
        (Kind::Owed, START_ANSWER_MS, Kind::Stopped),
        (Kind::Stopping, STOP_ANSWER_MS, Kind::Restoring),
        (Kind::Restoring, RESTORE_WAIT_MS, Kind::Stopped),
    ] {
        let deadline = SENT.saturating_add(Duration::from_millis(window));
        let mut mic = mic(phase(from));
        assert_eq!(mic.deadline(), Some(deadline), "{from:?}");
        let state = linked(Column::Off);
        let early = deadline.saturating_sub(Duration::from_millis(1));
        drop(mic.step(&state, &Fact::Tick, early));
        assert_eq!(kind(mic.phase), from, "{from:?}");
        drop(mic.step(&state, &Fact::Tick, deadline));
        assert_eq!(kind(mic.phase), to, "{from:?}");
    }
    for from in [Kind::Stopped, Kind::Failed, Kind::Running] {
        assert_eq!(mic(phase(from)).deadline(), None, "{from:?}");
    }
}

#[test]
fn auto_and_always_wait_for_the_model_number() {
    for column in [Column::Auto, Column::AlwaysNew] {
        let mut state = linked(column);
        state.mic_models = MicModels::default();
        state.model_number.clear();
        assert!(!wanted(&state), "{column:?}");
        state.model_number = MODEL_NUMBER.to_owned();
        assert!(wanted(&state), "{column:?}");
    }
}

#[test]
fn the_code_reads_the_link_then_a_spent_start_or_a_stop_then_the_buds_then_the_phase() {
    let worn = linked(Column::AlwaysNew);
    let in_the_case = State {
        ear: EarReport::default(),
        ..worn.clone()
    };
    let unlinked = State {
        connected: false,
        ..worn.clone()
    };
    for (from, on_worn, on_case) in [
        (Kind::Stopped, MicCode::Idle, MicCode::NotWorn),
        (Kind::Failed, MicCode::Failed, MicCode::Failed),
        (
            Kind::Unsupported,
            MicCode::Unsupported,
            MicCode::Unsupported,
        ),
        (Kind::Starting, MicCode::Starting, MicCode::NotWorn),
        (Kind::Owed, MicCode::Starting, MicCode::NotWorn),
        (Kind::Running, MicCode::Live, MicCode::NotWorn),
        (Kind::Stopping, MicCode::Stopping, MicCode::Stopping),
        (Kind::Restoring, MicCode::Stopping, MicCode::Stopping),
    ] {
        let mic = mic(phase(from));
        assert_eq!(mic.code(&worn), on_worn, "{from:?}");
        assert_eq!(mic.code(&in_the_case), on_case, "{from:?}");
        assert_eq!(mic.code(&unlinked), MicCode::Idle, "{from:?}");
    }
}

#[test]
fn the_requests_write_the_sample_bytes() {
    assert_eq!(UplinkRequest::Start.sdu(), pods_samples::uplink::START);
    assert_eq!(UplinkRequest::Stop.sdu(), pods_samples::uplink::STOP);
}

#[test]
fn the_exit_is_answered_once_the_uplink_is_stopped_and_restored() {
    let state = linked(Column::Exit);
    let mut stopped = Mic::default();
    assert!(
        stopped
            .step(&state, &Fact::Exit, SENT)
            .contains(&Effect::Exited)
    );
    let mut live = mic(phase(Kind::Running));
    let effects = live.step(&state, &Fact::Exit, SENT);
    assert_eq!(requests(&effects), STOP);
    assert!(!effects.contains(&Effect::Exited));
    let marker = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    assert!(!live.step(&state, &marker, SENT).contains(&Effect::Exited));
    let restored = Fact::Audio(AudioFact::Restored);
    assert!(live.step(&state, &restored, SENT).contains(&Effect::Exited));
    let lost = Fact::Link(Signal::Lost {
        device: device(),
        down_at: None,
    });
    let mut restoring = mic(phase(Kind::Restoring));
    let exit = restoring.step(&state, &Fact::Exit, SENT);
    assert!(!exit.contains(&Effect::Exited));
    assert!(
        restoring
            .step(&state, &lost, SENT)
            .contains(&Effect::Exited)
    );
}

#[test]
fn the_restore_window_opens_when_the_stop_ends() {
    let state = linked(Column::Off);
    let marker = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    let deadline = SENT.saturating_add(Duration::from_millis(STOP_ANSWER_MS));
    for (fact, at) in [(marker, SENT.saturating_add(SOON)), (Fact::Tick, deadline)] {
        let mut stopping = mic(phase(Kind::Stopping));
        drop(stopping.step(&state, &fact, at));
        let restored_by = at.saturating_add(Duration::from_millis(RESTORE_WAIT_MS));
        assert_eq!(stopping.deadline(), Some(restored_by), "{fact:?}");
    }
}
