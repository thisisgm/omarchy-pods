//! Decision 2 on a clock the test holds. Each row names the policy cell it checks.

use pods_proto::doap::ArmFailure;

use super::super::tests::device;
use super::*;
use crate::link::Signal;
use crate::state::Microphone;

const ARM: &[DictationRequest] = &[DictationRequest::Arm];
const CLOSE: &[DictationRequest] = &[DictationRequest::Close];

#[test]
fn the_provisional_mode_settle_time_is_150_ms() {
    assert_eq!(MODE_SETTLE_MS, 150);
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
