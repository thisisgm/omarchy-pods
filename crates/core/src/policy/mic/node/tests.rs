//! Support tests for the source node the source suites drive through the built daemon: every kind against every fact the node reads (it reads any other as `Changed`), the name, the code `mic` shows and the uplink a recorder wants.

use std::time::Duration;

use pods_proto::ear::EarReport;
use pods_proto::model::Model;
use pods_proto::uplink::UplinkState;
use pods_samples::MODEL_NUMBER;

use super::*;
use crate::link::Signal;
use crate::policy::UplinkRequest;
use crate::policy::audio::SinkState;
use crate::policy::mic::tests::{confirmed, hold, lines, requests};
use crate::policy::mic::{Mic, STOP_OWED_PLAYING_LINE, UPLINK_LINGER_MS};
use crate::policy::tests::{device, sink_in};
use crate::prefs::MicModels;
use crate::state::Microphone;

/// When the last bud of a node in its grace left.
const SENT: Duration = Duration::from_secs(10);
/// Inside the grace.
const SOON: Duration = Duration::from_millis(100);
const GRACE: Duration = Duration::from_millis(BUD_GRACE_MS);
/// The open every kind but `Closed` and `Broken` answers to.
const CURRENT: OpenId = OpenId(0);
/// An earlier open, whose facts the node must drop.
const EARLIER: OpenId = OpenId(7);
/// The counter past both ids, so a reopen takes neither.
const NEXT_ID: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Closed,
    Opening,
    Up,
    Down,
    /// Up, a recorder linked, a bud worn.
    Recording,
    /// Up, the last bud out at `SENT`.
    Grace,
    /// Up, a recorder linked, no bud worn and the grace over.
    RecordingOut,
    Broken,
}

/// Each with the pods linked, a confirmed model, `auto` and both buds worn, unless it says otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    Changed,
    Opened,
    Refused,
    Failed,
    Streaming,
    Paused,
    /// The recorder gone with no bud worn.
    PausedOut,
    BudOut,
    SilentUnworn,
    SilentWorn,
    /// The clock at the end of the grace of `Grace`, no bud worn.
    Deadline,
    Lost,
    Off,
    Exit,
    /// An `Up` of an earlier open.
    StaleUp,
    /// A `Down` of an earlier open.
    StaleDown,
    /// A recorder on an earlier open.
    StaleStreaming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sent {
    Nothing,
    Open,
    Close,
}

const KINDS: [Kind; 8] = [
    Kind::Closed,
    Kind::Opening,
    Kind::Up,
    Kind::Down,
    Kind::Recording,
    Kind::Grace,
    Kind::RecordingOut,
    Kind::Broken,
];

const EVENTS: [Event; 17] = [
    Event::Changed,
    Event::Opened,
    Event::Refused,
    Event::Failed,
    Event::Streaming,
    Event::Paused,
    Event::PausedOut,
    Event::BudOut,
    Event::SilentUnworn,
    Event::SilentWorn,
    Event::Deadline,
    Event::Lost,
    Event::Off,
    Event::Exit,
    Event::StaleUp,
    Event::StaleDown,
    Event::StaleStreaming,
];

