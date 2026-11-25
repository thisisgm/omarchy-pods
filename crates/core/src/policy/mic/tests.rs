//! Support tests for the machine the uplink suites drive through the built daemon: every phase against every fact, the lines, the deadline and the code `mic` shows.

use std::time::Duration;

use pods_proto::ear::EarReport;
use pods_samples::MODEL_NUMBER;

use super::*;
use crate::policy::tests::{device, sink_in};
use crate::prefs::MicModels;
use crate::state::Microphone;

/// When the START or STOP of every phase under test went out.
const SENT: Duration = Duration::from_secs(10);
/// Inside every window.
const SOON: Duration = Duration::from_millis(100);
/// The last bud out one grace before `SENT`, so its STOP is due at `SENT`.
const LEFT: Duration = SENT.saturating_sub(Duration::from_millis(BUD_GRACE_MS));
/// Past every window, the linger being the longest.
const LATER: Duration = Duration::from_millis(UPLINK_LINGER_MS);
const _: () = assert!(
    UPLINK_LINGER_MS > RESTORE_WAIT_MS
        && RESTORE_WAIT_MS > START_ANSWER_MS
        && RESTORE_WAIT_MS > STOP_ANSWER_MS
        && UPLINK_LINGER_MS > BUD_GRACE_MS
);

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
    /// The followed sink reported suspended.
    Quiet,
    Sleep,
    /// The wake after a sleep.
    Wake,
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
    /// Stopped by the pods, the want not yet absent since.
    Unasked,
    Starting,
    /// The last try's START out.
    LastTry,
    Owed,
    Running,
    Lingering,
    Waiting,
    Stopping,
    /// A STOP the gate let out for a quiet sink, which no restore follows.
    StoppingQuiet,
    Restoring,
    /// The restore after a stop the pods made on their own.
    RestoringUnasked,
    /// Stopped from logind's edge into sleep until the wake.
    Asleep,
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

const EVENTS: [Event; 11] = [
    Event::RunningMarker,
    Event::StoppedMarker,
    Event::Deadline,
    Event::Down,
    Event::Lost,
    Event::Changed,
    Event::Restored,
    Event::Exit,
    Event::Quiet,
    Event::Sleep,
    Event::Wake,
];

const KINDS: [Kind; 15] = [
    Kind::Stopped,
    Kind::Failed,
    Kind::Unsupported,
    Kind::Unasked,
    Kind::Starting,
    Kind::LastTry,
    Kind::Owed,
    Kind::Running,
    Kind::Lingering,
    Kind::Waiting,
    Kind::Stopping,
    Kind::StoppingQuiet,
    Kind::Restoring,
    Kind::RestoringUnasked,
    Kind::Asleep,
];

fn phase(kind: Kind) -> Phase {
    match kind {
        Kind::Stopped | Kind::Asleep => Phase::Stopped(Stopped::default()),
        Kind::Failed => Phase::Stopped(Stopped {
            spent: Some(Spent::Failed),
            pending: PendingMarker::None,
        }),
        Kind::Unsupported => Phase::Stopped(Stopped {
            spent: Some(Spent::Unsupported),
            pending: PendingMarker::None,
        }),
        Kind::Unasked => Phase::Stopped(Stopped {
            spent: Some(Spent::Unasked),
            pending: PendingMarker::None,
        }),
        Kind::Starting => Phase::Starting {
            since: SENT,
            tried: Try::FIRST,
            owed: false,
        },
        Kind::LastTry => Phase::Starting {
            since: SENT,
            tried: last_try(),
            owed: false,
        },
        Kind::Owed => Phase::Starting {
            since: SENT,
            tried: Try::FIRST,
            owed: true,
        },
        Kind::Running => Phase::Running(Hold::Wanted, watch(), Ownership::Owned),
        Kind::Lingering => Phase::Running(
            Hold::Lingering {
                since: SENT,
                unworn: None,
            },
            watch(),
            Ownership::Owned,
        ),
        Kind::Waiting => Phase::Running(Hold::Waiting { unworn: None }, watch(), Ownership::Owned),
        Kind::Stopping => Phase::Stopping {
            since: SENT,
            restore: true,
            ownership: Ownership::Owned,
            spent: None,
        },
        Kind::StoppingQuiet => Phase::Stopping {
            since: SENT,
            restore: false,
            ownership: Ownership::Owned,
            spent: None,
        },
        Kind::Restoring => Phase::Restoring {
            since: SENT,
            id: RestoreId(0),
            stopped: Stopped::default(),
        },
        Kind::RestoringUnasked => Phase::Restoring {
            since: SENT,
            id: RestoreId(0),
            stopped: Stopped {
                spent: Some(Spent::Unasked),
                pending: PendingMarker::None,
            },
        },
    }
}

