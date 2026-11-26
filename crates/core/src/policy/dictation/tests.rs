//! Decision 2 on a clock the test holds. Each row names the policy cell it checks.

use pods_proto::doap::ArmFailure;

use super::super::tests::device;
use super::*;
use crate::link::Signal;
use crate::state::Microphone;

const ARM: &[DictationRequest] = &[DictationRequest::Arm];
const CLOSE: &[DictationRequest] = &[DictationRequest::Close];

#[test]
fn the_mode_settle_time_is_90_ms() {
    assert_eq!(MODE_SETTLE_MS, 90);
}

#[test]
fn the_inhibitor_wait_after_settling_is_100_ms() {
    assert_eq!(INHIBIT_WAIT_MS, 100);
}

struct Row {
    cell: &'static str,
    connected: bool,
    preference: MicPref,
    fact: Fact,
    now: u64,
    code: MicDictation,
    deadline: Option<u64>,
    requests: &'static [DictationRequest],
    log: Option<&'static str>,
}

fn row(cell: &'static str, fact: Fact, code: MicDictation) -> Row {
    Row {
        cell,
        connected: true,
        preference: MicPref::Auto,
        fact,
        now: 0,
        code,
        deadline: None,
        requests: &[],
        log: None,
    }
}

fn up() -> Fact {
    Fact::Link(Signal::Up { device: device() })
}

fn table(rows: impl IntoIterator<Item = Row>) {
    let mut machine = Dictation::default();
    for row in rows {
        let state = State {
            connected: row.connected,
            microphone: Microphone::new(row.preference),
            ..State::default()
        };
        let mut wanted: Vec<_> = row
            .requests
            .iter()
            .copied()
            .map(Effect::Dictation)
            .collect();
        wanted.extend(row.log.map(|line| Effect::Log(line.to_owned())));
        let effects = machine.step(&state, &row.fact, Duration::from_millis(row.now));
        assert_eq!(effects, wanted, "{}", row.cell);
        assert_eq!(machine.code(&state), row.code, "{}", row.cell);
        assert_eq!(
            machine.armed(),
            row.code == MicDictation::Armed,
            "{}",
            row.cell
        );
        assert_eq!(
            machine.deadline(),
            row.deadline.map(Duration::from_millis),
            "{}",
            row.cell
        );
    }
}

#[test]
fn link_preference_and_exit_cells() {
    use MicDictation::{Armed, Arming, Off};
    table([
        Row {
            connected: false,
            ..row(
                "offline ignores a stale Armed",
                Fact::Dictation(DictationFact::Armed),
                Off,
            )
        },
        Row {
            preference: MicPref::Off,
            ..row("up with preference off", up(), Off)
        },
        Row {
            preference: MicPref::Off,
            ..row(
                "disabled ignores an adapter answer",
                Fact::Dictation(DictationFact::Armed),
                Off,
            )
        },
        Row {
            requests: ARM,
            ..row("enabled while linked starts the arm", Fact::Changed, Arming)
        },
        row("a repeated up does not reopen the channel", up(), Arming),
        row(
            "arming ignores an unrelated pod fact",
            Fact::Dictation(DictationFact::Ended),
            Arming,
        ),
        Row {
            now: 25,
            log: Some("dictation: armed after 25 ms"),
            ..row(
                "an answer arms the service",
                Fact::Dictation(DictationFact::Armed),
                Armed,
            )
        },
        row(
            "armed ignores a second answer",
            Fact::Dictation(DictationFact::Armed),
            Armed,
        ),
        row(
            "armed ignores a bearer failure before an arm",
            Fact::Dictation(DictationFact::BearerFailed),
            Armed,
        ),
        Row {
            preference: MicPref::Off,
            requests: CLOSE,
            ..row("off closes and clears armed", Fact::Changed, Off)
        },
        Row {
            requests: ARM,
            ..row("enabled again starts over", Fact::Changed, Arming)
        },
        Row {
            connected: false,
            ..row(
                "down during an arm resets it",
                Fact::Link(Signal::Down),
                Off,
            )
        },
        Row {
            requests: ARM,
            ..row("a fresh link starts over", up(), Arming)
        },
        Row {
            preference: MicPref::Off,
            requests: CLOSE,
            ..row("off closes an in-flight arm", Fact::Changed, Off)
        },
        Row {
            requests: ARM,
            ..row("a third arm after enabling", Fact::Changed, Arming)
        },
        Row {
            requests: CLOSE,
            ..row("exit closes an in-flight arm", Fact::Exit, Off)
        },
        row("exit ignores a new link", up(), Off),
    ]);
}