static CELLS: [(Kind, Event, Sent, Kind); 136] = {
    use Event::{
        BudOut, Changed, Deadline, Exit, Failed, Lost, Off, Opened, Paused, PausedOut, Refused,
        SilentUnworn, SilentWorn, StaleDown, StaleStreaming, StaleUp, Streaming,
    };
    use Kind::{Broken, Closed, Down, Grace, Opening, Recording, RecordingOut, Up};
    use Sent::{Close, Nothing, Open};
    [
        (Closed, Changed, Open, Opening),
        (Closed, Opened, Open, Opening),
        (Closed, Refused, Open, Opening),
        (Closed, Failed, Nothing, Broken),
        (Closed, Streaming, Open, Opening),
        (Closed, Paused, Open, Opening),
        (Closed, PausedOut, Open, Opening),
        (Closed, BudOut, Open, Opening),
        (Closed, SilentUnworn, Open, Opening),
        (Closed, SilentWorn, Open, Opening),
        (Closed, Deadline, Open, Opening),
        (Closed, Lost, Nothing, Closed),
        (Closed, Off, Nothing, Closed),
        (Closed, Exit, Nothing, Closed),
        (Closed, StaleUp, Open, Opening),
        (Closed, StaleDown, Open, Opening),
        (Closed, StaleStreaming, Open, Opening),
        (Opening, Changed, Nothing, Opening),
        (Opening, Opened, Nothing, Up),
        (Opening, Refused, Nothing, Down),
        (Opening, Failed, Nothing, Broken),
        (Opening, Streaming, Nothing, Opening),
        (Opening, Paused, Nothing, Opening),
        (Opening, PausedOut, Nothing, Opening),
        (Opening, BudOut, Nothing, Opening),
        (Opening, SilentUnworn, Nothing, Opening),
        (Opening, SilentWorn, Nothing, Opening),
        (Opening, Deadline, Nothing, Opening),
        (Opening, Lost, Close, Closed),
        (Opening, Off, Close, Closed),
        (Opening, Exit, Close, Closed),
        (Opening, StaleUp, Nothing, Opening),
        (Opening, StaleDown, Nothing, Opening),
        (Opening, StaleStreaming, Nothing, Opening),
        (Up, Changed, Nothing, Up),
        (Up, Opened, Nothing, Up),
        (Up, Refused, Nothing, Down),
        (Up, Failed, Nothing, Broken),
        (Up, Streaming, Nothing, Recording),
        (Up, Paused, Nothing, Up),
        (Up, PausedOut, Nothing, Grace),
        (Up, BudOut, Nothing, Grace),
        (Up, SilentUnworn, Nothing, Grace),
        (Up, SilentWorn, Nothing, Up),
        (Up, Deadline, Nothing, Grace),
        (Up, Lost, Close, Closed),
        (Up, Off, Close, Closed),
        (Up, Exit, Close, Closed),
        (Up, StaleUp, Nothing, Up),
        (Up, StaleDown, Nothing, Up),
        (Up, StaleStreaming, Nothing, Up),
        (Down, Changed, Nothing, Down),
        (Down, Opened, Nothing, Up),
        (Down, Refused, Nothing, Down),
        (Down, Failed, Nothing, Broken),
        (Down, Streaming, Nothing, Down),
        (Down, Paused, Nothing, Down),
        (Down, PausedOut, Nothing, Down),
        (Down, BudOut, Nothing, Down),
        (Down, SilentUnworn, Nothing, Down),
        (Down, SilentWorn, Nothing, Down),
        (Down, Deadline, Nothing, Down),
        (Down, Lost, Close, Closed),
        (Down, Off, Close, Closed),
        (Down, Exit, Close, Closed),
        (Down, StaleUp, Nothing, Down),
        (Down, StaleDown, Nothing, Down),
        (Down, StaleStreaming, Nothing, Down),
        (Recording, Changed, Nothing, Recording),
        (Recording, Opened, Nothing, Up),
        (Recording, Refused, Nothing, Down),
        (Recording, Failed, Nothing, Broken),
        (Recording, Streaming, Nothing, Recording),
        (Recording, Paused, Nothing, Up),
        (Recording, PausedOut, Nothing, Grace),
        (Recording, BudOut, Nothing, RecordingOut),
        (Recording, SilentUnworn, Nothing, RecordingOut),
        (Recording, SilentWorn, Nothing, Recording),
        (Recording, Deadline, Nothing, RecordingOut),
        (Recording, Lost, Close, Closed),
        (Recording, Off, Close, Closed),
        (Recording, Exit, Close, Closed),
        (Recording, StaleUp, Nothing, Recording),
        (Recording, StaleDown, Nothing, Recording),
        (Recording, StaleStreaming, Nothing, Recording),
        (Grace, Changed, Nothing, Up),
        (Grace, Opened, Nothing, Up),
        (Grace, Refused, Nothing, Down),
        (Grace, Failed, Nothing, Broken),
        (Grace, Streaming, Nothing, Recording),
        (Grace, Paused, Nothing, Up),
        (Grace, PausedOut, Nothing, Grace),
        (Grace, BudOut, Nothing, Grace),
        (Grace, SilentUnworn, Nothing, Grace),
        (Grace, SilentWorn, Nothing, Up),
        (Grace, Deadline, Close, Closed),
        (Grace, Lost, Close, Closed),
        (Grace, Off, Close, Closed),
        (Grace, Exit, Close, Closed),
        (Grace, StaleUp, Nothing, Up),
        (Grace, StaleDown, Nothing, Up),
        (Grace, StaleStreaming, Nothing, Up),
        (RecordingOut, Changed, Nothing, Recording),
        (RecordingOut, Opened, Nothing, Up),
        (RecordingOut, Refused, Nothing, Down),
        (RecordingOut, Failed, Nothing, Broken),
        (RecordingOut, Streaming, Nothing, Recording),
        (RecordingOut, Paused, Nothing, Up),
        (RecordingOut, PausedOut, Close, Closed),
        (RecordingOut, BudOut, Nothing, RecordingOut),
        (RecordingOut, SilentUnworn, Close, Closed),
        (RecordingOut, SilentWorn, Nothing, Recording),
        (RecordingOut, Deadline, Nothing, RecordingOut),
        (RecordingOut, Lost, Close, Closed),
        (RecordingOut, Off, Close, Closed),
        (RecordingOut, Exit, Close, Closed),
        (RecordingOut, StaleUp, Nothing, Recording),
        (RecordingOut, StaleDown, Nothing, Recording),
        (RecordingOut, StaleStreaming, Nothing, Recording),
        (Broken, Changed, Nothing, Broken),
        (Broken, Opened, Nothing, Broken),
        (Broken, Refused, Nothing, Broken),
        (Broken, Failed, Nothing, Broken),
        (Broken, Streaming, Nothing, Broken),
        (Broken, Paused, Nothing, Broken),
        (Broken, PausedOut, Nothing, Broken),
        (Broken, BudOut, Nothing, Broken),
        (Broken, SilentUnworn, Nothing, Broken),
        (Broken, SilentWorn, Nothing, Broken),
        (Broken, Deadline, Nothing, Broken),
        (Broken, Lost, Nothing, Broken),
        (Broken, Off, Nothing, Broken),
        (Broken, Exit, Nothing, Broken),
        (Broken, StaleUp, Nothing, Broken),
        (Broken, StaleDown, Nothing, Broken),
        (Broken, StaleStreaming, Nothing, Broken),
    ]
};