/// The stall check of a run its START began at `SENT`, before its first read.
fn watch() -> Watch {
    Watch::new(Try::FIRST, SENT)
}

fn last_try() -> Try {
    let mut tried = Try::FIRST;
    while let Some(next) = tried.next() {
        tried = next;
    }
    tried
}

fn kind(phase: Phase) -> Kind {
    match phase {
        Phase::Stopped(Stopped { spent: None, .. }) => Kind::Stopped,
        Phase::Stopped(Stopped {
            spent: Some(Spent::Failed),
            ..
        }) => Kind::Failed,
        Phase::Stopped(Stopped {
            spent: Some(Spent::Unsupported),
            ..
        }) => Kind::Unsupported,
        Phase::Stopped(Stopped {
            spent: Some(Spent::Unasked),
            ..
        }) => Kind::Unasked,
        Phase::Starting {
            owed: false, tried, ..
        } if tried.next().is_none() => Kind::LastTry,
        Phase::Starting { owed: false, .. } => Kind::Starting,
        Phase::Starting { owed: true, .. } => Kind::Owed,
        Phase::Running(Hold::Wanted | Hold::Late, ..) => Kind::Running,
        Phase::Running(Hold::Lingering { .. }, ..) => Kind::Lingering,
        Phase::Running(Hold::Waiting { .. }, ..) => Kind::Waiting,
        Phase::Stopping { restore: true, .. } => Kind::Stopping,
        Phase::Stopping { restore: false, .. } => Kind::StoppingQuiet,
        Phase::Restoring {
            stopped:
                Stopped {
                    spent: Some(Spent::Unasked),
                    ..
                },
            ..
        } => Kind::RestoringUnasked,
        Phase::Restoring { .. } => Kind::Restoring,
    }
}

/// The kind of `mic`, a stop while asleep told apart from one awake.
fn kind_of(mic: &Mic) -> Kind {
    let kind = kind(mic.phase);
    if kind == Kind::Stopped && mic.asleep {
        Kind::Asleep
    } else {
        kind
    }
}

pub(in crate::policy::mic) fn confirmed() -> MicModels {
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
        Event::Restored => (Fact::Audio(AudioFact::Restored(RestoreId(0))), SOON),
        Event::Exit => (Fact::Exit, SOON),
        Event::Quiet => (sink_in(true, SinkState::Suspended), SOON),
        Event::Sleep => (Fact::Sleep(SleepEdge::Sleeping), SOON),
        Event::Wake => (Fact::Sleep(SleepEdge::Woke), SOON),
    };
    if matches!(event, Event::Down | Event::Lost) {
        state.connected = false;
    }
    (fact, SENT.saturating_add(at), state)
}