#[test]
fn table_failures_are_terminal_until_a_new_link() {
    use MicDictation::{Arming, Off, Unavailable};
    for (failure, cell, log) in [
        (
            ArmFailure::NoService,
            "no service",
            "dictation: unavailable (no_service)",
        ),
        (
            ArmFailure::MissingCharacteristic,
            "missing characteristic",
            "dictation: unavailable (no_service)",
        ),
        (
            ArmFailure::NoCccd,
            "missing CCCD",
            "dictation: unavailable (no_service)",
        ),
        (
            ArmFailure::NoCodec,
            "no codec",
            "dictation: unavailable (no_codec)",
        ),
        (
            ArmFailure::Refused {
                request: 0x12,
                code: 0x03,
            },
            "error response",
            "dictation: unavailable (refused)",
        ),
        (
            ArmFailure::Garbled,
            "garbled response",
            "dictation: unavailable (refused)",
        ),
        (
            ArmFailure::TooLong,
            "walk bound",
            "dictation: unavailable (refused)",
        ),
    ] {
        table([
            Row {
                requests: ARM,
                ..row("new link arms", up(), Arming)
            },
            Row {
                log: Some(log),
                ..row(
                    cell,
                    Fact::Dictation(DictationFact::ArmFailed(failure)),
                    Unavailable,
                )
            },
            row("terminal ignores timers", Fact::Tick, Unavailable),
            row(
                "terminal ignores Armed",
                Fact::Dictation(DictationFact::Armed),
                Unavailable,
            ),
            Row {
                preference: MicPref::Off,
                ..row("disabled terminal shows off", Fact::Changed, Off)
            },
            row(
                "enabling cannot retry a terminal failure on the same link",
                Fact::Changed,
                Unavailable,
            ),
            Row {
                connected: false,
                ..row(
                    "lost clears terminal failure",
                    Fact::Link(Signal::Lost {
                        device: device(),
                        down_at: None,
                    }),
                    Off,
                )
            },
            Row {
                requests: ARM,
                ..row("next link still arms", up(), Arming)
            },
        ]);
    }
}

#[test]
fn bearer_failure_cells_run_the_whole_ladder() {
    use MicDictation::{Arming, Unavailable};
    let failed = || Fact::Dictation(DictationFact::BearerFailed);
    table([
        Row {
            requests: ARM,
            ..row("new link arms immediately", up(), Arming)
        },
        Row {
            now: 10,
            deadline: Some(760),
            ..row("first refusal waits 750 ms", failed(), Arming)
        },
        Row {
            now: 759,
            deadline: Some(760),
            ..row("early tick keeps waiting", Fact::Tick, Arming)
        },
        Row {
            now: 760,
            deadline: Some(760),
            ..row("only Tick releases the wait", Fact::Changed, Arming)
        },
        Row {
            now: 760,
            deadline: Some(760),
            ..row(
                "waiting ignores stale Armed",
                Fact::Dictation(DictationFact::Armed),
                Arming,
            )
        },
        Row {
            now: 760,
            requests: ARM,
            ..row("first deadline arms", Fact::Tick, Arming)
        },
        Row {
            now: 765,
            deadline: Some(2265),
            ..row("second refusal waits 1500 ms", failed(), Arming)
        },
        Row {
            now: 2266,
            requests: ARM,
            ..row("a late tick releases the second wait", Fact::Tick, Arming)
        },
        Row {
            now: 2275,
            deadline: Some(5275),
            ..row("third refusal waits 3000 ms", failed(), Arming)
        },
        Row {
            now: 5275,
            requests: ARM,
            ..row("third deadline arms", Fact::Tick, Arming)
        },
        Row {
            now: 5280,
            log: Some("dictation: unavailable (bearer)"),
            ..row("the last refusal ends the ladder", failed(), Unavailable)
        },
        Row {
            now: 10000,
            ..row(
                "exhaustion never dials another time",
                Fact::Tick,
                Unavailable,
            )
        },
    ]);
}