/// The end to end tests and the cells each drives, as `test Kind Event`.
const COVERED: &str = "\
a_worn_bud_brings_the_node_up_with_its_name_and_latency Closed Changed
a_worn_bud_brings_the_node_up_with_its_name_and_latency Opening Opened
no_property_of_the_node_holds_an_address_or_the_device_name Opening Opened
the_first_recorder_starts_the_uplink_cold Up Streaming
two_recorders_inside_the_linger_send_one_start_and_no_stop Recording Paused
two_recorders_inside_the_linger_send_one_start_and_no_stop Up Streaming
the_node_has_no_link_and_does_not_run_with_no_recorder Up Changed
the_preference_off_removes_the_node_and_stops_the_uplink Recording Off
the_link_lost_removes_the_node_and_ends_the_recorder_cleanly Recording Lost
with_no_pipewire_the_code_is_no_audio_server_and_the_link_is_kept Opening Refused
an_outage_logs_its_reason_once_and_a_late_server_gets_the_node Opening Refused
an_outage_logs_its_reason_once_and_a_late_server_gets_the_node Down Refused
an_outage_logs_its_reason_once_and_a_late_server_gets_the_node Down Opened
the_last_bud_out_withdraws_the_node_after_the_grace Up BudOut
the_last_bud_out_withdraws_the_node_after_the_grace Grace Deadline
buds_out_withdraws_a_stopped_stream_while_the_recorder_is_linked RecordingOut SilentUnworn
buds_out_keeps_a_talking_uplink_then_withdraws_on_silence RecordingOut SilentUnworn
buds_out_keeps_the_node_if_one_is_worn_or_returns_in_time Recording BudOut
turns_of_the_setting_while_an_open_hangs_end_on_the_last Opening Off
turns_of_the_setting_while_an_open_hangs_end_on_the_last Closed Changed
a_recorder_kept_through_a_stop_by_the_pods_starts_again_only_when_it_links_again Recording Paused
a_recorder_kept_through_a_stop_by_the_pods_starts_again_only_when_it_links_again Up Streaming
";