pub(in crate::policy::mic) fn requests(effects: &[Effect]) -> Vec<UplinkRequest> {
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

fn restore_ids(effects: &[Effect]) -> Vec<RestoreId> {
    effects
        .iter()
        .filter_map(|effect| {
            if let Effect::Audio(AudioRequest::Restore(id)) = effect {
                Some(*id)
            } else {
                None
            }
        })
        .collect()
}

pub(in crate::policy::mic) fn lines(effects: &[Effect]) -> Vec<&str> {
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

/// The hold of a running uplink, apart from its stall check.
pub(in crate::policy::mic) fn hold(mic: &Mic) -> Hold {
    let Phase::Running(hold, ..) = mic.phase else {
        panic!("not running: {:?}", mic.phase);
    };
    hold
}

fn mic(phase: Phase) -> Mic {
    Mic {
        phase,
        ..Mic::default()
    }
}

fn run(from: Kind, event: Event, column: Column) -> (Vec<Effect>, Mic) {
    let mut mic = Mic {
        exiting: column == Column::Exit,
        asleep: from == Kind::Asleep || event == Event::Wake,
        ..mic(phase(from))
    };
    let (fact, now, state) = stepped(event, column);
    (mic.step(&state, &fact, now), mic)
}

#[test]
fn a_start_and_its_answer_log_their_lines_and_the_time_it_took() {
    let mut mic = Mic::default();
    let (fact, _, state) = stepped(Event::Changed, Column::AlwaysNew);
    assert_eq!(lines(&mic.step(&state, &fact, SENT)), [start_sent_line(1)]);
    let (fact, _, state) = stepped(Event::RunningMarker, Column::AlwaysNew);
    let effects = mic.step(&state, &fact, SENT.saturating_add(SOON));
    assert_eq!(lines(&effects), ["mic: uplink running after 100 ms"]);
}

#[test]
fn off_while_live_logs_the_wait_once_then_the_stop_and_its_answer() {
    let mut mic = mic(phase(Kind::Running));
    let (fact, _, state) = stepped(Event::Changed, Column::Off);
    assert_eq!(
        lines(&mic.step(&state, &fact, SENT)),
        [STOP_OWED_PLAYING_LINE]
    );
    assert_eq!(lines(&mic.step(&state, &fact, SENT)), Vec::<&str>::new());
    let quiet = sink_in(true, SinkState::Suspended);
    assert_eq!(lines(&mic.step(&state, &quiet, SENT)), [Word::Quiet.line()]);
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
fn an_unanswered_last_try_logs_why_it_is_spent() {
    let last = unanswered_line(START_TRIES);
    for (column, wanted) in [
        (Column::AlwaysNew, vec![last.as_str(), NO_UPLINK_LINE]),
        (Column::AlwaysKnown, vec![last.as_str(), FAILED_LINE]),
        (Column::Off, vec![last.as_str()]),
    ] {
        let (effects, _) = run(Kind::LastTry, Event::Deadline, column);
        assert_eq!(lines(&effects), wanted, "{column:?}");
    }
}

#[test]
fn an_unanswered_stop_logs_that_it_is_not_confirmed() {
    let (effects, _) = run(Kind::Stopping, Event::Deadline, Column::Off);
    assert_eq!(lines(&effects), [STOP_UNCONFIRMED_LINE]);
}

#[test]
fn each_answer_window_ends_at_its_deadline_and_not_before() {
    for (from, window, to) in [
        (Kind::Owed, START_ANSWER_MS, Kind::Stopped),
        (Kind::Stopping, STOP_ANSWER_MS, Kind::Restoring),
        (Kind::StoppingQuiet, STOP_ANSWER_MS, Kind::Stopped),
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
    for from in [Kind::Stopped, Kind::Failed] {
        assert_eq!(mic(phase(from)).deadline(), None, "{from:?}");
    }
    // A running uplink's only deadline is its stall check, the first read due at once.
    for from in [Kind::Running, Kind::Waiting] {
        assert_eq!(mic(phase(from)).deadline(), Some(SENT), "{from:?}");
    }
}

#[test]
fn auto_and_always_wait_for_the_model_number() {
    for column in [Column::Auto, Column::AlwaysNew] {
        let mut state = linked(column);
        state.mic_models = MicModels::default();
        state.model_number.clear();
        assert!(!wanted(&state, false), "{column:?}");
        state.model_number = MODEL_NUMBER.to_owned();
        assert!(wanted(&state, false), "{column:?}");
    }
}

#[test]
fn the_code_reads_the_link_then_a_spent_start_or_a_stop_then_the_buds_then_the_phase() {
    use MicCode::{Failed, Idle, Live, NotWorn, Off, Starting, Stopping, Unsupported};

    let worn = linked(Column::AlwaysNew);
    let in_the_case = State {
        ear: EarReport::default(),
        ..worn.clone()
    };
    let unlinked = State {
        connected: false,
        ..worn.clone()
    };
    let mut off = linked(Column::Off);
    let mut off_unlinked = State {
        connected: false,
        ..off.clone()
    };
    for (from, on_worn, on_case, on_off) in [
        (Kind::Stopped, Idle, NotWorn, Off),
        (Kind::Failed, Failed, Failed, Off),
        (Kind::Unsupported, Unsupported, Unsupported, Off),
        (Kind::Unasked, Idle, NotWorn, Off),
        (Kind::Starting, Starting, NotWorn, Off),
        (Kind::LastTry, Starting, NotWorn, Off),
        (Kind::Owed, Starting, NotWorn, Stopping),
        (Kind::Running, Live, NotWorn, Stopping),
        (Kind::Lingering, Live, NotWorn, Stopping),
        (Kind::Waiting, Live, NotWorn, Stopping),
        (Kind::Stopping, Stopping, Stopping, Stopping),
        (Kind::StoppingQuiet, Stopping, Stopping, Stopping),
        (Kind::Restoring, Stopping, Stopping, Stopping),
        (Kind::RestoringUnasked, Stopping, Stopping, Stopping),
    ] {
        let mut mic = mic(phase(from));
        assert_eq!(mic.code(&worn), on_worn, "{from:?}");
        assert_eq!(mic.code(&in_the_case), on_case, "{from:?}");
        assert_eq!(mic.code(&unlinked), Idle, "{from:?}");

        // A source failure would mask a pending STOP if the off branch fell through.
        mic.node
            .step(&off, &Fact::Mic(MicFact::Broken), SENT, &mut Vec::new());
        mic.show(&mut off);
        assert_eq!(off.microphone.code(), on_off, "{from:?}, off and linked");
        mic.show(&mut off_unlinked);
        assert_eq!(
            off_unlinked.microphone.code(),
            Off,
            "{from:?}, off and unlinked"
        );
    }
}

#[test]
fn a_headset_profile_holds_a_due_stop_until_it_clears() {
    let state = State {
        audio_server: AudioServer::Up,
        ..linked(Column::Auto)
    };
    let mut mic = mic(phase(Kind::Running));
    mic.step(&state, &Fact::Audio(AudioFact::Hint(false)), SENT);
    mic.step(&state, &Fact::Audio(AudioFact::Headset(true)), SENT);
    let due = SENT.saturating_add(LATER);
    assert_eq!(mic.deadline(), None);
    assert_eq!(
        requests(&mic.step(&state, &Fact::Audio(AudioFact::Sink(None)), due)),
        []
    );
    assert_eq!(requests(&mic.step(&state, &Fact::Tick, due)), []);
    assert_eq!(mic.code(&state), MicCode::Blocked);
    assert_eq!(
        requests(&mic.step(&state, &Fact::Audio(AudioFact::Headset(false)), due)),
        [UplinkRequest::Stop]
    );
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
    assert_eq!(requests(&effects), NOTHING);
    assert!(!effects.contains(&Effect::Exited) && live.stop_waits());
    let unworn = State {
        ear: EarReport::default(),
        ..state.clone()
    };
    let due = SENT.saturating_add(Duration::from_millis(BUD_GRACE_MS));
    drop(live.step(&unworn, &Fact::Changed, SENT));
    let early = due.saturating_sub(Duration::from_millis(1));
    assert_eq!(requests(&live.step(&unworn, &Fact::Tick, early)), NOTHING);
    let effects = live.step(&unworn, &Fact::Tick, due);
    assert_eq!(requests(&effects), STOP);
    assert!(!effects.contains(&Effect::Exited) && !live.stop_waits());
    let marker = Fact::Link(Signal::Uplink(UplinkState::Stopped));
    assert!(!live.step(&unworn, &marker, due).contains(&Effect::Exited));
    let restored = Fact::Audio(AudioFact::Restored(RestoreId(0)));
    assert!(live.step(&unworn, &restored, due).contains(&Effect::Exited));
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

#[test]
fn a_late_answer_to_an_earlier_restore_leaves_the_next_one_restoring() {
    let always = linked(Column::AlwaysKnown);
    // No bud worn, so the gate lets the STOP `off` owes out at once, with a restore.
    let off = State {
        ear: EarReport::default(),
        ..linked(Column::Off)
    };
    let mut mic = mic(phase(Kind::Stopping));
    let marker = |state| Fact::Link(Signal::Uplink(state));
    let mut at = SENT;
    let first = mic.step(&off, &marker(UplinkState::Stopped), at);
    at = at.saturating_add(LATER);
    drop(mic.step(&off, &Fact::Tick, at));
    assert_eq!(requests(&mic.step(&always, &Fact::Changed, at)), START);
    drop(mic.step(&always, &marker(UplinkState::Running), at));
    assert_eq!(requests(&mic.step(&off, &Fact::Changed, at)), STOP);
    drop(mic.step(&off, &marker(UplinkState::Stopped), at));
    let late = first.iter().find_map(|effect| {
        if let Effect::Audio(AudioRequest::Restore(id)) = effect {
            Some(AudioFact::Restored(*id))
        } else {
            None
        }
    });
    drop(mic.step(&off, &Fact::Audio(late.unwrap()), at));
    assert_eq!(kind(mic.phase), Kind::Restoring);
}

mod faults;
mod linger;
mod ownership;
mod stall;
pub(super) mod table;