#[test]
fn loss_restarts_the_ladder_and_off_cancels_the_wait() {
    use MicDictation::{Armed, Arming, Off};
    table([
        Row {
            requests: ARM,
            ..row("new link arms", up(), Arming)
        },
        row(
            "loss before Armed is not an armed bearer loss",
            Fact::Dictation(DictationFact::BearerLost),
            Arming,
        ),
        Row {
            now: 15,
            log: Some("dictation: armed after 15 ms"),
            ..row("arm answers", Fact::Dictation(DictationFact::Armed), Armed)
        },
        Row {
            now: 20,
            deadline: Some(770),
            log: Some("dictation: bearer lost, arming again"),
            ..row(
                "armed bearer loss waits 750 ms",
                Fact::Dictation(DictationFact::BearerLost),
                Arming,
            )
        },
        Row {
            now: 770,
            requests: ARM,
            ..row("first recovery dial", Fact::Tick, Arming)
        },
        Row {
            now: 780,
            deadline: Some(2280),
            ..row(
                "recovery uses the second delay after the first refusal",
                Fact::Dictation(DictationFact::BearerFailed),
                Arming,
            )
        },
        Row {
            preference: MicPref::Off,
            now: 800,
            ..row("off cancels the recovery delay", Fact::Changed, Off)
        },
        Row {
            preference: MicPref::Off,
            now: 2280,
            ..row("a cancelled timer cannot open the channel", Fact::Tick, Off)
        },
        Row {
            now: 2281,
            requests: ARM,
            ..row(
                "enable after cancellation starts immediately",
                Fact::Changed,
                Arming,
            )
        },
        Row {
            now: 2282,
            deadline: Some(3032),
            ..row(
                "the new arm has the full ladder",
                Fact::Dictation(DictationFact::BearerFailed),
                Arming,
            )
        },
        Row {
            connected: false,
            now: 2300,
            ..row(
                "link down cancels the retry clock",
                Fact::Link(Signal::Down),
                Off,
            )
        },
    ]);
}

type SessionCell = (
    u64,
    Fact,
    Option<Stop>,
    Option<MicDictation>,
    Option<DictationRequest>,
    bool,
);