/// Source tests outside the node policy: card properties, daemon exit and log contents.
const OUTSIDE_THE_TABLE: [&str; 7] = [
    "a_relinked_node_carries_no_card_id_from_the_link_before",
    "a_reopened_channel_labels_the_node_only_with_the_new_card_id",
    "an_audio_server_outage_clears_the_card_id_until_the_server_returns",
    "bluetooth_card_appearing_late_labels_the_existing_mic_node",
    "bluetooth_card_reconnecting_updates_the_same_mic_node",
    "exit_removes_the_node",
    "the_log_holds_no_address_and_no_name",
];

/// The pods linked with both buds worn, a confirmed model and `auto`.
fn linked() -> State {
    State {
        connected: true,
        model: Model::from_number(MODEL_NUMBER),
        model_number: MODEL_NUMBER.to_owned(),
        ear: EarReport::from_in_ear(true, true),
        microphone: Microphone::new(MicPref::Auto),
        mic_models: confirmed(),
        ..State::default()
    }
}

fn node(kind: Kind) -> Node {
    let buds = match kind {
        Kind::Grace => Buds::Left(SENT),
        Kind::RecordingOut => Buds::Out,
        Kind::Closed | Kind::Opening | Kind::Up | Kind::Down | Kind::Recording | Kind::Broken => {
            Buds::Worn
        }
    };
    let source = match kind {
        Kind::Closed => Source::Closed,
        Kind::Broken => Source::Broken,
        Kind::Opening => Source::Opening(CURRENT),
        Kind::Down => Source::Down(CURRENT),
        Kind::Up | Kind::Grace => Source::Up {
            id: CURRENT,
            capture: Capture::Paused,
        },
        Kind::Recording | Kind::RecordingOut => Source::Up {
            id: CURRENT,
            capture: Capture::Recording,
        },
    };
    Node {
        source,
        next_id: NEXT_ID,
        buds,
        silence_check: (kind == Kind::RecordingOut)
            .then_some(SENT.saturating_add(SILENCE_CHECK_INTERVAL)),
        ..Node::default()
    }
}

fn kind(node: Node) -> Kind {
    match node.source {
        Source::Broken => Kind::Broken,
        Source::Closed => Kind::Closed,
        Source::Opening(_) => Kind::Opening,
        Source::Down(_) => Kind::Down,
        Source::Up { .. } => match (node.recording(), node.buds) {
            (true, Buds::Worn) => Kind::Recording,
            (true, Buds::Left(_) | Buds::Out) => Kind::RecordingOut,
            (false, Buds::Left(_)) => Kind::Grace,
            (false, Buds::Worn) => Kind::Up,
            (false, Buds::Out) => panic!("an up node with no recorder, no bud and no grace"),
        },
    }
}

/// The fact, the clock it comes at and the state after its step.
fn stepped(event: Event) -> (Fact, Duration, State) {
    let mut state = linked();
    let mut at = SENT.saturating_add(SOON);
    let fact = match event {
        Event::Changed | Event::BudOut | Event::Off => Fact::Changed,
        Event::Opened => Fact::Mic(MicFact::Up(CURRENT)),
        Event::Refused => Fact::Mic(MicFact::Down(CURRENT)),
        Event::Failed => Fact::Mic(MicFact::Broken),
        Event::Streaming => Fact::Mic(MicFact::Recording(CURRENT, true)),
        Event::Paused | Event::PausedOut => Fact::Mic(MicFact::Recording(CURRENT, false)),
        Event::SilentUnworn | Event::SilentWorn => Fact::Mic(MicFact::OutputSilent),
        Event::StaleUp => Fact::Mic(MicFact::Up(EARLIER)),
        Event::StaleDown => Fact::Mic(MicFact::Down(EARLIER)),
        Event::StaleStreaming => Fact::Mic(MicFact::Recording(EARLIER, true)),
        Event::Deadline => {
            at = SENT.saturating_add(GRACE);
            Fact::Tick
        }
        Event::Lost => Fact::Link(Signal::Lost {
            device: device(),
            down_at: None,
        }),
        Event::Exit => Fact::Exit,
    };
    match event {
        Event::PausedOut | Event::BudOut | Event::SilentUnworn | Event::Deadline => {
            state.ear = EarReport::default();
        }
        Event::Lost => state.connected = false,
        Event::Off => state.microphone.set_preference(MicPref::Off),
        Event::Changed
        | Event::Opened
        | Event::Refused
        | Event::Failed
        | Event::Streaming
        | Event::Paused
        | Event::SilentWorn
        | Event::Exit
        | Event::StaleUp
        | Event::StaleDown
        | Event::StaleStreaming => {}
    }
    (fact, at, state)
}

fn sent(effects: &[Effect]) -> Sent {
    let asked: Vec<&MicRequest> = effects
        .iter()
        .filter_map(|effect| {
            if let Effect::Mic(request) = effect {
                Some(request)
            } else {
                None
            }
        })
        .collect();
    match asked.as_slice() {
        [] => Sent::Nothing,
        [MicRequest::Open { .. }] => Sent::Open,
        [MicRequest::Close] => Sent::Close,
        more => panic!("more than one request in a step: {more:?}"),
    }
}

#[test]
fn every_kind_against_every_fact() {
    for (from, event, wanted, to) in CELLS {
        let mut node = node(from);
        let (fact, now, state) = stepped(event);
        let mut effects = Vec::new();
        node.step(&state, &fact, now, &mut effects);
        let cell = format!("{from:?}, {event:?}");
        assert_eq!(sent(&effects), wanted, "{cell}");
        assert_eq!(kind(node), to, "{cell}");
        if from == Kind::RecordingOut && event == Event::SilentUnworn {
            assert_eq!(lines(&effects), [withdrawn_line("silent")], "{cell}");
        }
        if from == Kind::RecordingOut && event != Event::Deadline {
            let held = self::node(from).deadline().filter(|_| to == from);
            assert_eq!(node.deadline(), held, "{cell}");
        }
        // A grace that holds keeps its end; a restarted one would move it.
        if to == from && from == Kind::Grace {
            assert_eq!(node.deadline(), self::node(from).deadline(), "{cell}");
        }
    }
}

#[test]
fn a_closed_node_without_a_confirmed_model_stays_closed_on_a_silent_fact() {
    for event in [Event::SilentUnworn, Event::SilentWorn] {
        let mut node = node(Kind::Closed);
        let (fact, now, mut state) = stepped(event);
        state.mic_models = MicModels::default();
        let mut effects = Vec::new();
        node.step(&state, &fact, now, &mut effects);
        assert_eq!(sent(&effects), Sent::Nothing, "{event:?}");
        assert_eq!(kind(node), Kind::Closed, "{event:?}");
    }
}

#[test]
fn the_table_holds_each_kind_and_fact_once() {
    for from in KINDS {
        for event in EVENTS {
            let rows = CELLS
                .iter()
                .filter(|(kind, fact, ..)| *kind == from && *fact == event);
            assert_eq!(rows.count(), 1, "{from:?}, {event:?}");
        }
    }
}

#[test]
fn every_end_to_end_test_names_its_cell_or_stands_outside_the_table() {
    let suites = [
        include_str!("../../../../../daemon/tests/source.rs"),
        include_str!("../../../../../daemon/tests/source_slow.rs"),
    ];
    let cells: Vec<String> = CELLS
        .iter()
        .map(|(from, event, ..)| format!("{from:?} {event:?}"))
        .collect();
    let mut named = Vec::from(OUTSIDE_THE_TABLE);
    // Sample input: `the_first_recorder_starts_the_uplink_cold Up Streaming`.
    for line in COVERED.lines().filter(|line| !line.is_empty()) {
        let (test, cell) = line.split_once(' ').unwrap();
        assert!(cells.iter().any(|known| known == cell), "{line}");
        named.push(test);
    }
    named.sort_unstable();
    named.dedup();
    let mut tests: Vec<&str> = suites
        .into_iter()
        .flat_map(crate::policy::mic::tests::table::test_names)
        .collect();
    tests.sort_unstable();
    assert_eq!(named, tests);
}