fn session_table(cells: impl IntoIterator<Item = SessionCell>) {
    let mut session = Session::default();
    for (ms, fact, why, status, request, gives_up) in cells {
        let mut effects = Vec::new();
        assert_eq!(
            session.step(&fact, Duration::from_millis(ms), why, &mut effects),
            gives_up,
            "at {ms} ms"
        );
        let sent: Vec<_> = effects
            .into_iter()
            .filter_map(|effect| {
                if let Effect::Dictation(request) = effect {
                    Some(request)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(sent, Vec::from_iter(request), "at {ms} ms");
        assert_eq!(session.code(), status, "at {ms} ms");
    }
}

fn tick_session(session: &mut Session, ms: u64, stop: Option<Stop>) -> Vec<Effect> {
    let mut effects = Vec::new();
    assert!(!session.step(&Fact::Tick, Duration::from_millis(ms), stop, &mut effects));
    effects
}

fn settling_with_pending_inhibitor() -> Session {
    let mut session = Session::default();
    assert_eq!(
        tick_session(&mut session, 0, None),
        [Effect::Dictation(DictationRequest::Mode)]
    );
    session.set_inhibit_pending(true);
    session
}

#[test]
fn start_needs_both_the_mode_settle_and_a_resolved_inhibitor() {
    for resolved in [MODE_SETTLE_MS - 1, MODE_SETTLE_MS + 1] {
        let mut session = settling_with_pending_inhibitor();
        assert_eq!(tick_session(&mut session, resolved, None), []);
        session.set_inhibit_pending(false);
        let start_at = resolved.max(MODE_SETTLE_MS);
        assert_eq!(
            session.deadline(),
            Some(Duration::from_millis(MODE_SETTLE_MS))
        );
        assert!(
            tick_session(&mut session, start_at, None)
                .contains(&Effect::Dictation(DictationRequest::Start))
        );
        assert_eq!(
            session.deadline(),
            Some(Duration::from_millis(
                start_at.saturating_add(DOAP_START_ANSWER_MS)
            ))
        );
    }
}

#[test]
fn an_unresolved_inhibitor_starts_at_the_bound_and_logs_once_across_a_retry() {
    const BOUND: u64 = MODE_SETTLE_MS + INHIBIT_WAIT_MS;
    const RETRY: u64 = BOUND + 1;
    let mut session = settling_with_pending_inhibitor();
    let timeout = Effect::Log("dictation: logind inhibitor timed out, starting without it".into());
    let start = Effect::Dictation(DictationRequest::Start);
    assert_eq!(session.deadline(), Some(Duration::from_millis(BOUND)));
    assert_eq!(tick_session(&mut session, BOUND - 1, None), []);
    let effects = tick_session(&mut session, BOUND, None);
    assert!(effects.contains(&start));
    assert_eq!(
        effects.iter().filter(|effect| **effect == timeout).count(),
        1
    );

    let refused = Fact::Dictation(DictationFact::Refused {
        reason: pods_proto::doap::STOP_REASON_UNSOLICITED,
    });
    let mut effects = Vec::new();
    session.step(&refused, Duration::from_millis(RETRY), None, &mut effects);
    assert!(effects.contains(&Effect::Dictation(DictationRequest::Mode)));
    assert_eq!(tick_session(&mut session, RETRY + BOUND - 1, None), []);
    let effects = tick_session(&mut session, RETRY + BOUND, None);
    assert!(effects.contains(&start));
    assert!(!effects.contains(&timeout));
}

#[test]
fn sleep_and_recorder_departure_clear_mode_while_start_waits() {
    const NEW_MODE: u64 = MODE_SETTLE_MS + INHIBIT_WAIT_MS;
    for stop in [Stop::Sleep, Stop::Left] {
        let mut session = settling_with_pending_inhibitor();
        assert_eq!(
            tick_session(&mut session, MODE_SETTLE_MS + 1, Some(stop)),
            [Effect::Dictation(DictationRequest::ModeOff)]
        );
        assert_eq!(session.deadline(), None);
        assert_eq!(
            tick_session(&mut session, NEW_MODE, None),
            [Effect::Dictation(DictationRequest::Mode)]
        );
        assert!(
            tick_session(&mut session, NEW_MODE + MODE_SETTLE_MS, None)
                .contains(&Effect::Dictation(DictationRequest::Start))
        );
    }
}

#[test]
fn session_cells() {
    use DictationFact::{Answered, Ended, FirstAudio};
    use DictationRequest::{Mode, ModeOff, Start};
    use MicDictation::{Starting, Stopping, Streaming};

    let stop = DictationRequest::Stop {
        reason: pods_proto::doap::STOP_REASON_HOST,
    };
    let left = Some(Stop::Left);
    let exit = Some(Stop::Exit);
    let sleep = Some(Stop::Sleep);
    session_table([
        (0, Fact::Tick, None, Some(Starting), Some(Mode), false),
        (89, Fact::Tick, None, Some(Starting), None, false),
        (90, Fact::Tick, None, Some(Starting), Some(Start), false),
        (
            100,
            Fact::Dictation(FirstAudio),
            None,
            Some(Streaming),
            None,
            false,
        ),
        (
            101,
            Fact::Dictation(Answered),
            None,
            Some(Streaming),
            None,
            false,
        ),
        (140, Fact::Tick, left, Some(Stopping), Some(stop), false),
        (639, Fact::Tick, left, Some(Stopping), None, false),
        (640, Fact::Tick, left, None, Some(ModeOff), false),
        (641, Fact::Tick, None, Some(Starting), Some(Mode), false),
        (731, Fact::Tick, None, Some(Starting), Some(Start), false),
        (732, Fact::Tick, exit, Some(Stopping), None, false),
        (
            733,
            Fact::Dictation(Answered),
            exit,
            Some(Stopping),
            Some(stop),
            false,
        ),
        (
            734,
            Fact::Dictation(Ended),
            exit,
            None,
            Some(ModeOff),
            false,
        ),
        (735, Fact::Tick, None, Some(Starting), Some(Mode), false),
        (825, Fact::Tick, None, Some(Starting), Some(Start), false),
        (826, Fact::Tick, sleep, Some(Stopping), None, false),
        (
            1325,
            Fact::Dictation(Answered),
            sleep,
            Some(Stopping),
            Some(stop),
            false,
        ),
        (
            1326,
            Fact::Dictation(Ended),
            sleep,
            None,
            Some(ModeOff),
            false,
        ),
    ]);
}

#[test]
fn session_end_cells() {
    use DictationFact::{Answered, PodEvent, Refused, UnsolicitedAudio};
    use DictationRequest::{Mode, ModeOff, Start};
    use MicDictation::{Starting, Stopping, Streaming};
    use pods_proto::doap::STOP_REASON_UNSOLICITED;

    let stray = DictationRequest::Stop {
        reason: STOP_REASON_UNSOLICITED,
    };
    let left = Some(Stop::Left);
    session_table([
        (
            0,
            Fact::Dictation(UnsolicitedAudio),
            left,
            None,
            Some(stray),
            false,
        ),
        (1, Fact::Tick, None, Some(Starting), Some(Mode), false),
        (91, Fact::Tick, None, Some(Starting), Some(Start), false),
        (92, Fact::Tick, left, Some(Stopping), None, false),
        (
            93,
            Fact::Dictation(Refused {
                reason: STOP_REASON_UNSOLICITED,
            }),
            left,
            None,
            Some(ModeOff),
            false,
        ),
        (94, Fact::Tick, None, Some(Starting), Some(Mode), false),
        (184, Fact::Tick, None, Some(Starting), Some(Start), false),
        (
            185,
            Fact::Dictation(Answered),
            None,
            Some(Streaming),
            None,
            false,
        ),
        (
            186,
            Fact::Dictation(PodEvent(Event::SiriCancel)),
            left,
            None,
            Some(ModeOff),
            false,
        ),
    ]);
}

#[test]
fn exit_during_an_open_session_defers_closing_the_bearer() {
    let mut dictation = Dictation::default();
    let state = State {
        connected: true,
        microphone: Microphone::new(MicPref::Auto),
        ..State::default()
    };
    let armed = Fact::Dictation(DictationFact::Armed);
    let answered = Fact::Dictation(DictationFact::Answered);
    let ended = Fact::Dictation(DictationFact::Ended);
    let route = Some(Route::Dictation);
    let settled = Duration::from_millis(MODE_SETTLE_MS);

    assert_eq!(
        dictation.step(&state, &up(), Duration::ZERO),
        [Effect::Dictation(DictationRequest::Arm)]
    );
    let mut effects = dictation.step(&state, &armed, Duration::ZERO);
    dictation.session(&state, &armed, Duration::ZERO, route, &mut effects);
    dictation.session(&state, &Fact::Tick, settled, route, &mut effects);
    dictation.session(&state, &answered, settled, route, &mut effects);
    assert_eq!(dictation.code(&state), MicDictation::Streaming);

    let stop = DictationRequest::Stop {
        reason: pods_proto::doap::STOP_REASON_HOST,
    };
    for (fact, keep, requests) in [
        (&Fact::Exit, true, &[stop][..]),
        (
            &ended,
            false,
            &[DictationRequest::ModeOff, DictationRequest::Close][..],
        ),
    ] {
        let mut effects = dictation.step(&state, fact, settled);
        assert_eq!(effects, []);
        dictation.session(&state, fact, settled, route, &mut effects);
        assert_eq!(dictation.armed(), keep);
        let sent: Vec<_> = effects
            .into_iter()
            .filter_map(|effect| {
                if let Effect::Dictation(request) = effect {
                    Some(request)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(sent, requests);
    }
}

#[test]
fn a_refusal_with_another_reason_is_logged() {
    const REASON: u8 = 9;
    let settled = Duration::from_millis(MODE_SETTLE_MS);
    let mut session = Session::default();
    let mut effects = Vec::new();
    session.step(&Fact::Tick, Duration::ZERO, None, &mut effects);
    session.step(&Fact::Tick, settled, None, &mut effects);
    effects.clear();
    let fact = Fact::Dictation(DictationFact::Refused { reason: REASON });
    assert!(!session.step(&fact, settled, None, &mut effects));
    assert_eq!(session.code(), Some(MicDictation::Starting));
    assert_eq!(
        effects,
        [Effect::Log(format!("dictation: refused (reason {REASON})"))]
    );
}

mod fallback;