#[test]
fn the_open_names_the_model_in_lower_case_and_describes_it() {
    let mut node = Node::default();
    let mut effects = Vec::new();
    node.step(&linked(), &Fact::Changed, SENT, &mut effects);
    let open = MicRequest::Open {
        id: OpenId(0),
        name: format!("omarchypods_input.{}", MODEL_NUMBER.to_lowercase()),
        description: "AirPods Pro 3 Microphone".to_owned(),
    };
    assert_eq!(effects, [Effect::Mic(open)]);
    let unknown = State {
        model: Model::Unknown,
        ..linked()
    };
    let MicRequest::Open { description, .. } = super::open(CURRENT, &unknown) else {
        panic!("no open");
    };
    assert_eq!(description, "AirPods Microphone");
}

#[test]
fn the_source_is_named_only_while_the_node_is_up() {
    for kind in KINDS {
        let mut state = linked();
        state.microphone.show(MicCode::Ready, 0, node(kind).up());
        let shown = state.mic_source();
        let up = matches!(
            kind,
            Kind::Up | Kind::Recording | Kind::Grace | Kind::RecordingOut
        );
        let name = if up {
            state.mic_node_name()
        } else {
            String::new()
        };
        assert_eq!(shown, name, "{kind:?}");
    }
}

#[test]
fn the_library_failure_is_logged_once_and_never_opens_again() {
    let mut node = node(Kind::Opening);
    let mut effects = Vec::new();
    node.step(&linked(), &Fact::Mic(MicFact::Broken), SENT, &mut effects);
    node.step(&linked(), &Fact::Mic(MicFact::Broken), SENT, &mut effects);
    node.step(&linked(), &Fact::Changed, SENT, &mut effects);
    assert_eq!(effects, [Effect::Log(SOURCE_BROKEN_LINE.to_owned())]);
}

#[test]
fn the_grace_is_the_deadline_and_ends_with_it() {
    let mut node = node(Kind::Up);
    let mut state = linked();
    state.ear = EarReport::default();
    let mut effects = Vec::new();
    node.step(&state, &Fact::Changed, SENT, &mut effects);
    assert_eq!(node.deadline(), Some(SENT.saturating_add(GRACE)));
    let early = SENT
        .saturating_add(GRACE)
        .saturating_sub(Duration::from_millis(1));
    node.step(&state, &Fact::Tick, early, &mut effects);
    assert_eq!(kind(node), Kind::Grace);
    node.step(
        &state,
        &Fact::Tick,
        SENT.saturating_add(GRACE),
        &mut effects,
    );
    assert_eq!(node.deadline(), None);
    let later = SENT.saturating_add(GRACE).saturating_add(SOON);
    node.step(&state, &Fact::Changed, later, &mut effects);
    node.step(&state, &Fact::Tick, later, &mut effects);
    // The one Close, and no reopen after it while no bud is worn.
    assert_eq!(sent(&effects), Sent::Close);
}

#[test]
fn an_unworn_recorder_withdraws_when_the_output_is_silent_after_grace() {
    let mut node = node(Kind::Recording);
    let mut state = linked();
    state.ear = EarReport::default();
    let mut effects = Vec::new();
    let silent = Fact::Mic(MicFact::OutputSilent);
    let due = SENT.saturating_add(GRACE);
    node.step(&state, &silent, SENT, &mut effects);
    node.step(&state, &Fact::Tick, due, &mut effects);
    assert_eq!(effects, []);
    assert_eq!(
        node.deadline(),
        Some(due.saturating_add(SILENCE_CHECK_INTERVAL))
    );

    node.step(&state, &silent, due, &mut effects);
    assert_eq!(
        effects,
        [
            Effect::Mic(MicRequest::Close),
            Effect::Log(withdrawn_line("silent")),
        ]
    );
    assert_eq!(node.deadline(), None);
}

#[test]
fn a_recorder_ending_inside_the_grace_keeps_the_node_until_the_grace_ends() {
    let mut node = node(Kind::Recording);
    let mut state = linked();
    state.ear = EarReport::default();
    let mut effects = Vec::new();
    node.step(&state, &Fact::Changed, SENT, &mut effects);
    let paused = Fact::Mic(MicFact::Recording(CURRENT, false));
    node.step(&state, &paused, SENT.saturating_add(SOON), &mut effects);
    assert_eq!(sent(&effects), Sent::Nothing);
    assert_eq!(node.deadline(), Some(SENT.saturating_add(GRACE)));
    node.step(
        &state,
        &Fact::Tick,
        SENT.saturating_add(GRACE),
        &mut effects,
    );
    assert_eq!(sent(&effects), Sent::Close);
}

#[test]
fn the_code_reads_the_library_then_the_uplink_then_the_node() {
    let state = linked();
    for (kind, code) in [
        (Kind::Closed, MicCode::Idle),
        (Kind::Opening, MicCode::Idle),
        (Kind::Up, MicCode::Ready),
        (Kind::Down, MicCode::NoAudioServer),
        (Kind::Recording, MicCode::Ready),
        (Kind::Grace, MicCode::Ready),
        (Kind::RecordingOut, MicCode::Ready),
        (Kind::Broken, MicCode::LibraryError),
    ] {
        let mic = Mic {
            node: node(kind),
            ..Mic::default()
        };
        assert_eq!(mic.code(&state), code, "{kind:?}");
        let unlinked = State {
            connected: false,
            ..state.clone()
        };
        let unlinked_code = if kind == Kind::Broken {
            MicCode::LibraryError
        } else {
            MicCode::Idle
        };
        assert_eq!(mic.code(&unlinked), unlinked_code, "{kind:?}");
    }
    let mut running = ready();
    drop(running.step(&state, &Fact::Mic(MicFact::Recording(CURRENT, true)), SENT));
    let marker = Fact::Link(Signal::Uplink(UplinkState::Running));
    drop(running.step(&state, &marker, SENT));
    assert_eq!(running.code(&state), MicCode::Recording);
    running.node.source = Source::Broken;
    assert_eq!(running.code(&state), MicCode::LibraryError);
}

/// A `Mic` with the node up under `auto` and a confirmed model, so nothing but a recorder wants the uplink.
fn ready() -> Mic {
    let mut mic = Mic::default();
    let state = linked();
    let opened = mic.step(&state, &Fact::Changed, SENT);
    let first = Effect::Mic(open(CURRENT, &state));
    assert!(opened.contains(&first), "{opened:?}");
    drop(mic.step(&state, &Fact::Mic(MicFact::Up(CURRENT)), SENT));
    assert_eq!(mic.code(&state), MicCode::Ready);
    mic
}

#[test]
fn a_recorder_starts_the_uplink_cold_and_reads_recording() {
    let state = linked();
    let mut mic = ready();
    let streaming = Fact::Mic(MicFact::Recording(CURRENT, true));
    assert_eq!(
        requests(&mic.step(&state, &streaming, SENT)),
        [UplinkRequest::Start]
    );
    assert_eq!(mic.code(&state), MicCode::Starting);
    let running = Fact::Link(Signal::Uplink(UplinkState::Running));
    assert_eq!(requests(&mic.step(&state, &running, SENT)), []);
    assert_eq!(mic.code(&state), MicCode::Recording);
}

#[test]
fn a_second_recorder_inside_the_linger_sends_nothing() {
    let state = linked();
    let mut mic = ready();
    let streaming = Fact::Mic(MicFact::Recording(CURRENT, true));
    let paused = Fact::Mic(MicFact::Recording(CURRENT, false));
    let running = Fact::Link(Signal::Uplink(UplinkState::Running));
    drop(mic.step(&state, &streaming, SENT));
    drop(mic.step(&state, &running, SENT));
    assert_eq!(requests(&mic.step(&state, &paused, SENT)), []);
    assert_eq!(mic.code(&state), MicCode::Live);
    let linger = Duration::from_millis(UPLINK_LINGER_MS);
    assert_eq!(hold(&mic).deadline(), Some(SENT.saturating_add(linger)));
    assert_eq!(
        requests(&mic.step(&state, &streaming, SENT.saturating_add(SOON))),
        []
    );
    assert_eq!(mic.code(&state), MicCode::Recording);
    assert_eq!(hold(&mic).deadline(), None);
}

#[test]
fn off_while_recording_closes_the_node_and_owes_the_stop_in_one_step() {
    let mut state = linked();
    let mut mic = ready();
    drop(mic.step(&state, &Fact::Mic(MicFact::Recording(CURRENT, true)), SENT));
    let running = Fact::Link(Signal::Uplink(UplinkState::Running));
    drop(mic.step(&state, &running, SENT));
    state.microphone.set_preference(MicPref::Off);
    let effects = mic.step(&state, &Fact::Changed, SENT);
    assert_eq!(sent(&effects), Sent::Close);
    assert_eq!(requests(&effects), []);
    assert!(
        lines(&effects).contains(&STOP_OWED_PLAYING_LINE),
        "{effects:?}"
    );
    let quiet = sink_in(true, SinkState::Suspended);
    assert_eq!(
        requests(&mic.step(&state, &quiet, SENT)),
        [UplinkRequest::Stop]
    );
}

#[test]
fn each_withdrawal_logs_its_word_and_the_first_up_its_line() {
    let up = |effects: &[Effect]| lines(effects).contains(&SOURCE_UP_LINE);
    let mut opening = node(Kind::Opening);
    let mut effects = Vec::new();
    opening.step(
        &linked(),
        &Fact::Mic(MicFact::Up(CURRENT)),
        SENT,
        &mut effects,
    );
    assert!(up(&effects), "{effects:?}");
    let mut again = Vec::new();
    opening.step(
        &linked(),
        &Fact::Mic(MicFact::Up(CURRENT)),
        SENT,
        &mut again,
    );
    assert!(!up(&again), "{again:?}");
    for (event, word) in [
        (Event::Deadline, "not_worn"),
        (Event::Off, "off"),
        (Event::Lost, "link"),
        (Event::Refused, "server"),
    ] {
        let mut node = node(if event == Event::Deadline {
            Kind::Grace
        } else {
            Kind::Up
        });
        let (fact, now, state) = stepped(event);
        let mut effects = Vec::new();
        node.step(&state, &fact, now, &mut effects);
        assert_eq!(lines(&effects), [withdrawn_line(word)], "{event:?}");
    }
    let mut exiting = node(Kind::Up);
    let mut effects = Vec::new();
    exiting.step(&linked(), &Fact::Exit, SENT, &mut effects);
    assert_eq!(sent(&effects), Sent::Close);
    assert_eq!(lines(&effects), [withdrawn_line("exit")]);
}

#[test]
fn an_old_sources_words_after_a_reopen_start_nothing() {
    let mut off = linked();
    off.microphone.set_preference(MicPref::Off);
    let state = linked();
    let mut mic = ready();
    drop(mic.step(&off, &Fact::Changed, SENT));
    let reopened = mic.step(&state, &Fact::Changed, SENT);
    let [Effect::Mic(MicRequest::Open { id, .. })] = reopened.as_slice() else {
        panic!("no reopen: {reopened:?}");
    };
    assert_ne!(*id, CURRENT);
    // The closed source's Up and recorder, read after the reopen went out.
    let mut effects = mic.step(&state, &Fact::Mic(MicFact::Up(CURRENT)), SENT);
    let streaming = Fact::Mic(MicFact::Recording(CURRENT, true));
    effects.extend(mic.step(&state, &streaming, SENT));
    assert_eq!(requests(&effects), []);
    assert_eq!(mic.code(&state), MicCode::Idle);
    drop(mic.step(&state, &Fact::Mic(MicFact::Up(*id)), SENT));
    assert_eq!(mic.code(&state), MicCode::Ready);
}
