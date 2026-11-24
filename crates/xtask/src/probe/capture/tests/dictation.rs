//! The dictation stream against fake pods on two socket pairs: the walk of their table, the two logs, and what the pods ask of the host.

use std::cell::Cell;

use pods_proto::att;
use pods_proto::dictation::{DictationPacket, SESSION_ACTIVE, SESSION_IDLE, STARTED, STOPPED};
use pods_proto::doap::Handles;
use pods_proto::packets::{HANDSHAKE, REQUEST_NOTIFICATIONS, SET_SPECIFIC_FEATURES};
use pods_proto::sdulog::Direction;
use pods_proto::uplink;

use super::super::super::dictate::{HandleCheck, HandleWalk, START, STOP};
use super::super::super::script::{Event, Route, rescue};
use super::super::*;
use super::att_pods::{BURST_PACKETS, TABLE_END};
use super::case::{BOTH_CHANNELS, Case, PATIENT_MS, Report, TAIL_NS};
use super::pods::{pair, queued};
use super::steps::engine_in;
use super::table::{
    CUT_NOTIFICATION, INDICATION, MTU_REQUEST, PAGE, READ_REQUEST, SHORT_WALK_MS, WALK_SLACK_NS,
    walk_requests,
};
use super::{SHORT_NS, at};

/// How long the active marker leads the start write in the two streams here.
const LEAD_MS: u32 = 5;
/// One dictation stream in a few tens of milliseconds, in the shape of every script of the route.
pub(super) const STREAM: [Event; 5] = [
    at(0, Action::MarkActive),
    at(LEAD_MS, Action::DictationStart),
    at(40, Action::DictationStop),
    at(45, Action::MarkIdle),
    at(60, Action::End),
];
/// A stream the script never stops, so only a way out of the run can stop it.
pub(super) const OPEN_STREAM: [Event; 3] = [
    at(0, Action::MarkActive),
    at(LEAD_MS, Action::DictationStart),
    at(PATIENT_MS, Action::End),
];
/// The three requests of the handshake, which is all a run sends on the control channel before its gate.
pub(super) const HANDSHAKE_REQUESTS: [&[u8]; 3] =
    [&HANDSHAKE, &SET_SPECIFIC_FEATURES, &REQUEST_NOTIFICATIONS];

pub(super) fn dictation_case(events: &[Event]) -> Case {
    let mut case = Case::new(Opening::Handshake, events);
    case.wired = BOTH_CHANNELS;
    case
}

pub(super) fn stream_kinds(report: &Report) -> Vec<&str> {
    let kinds = report.kinds().into_iter();
    let stream = [
        "active",
        "dictation-start",
        "start",
        "stop",
        "dictation-stop",
        "idle",
        "end",
    ];
    kinds.filter(|kind| stream.contains(kind)).collect()
}

pub(super) struct TwoBench<T> {
    pub(super) result: T,
    /// Whether what the guards sent as they dropped was sent and recorded.
    pub(super) recorded: Result<(), IoFailure>,
    /// The packets the far end of the control channel got, in order.
    pub(super) got: Vec<Vec<u8>>,
    /// The packets the far end of the attribute channel got, in order; none when `drive` closed it.
    pub(super) att_got: Vec<Vec<u8>>,
    pub(super) dictation_starts: u32,
    pub(super) dictation_stops: u32,
    pub(super) marks: String,
}

/// Hands the loop past its gate on two unplayed socket pairs to `drive`, then drops it as a return would; the clock moves by one at each reading.
pub(super) fn on_two_channels<T>(
    script: &Script,
    drive: impl FnOnce(&mut Engine<'_, '_>, &Cell<u64>, &mut Option<Channel>) -> T,
) -> TwoBench<T> {
    let (near, far) = pair();
    let (near_att, far_att) = pair();
    let mut far_att = Some(far_att);
    let clock = Cell::new(0_u64);
    let now_ns = || clock.replace(clock.get().saturating_add(1));
    let wires = Wires {
        accessory: Some(&near),
        att: &RefCell::new(Some(near_att)),
        now_ns: &now_ns,
    };
    let (mut log, mut att_log, mut handles) = (Vec::new(), Vec::new(), Vec::new());
    let (mut marks, mut cues) = (Vec::new(), Vec::new());
    let mut out = Outputs {
        log: &mut log,
        att_log: Some(&mut att_log),
        handles: Some(&mut handles),
        marks: &mut marks,
        cues: &mut cues,
    };
    let mut recorder = Recorder::new(&mut out);
    let dropped = Dropped::default();
    let interrupted = AtomicBool::new(false);
    let scripted = Phase::Scripted {
        gate_ns: 0,
        next: 0,
    };
    let result = {
        let mut engine = engine_in(
            scripted,
            wires,
            script,
            &mut recorder,
            &interrupted,
            &dropped,
        );
        drive(&mut engine, &clock, &mut far_att)
    };
    let guards = record_dropped(&mut recorder, &dropped);
    let (dictation_starts, dictation_stops) = (recorder.dictation_starts, recorder.dictation_stops);
    TwoBench {
        result,
        recorded: guards,
        got: queued(&far),
        att_got: far_att.as_ref().map_or_default(queued),
        dictation_starts,
        dictation_stops,
        marks: String::from_utf8(marks).unwrap(),
    }
}

/// A script that starts the stream and so reads the table first, with no event the bench fires by itself.
pub(super) fn bench_script() -> Script {
    Script {
        opening: Opening::Handshake,
        needs_initial_side: false,
        unchecked: false,
        events: vec![at(0, Action::DictationStart)],
    }
}

#[test]
fn pdus_go_to_att_log_and_sdus_to_sdu_log_on_one_clock() {
    let events = [
        at(0, Action::MarkActive),
        at(5, Action::DictationStart),
        at(10, Action::Start),
        at(30, Action::Stop),
        at(40, Action::DictationStop),
        at(45, Action::MarkIdle),
        at(60, Action::End),
    ];
    let report = dictation_case(&events).run();

    // Each log holds what its channel carried and nothing of the other; the pods may send on after the loop stopped reading.
    let from_att = report.att_logged(Direction::FromPods);
    assert_eq!(from_att, report.pods.att_sent[..from_att.len()]);
    assert_eq!(report.att_logged(Direction::ToPods), report.pods.att_got);
    let from_control = report.logged(Direction::FromPods);
    assert_eq!(from_control, report.pods.sent[..from_control.len()]);
    assert_eq!(report.logged(Direction::ToPods), report.pods.got());
    let mut expected = walk_requests();
    expected.extend([START.to_vec(), STOP.to_vec()]);
    assert_eq!(report.pods.att_got, expected);
    let mut expected = HANDSHAKE_REQUESTS.to_vec();
    expected.extend([
        &SESSION_ACTIVE[..],
        &uplink::START,
        &uplink::STOP,
        &SESSION_IDLE,
    ]);
    assert_eq!(report.pods.got(), expected);

    let outcome = report.outcome();
    assert_eq!(outcome.att_pdus_in, u64::try_from(from_att.len()).unwrap());
    assert_eq!(outcome.sdus_in, u64::try_from(from_control.len()).unwrap());
    let audio = from_att.iter().filter(|pdu| {
        let packet = DictationPacket::parse(pdu, &Handles::OBSERVED);
        matches!(packet, Ok(Some(DictationPacket::Audio(_))))
    });
    assert_eq!(
        outcome.audio_notifications,
        u64::try_from(audio.count()).unwrap()
    );
    // At least the packets a session opens with were logged as audio.
    let opening = u64::try_from(BURST_PACKETS).unwrap();
    assert!(outcome.audio_notifications >= opening, "{outcome:?}");
    assert!(outcome.audio_sdus > 0, "{outcome:?}");
    let counts = (outcome.starts, outcome.stops);
    assert_eq!(counts, (1, 1));
    let counts = (outcome.dictation_starts, outcome.dictation_stops);
    assert_eq!(counts, (1, 1));
    assert_eq!((outcome.truncated, outcome.att_requests_refused), (0, 0));
    assert_eq!(outcome.end, EndReason::ScriptDone);

    // One clock: every stamp of either log lies between the marks that open and close the run, and the marks of both channels are in the order of the script.
    let opened_ns = report.marks("connect")[0].0;
    let ended_ns = report.marks("end")[0].0;
    for (name, records) in [
        ("sdu.log", report.records()),
        ("att.log", report.att_records()),
    ] {
        let stamps: Vec<u64> = records.iter().map(|record| record.at_ns).collect();
        assert!(stamps.is_sorted(), "{name}: {stamps:?}");
        let inside = stamps
            .iter()
            .all(|at_ns| (opened_ns..=ended_ns).contains(at_ns));
        assert!(inside, "{name}: outside {opened_ns} to {ended_ns}");
    }
    let order = [
        "active",
        "dictation-start",
        "start",
        "stop",
        "dictation-stop",
        "idle",
        "end",
    ];
    assert_eq!(stream_kinds(&report), order);
    let stamps: Vec<u64> = order.map(|kind| report.marks(kind)[0].0).to_vec();
    assert!(stamps.is_sorted(), "{stamps:?}");
    let in_att_log = |pdu: &[u8]| {
        let records = report.att_records();
        let mut sent = records.iter().filter(|record| record.sdu == pdu);
        sent.next().unwrap().at_ns
    };
    assert_eq!(in_att_log(&START), report.marks("dictation-start")[0].0);
    assert_eq!(in_att_log(&STOP), report.marks("dictation-stop")[0].0);
    assert_eq!(
        report.sent_at_ns(&uplink::START),
        report.marks("start")[0].0
    );
}

#[test]
fn the_gate_waits_for_the_walk_and_the_start_write_follows_the_marker() {
    let report = dictation_case(&STREAM).run();

    let kinds = report.kinds();
    let at_kind = |kind| kinds.iter().position(|found| *found == kind).unwrap();
    assert!(at_kind("ready") < at_kind("handles"), "{kinds:?}");
    assert!(at_kind("handles") < at_kind("gate"), "{kinds:?}");
    assert!(at_kind("gate") < at_kind("active"), "{kinds:?}");
    assert_eq!(report.mark("handles"), "present");
    assert_eq!(report.mark("active"), "session active");
    assert_eq!(report.mark("dictation-start"), "dictation start");
    assert_eq!(report.mark("dictation-stop"), "dictation stop");
    assert_eq!(report.mark("idle"), "session idle");
    // No request of the walk is out when the marker leaves: the pods answered the last one with the error that ends a table.
    let ended = att::error_response(0x04, TABLE_END + 1, att::ATTRIBUTE_NOT_FOUND);
    let mut from_pods = report.pods.att_sent.iter();
    let walk_end = from_pods.position(|pdu| *pdu == ended);
    assert_eq!(walk_end, Some(usize::from(TABLE_END / PAGE)));
    let led_ns = report.marks("dictation-start")[0].0 - report.marks("active")[0].0;
    let script_ns = u64::from(LEAD_MS) * NS_PER_MS;
    // The start write leaves not before the script's lead and not after the slack a shared host gets.
    let on_time = script_ns..=script_ns.saturating_add(WALK_SLACK_NS);
    assert!(on_time.contains(&led_ns), "the marker led by {led_ns} ns");
    // The list of the table has every attribute the pods listed, in order.
    let lines: Vec<&str> = report.handles.lines().collect();
    assert_eq!(lines.len(), usize::from(TABLE_END), "{}", report.handles);
    assert_eq!(lines[0], "attribute\t0x0001\t0x2b2a");
    assert_eq!(lines[6], "attribute\t0x0007\t0x2b2a");
    assert_eq!(report.outcome().end, EndReason::ScriptDone);
    assert!(report.returned_ns - report.marks("idle")[0].0 >= TAIL_NS);
}

#[test]
fn started_and_stopped_replies_are_marked_with_their_delay() {
    let bench = on_two_channels(&bench_script(), |engine, clock, _far| {
        engine.checked = true;
        // A reply with no write of this run before it is marked as that.
        engine.on_pdu(50, &STARTED).unwrap();
        clock.set(100);
        engine.dictation_start().unwrap();
        engine.on_pdu(163, &STARTED).unwrap();
        clock.set(200);
        engine.dictation_stop().unwrap();
        engine.on_pdu(245, &STOPPED).unwrap();
    });

    let replies: Vec<&str> = bench
        .marks
        .lines()
        .filter(|line| line.contains("\tstarted\t") || line.contains("\tstopped\t"))
        .collect();
    let expected = [
        "50\tstarted\tno start write was sent by this run",
        "163\tstarted\t63 ns after the start write",
        "245\tstopped\t45 ns after the stop write",
    ];
    assert_eq!(replies, expected, "{}", bench.marks);
    assert_eq!(bench.att_got, [&START[..], &STOP[..]]);

    // The real loop marks the reply of the fake pods with the time since its own start write.
    let report = dictation_case(&STREAM).run();
    let (at_ns, text) = report.marks("started")[0];
    let delay = text.strip_suffix(" ns after the start write").unwrap();
    let delay: u64 = delay.parse().unwrap();
    assert_eq!(delay, at_ns - report.marks("dictation-start")[0].0);
}

#[test]
fn dictation_start_is_refused_without_the_handle_check() {
    let without = |missing: u16| -> Vec<u16> {
        let table = 1..=TABLE_END;
        table.filter(|handle| *handle != missing).collect()
    };
    let mut lacking = dictation_case(&STREAM);
    lacking.pods.att.table = Some(without(0x000a));
    let mut silent = dictation_case(&STREAM);
    silent.pods.att.table = None;
    silent.timing.handle_walk_ms = SHORT_WALK_MS;
    let mut empty = dictation_case(&STREAM);
    empty.pods.att.table = Some(Vec::new());
    let missing = HandleCheck::Missing(vec![0x000a]);
    for (name, case, check, mark, requests) in [
        ("a handle not listed", lacking, missing, "missing 0x000a", 7),
        ("no answer", silent, HandleCheck::NoListing, "no listing", 1),
        (
            "an error for an answer",
            empty,
            HandleCheck::NoListing,
            "no listing",
            1,
        ),
    ] {
        let report = case.run();
        // No marker and no write reach the pods: the control channel carried the handshake and the other the walk.
        assert_eq!(report.pods.got(), HANDSHAKE_REQUESTS, "{name}");
        let att_got = &report.pods.att_got;
        assert_eq!(att_got.len(), requests, "{name}: {att_got:02x?}");
        assert!(
            att_got.iter().all(|pdu| pdu[0] == 0x04),
            "{name}: {att_got:02x?}"
        );
        assert_eq!(att_got[0], HandleWalk::first(), "{name}");
        let outcome = report.outcome();
        assert_eq!(outcome.end, EndReason::HandleCheck(check), "{name}");
        let counts = (outcome.dictation_starts, outcome.dictation_stops);
        assert_eq!(counts, (0, 0), "{name}");
        assert_eq!(report.mark("handles"), mark, "{name}");
        assert_eq!(report.marks("gate"), [], "{name}");
        assert_eq!(stream_kinds(&report), ["end"], "{name}");
    }

    // The start write itself asks for the check, whatever brought the script to it.
    let bench = on_two_channels(&bench_script(), |engine, _clock, _far| {
        engine.dictation_start()
    });
    let unchecked = EndReason::HandleCheck(HandleCheck::NoListing);
    assert_eq!(bench.result, Err(unchecked));
    assert_eq!(bench.att_got, Vec::<Vec<u8>>::new());
    assert_eq!((bench.dictation_starts, bench.dictation_stops), (0, 0));
    assert_eq!(bench.recorded, Ok(()));
}

#[test]
fn the_wait_for_each_answer_of_the_walk_is_its_own() {
    let mut silent = dictation_case(&STREAM);
    silent.pods.att.table = None;
    silent.timing.handle_walk_ms = SHORT_WALK_MS;
    let report = silent.run();
    let asked_ns = report.marks("sent").last().unwrap().0;
    let waited_ns = report.marks("handles")[0].0 - asked_ns;
    assert!(waited_ns >= SHORT_NS, "gave up after {waited_ns} ns");

    // A listing that stops being answered half way is a table with handles missing, not one the pods did not list.
    let mut script = bench_script();
    script.unchecked = true;
    let bench = on_two_channels(&script, |engine, clock, _far| {
        engine.start_walk().unwrap();
        // The clock moves on before the second request, so a wait still counted from the first is over before this one.
        let second_ns = 1_000;
        clock.set(second_ns);
        engine
            .on_pdu(second_ns, &[0x05, 0x01, 0x07, 0x00, 0x2a, 0x2b])
            .unwrap();
        let wait_ns = u64::from(Timing::REAL.handle_walk_ms) * NS_PER_MS;
        // One nanosecond short of the wait changes nothing.
        engine.end_overdue_wait(second_ns + wait_ns - 1).unwrap();
        assert!(engine.waits_on_the_table() && !engine.checked);
        let ended = engine.end_overdue_wait(second_ns + wait_ns);
        (ended, engine.checked)
    });
    let missing = HandleCheck::Missing(vec![0x000a, 0x000d, 0x0010]);
    assert_eq!(bench.result, (Err(EndReason::HandleCheck(missing)), false));
    let requests = [
        [0x04, 0x01, 0x00, 0xff, 0xff],
        [0x04, 0x08, 0x00, 0xff, 0xff],
    ];
    assert_eq!(bench.att_got, requests);
}

#[test]
fn unchecked_sends_the_start_after_no_listing_and_marks_it() {
    let mut silent = dictation_case(&STREAM);
    silent.script.unchecked = true;
    silent.pods.att.table = None;
    silent.timing.handle_walk_ms = SHORT_WALK_MS;
    silent.fresh_att = true;
    let mut empty = dictation_case(&STREAM);
    empty.script.unchecked = true;
    empty.pods.att.table = Some(Vec::new());
    let mut lost = dictation_case(&STREAM);
    lost.script.unchecked = true;
    lost.pods.att.greeting = vec![CUT_NOTIFICATION.to_vec()];
    lost.fresh_att = true;
    // A request left open, by silence or by a reply the walk could not follow, ends its channel, so only then does the start go out on a second one.
    let cases = [
        ("no answer", silent, 2, Some(1)),
        ("a reply the walk could not follow", lost, 2, Some(1)),
        ("an error for an answer", empty, 1, None),
    ];
    for (name, case, dials, fresh_from) in cases {
        let report = case.run();
        assert_eq!(report.mark("handles"), "unchecked", "{name}");
        let sent = [&HandleWalk::first()[..], &START, &STOP];
        assert_eq!(report.pods.att_got, sent, "{name}");
        assert_eq!(report.att_dials, dials, "{name}");
        assert_eq!(report.pods.att_fresh_from, fresh_from, "{name}");
        let mut expected = HANDSHAKE_REQUESTS.to_vec();
        expected.extend([&SESSION_ACTIVE[..], &SESSION_IDLE]);
        assert_eq!(report.pods.got(), expected, "{name}");
        let outcome = report.outcome();
        let counts = (outcome.dictation_starts, outcome.dictation_stops);
        assert_eq!(counts, (1, 1), "{name}");
        let opening = u64::try_from(BURST_PACKETS).unwrap();
        assert!(
            outcome.audio_notifications >= opening,
            "{name}: {outcome:?}"
        );
        assert_eq!(outcome.end, EndReason::ScriptDone, "{name}");
        assert_eq!(report.handles, "", "{name}");
    }

    // A table the pods did list is checked as ever, and the mark says what was found.
    let mut listed = dictation_case(&STREAM);
    listed.script.unchecked = true;
    let report = listed.run();
    assert_eq!(report.mark("handles"), "present");
    assert_eq!(report.outcome().end, EndReason::ScriptDone);
}

#[test]
fn unchecked_does_not_override_a_missing_handle() {
    let mut case = dictation_case(&STREAM);
    case.script.unchecked = true;
    let table = 1..=TABLE_END;
    case.pods.att.table = Some(table.filter(|handle| *handle != 0x0010).collect());
    let report = case.run();

    let missing = HandleCheck::Missing(vec![0x0010]);
    assert_eq!(report.outcome().end, EndReason::HandleCheck(missing));
    assert_eq!(report.mark("handles"), "missing 0x0010");
    assert_eq!(report.pods.got(), HANDSHAKE_REQUESTS);
    let att_got = &report.pods.att_got;
    assert!(att_got.iter().all(|pdu| pdu[0] == 0x04), "{att_got:02x?}");
    let end = report.mark("end");
    assert!(end.contains("missing 0x0010"), "{end}");
    assert!(end.ends_with("so the start write was not sent"), "{end}");
}

#[test]
fn a_request_from_the_peer_is_refused_and_counted() {
    let mut case = dictation_case(&[at(30, Action::End)]);
    case.pods.att.greeting = vec![MTU_REQUEST.to_vec(), READ_REQUEST.to_vec()];
    let report = case.run();

    // Each refusal names the opcode it answers, no attribute, and the code for a request this host does not serve.
    let refusals = [
        [0x01, 0x02, 0x00, 0x00, 0x06],
        [0x01, 0x0a, 0x00, 0x00, 0x06],
    ];
    assert_eq!(report.pods.att_got, refusals);
    assert_eq!(report.att_logged(Direction::ToPods), refusals);
    let marks: Vec<&str> = report
        .marks("att-request")
        .iter()
        .map(|mark| mark.1)
        .collect();
    assert_eq!(marks, ["opcode 0x02 refused", "opcode 0x0a refused"]);
    let outcome = report.outcome();
    assert_eq!(outcome.att_requests_refused, 2);
    assert_eq!(outcome.att_pdus_in, 2);
    assert_eq!(outcome.end, EndReason::ScriptDone);
    // No script of this run walks the table, so nothing else went out on the attribute channel.
    assert_eq!(report.marks("handles"), []);
}

#[test]
fn an_indication_is_confirmed() {
    let mut case = dictation_case(&[at(30, Action::End)]);
    case.pods.att.greeting = vec![INDICATION.to_vec()];
    let report = case.run();

    assert_eq!(report.pods.att_got, [[0x1e]]);
    assert_eq!(report.att_logged(Direction::ToPods), [[0x1e]]);
    assert!(
        report
            .marks("sent")
            .iter()
            .any(|mark| mark.1 == "confirmation")
    );
    let outcome = report.outcome();
    assert_eq!(outcome.att_requests_refused, 0);
    assert_eq!(report.marks("att-request"), []);
    assert_eq!(outcome.end, EndReason::ScriptDone);
}

#[test]
fn what_the_pods_ask_during_the_walk_is_answered_and_the_walk_goes_on() {
    let mut case = dictation_case(&STREAM);
    case.pods.att.greeting = vec![MTU_REQUEST.to_vec(), INDICATION.to_vec()];
    let report = case.run();

    // The two answers leave when their packets arrive, which may be before the walk or between two of its requests.
    let refusal = [0x01, 0x02, 0x00, 0x00, 0x06];
    let confirmation = [0x1e];
    let att_got = &report.pods.att_got;
    let count = |pdu: &[u8]| att_got.iter().filter(|got| *got == pdu).count();
    assert_eq!(
        (count(&refusal), count(&confirmation)),
        (1, 1),
        "{att_got:02x?}"
    );
    let mut asked = att_got.clone();
    asked.retain(|pdu| *pdu != refusal && *pdu != confirmation);
    let mut expected = walk_requests();
    expected.extend([START.to_vec(), STOP.to_vec()]);
    assert_eq!(asked, expected);
    assert_eq!(report.mark("handles"), "present");
    let outcome = report.outcome();
    assert_eq!(outcome.att_requests_refused, 1);
    assert_eq!(outcome.end, EndReason::ScriptDone);
}

#[test]
fn a_start_or_a_marker_is_not_repeated_while_its_stream_or_session_runs() {
    let events = [
        at(0, Action::MarkActive),
        at(2, Action::MarkActive),
        at(5, Action::DictationStart),
        at(10, Action::DictationStart),
        at(40, Action::DictationStop),
        at(45, Action::MarkIdle),
        at(60, Action::End),
    ];
    let report = dictation_case(&events).run();

    let count = |pdus: &[Vec<u8>], pdu: &[u8]| pdus.iter().filter(|got| *got == pdu).count();
    assert_eq!(count(&report.pods.att_got, &START), 1);
    assert_eq!(count(&report.pods.att_got, &STOP), 1);
    assert_eq!(report.pods.count(&SESSION_ACTIVE), 1);
    assert_eq!(report.pods.count(&SESSION_IDLE), 1);
    let outcome = report.outcome();
    assert_eq!((outcome.dictation_starts, outcome.dictation_stops), (1, 1));
    // The second of each is passed over, so the script still runs to its end.
    assert_eq!(outcome.end, EndReason::ScriptDone);
}

#[test]
fn the_dictation_rescue_sends_the_stop_then_the_idle_marker_without_a_walk() {
    let mut case = dictation_case(&[]);
    case.script = rescue(Route::Dictation).unwrap();
    let report = case.run();

    assert_eq!(report.pods.att_got, [STOP]);
    let mut expected = HANDSHAKE_REQUESTS.to_vec();
    expected.push(&SESSION_IDLE);
    assert_eq!(report.pods.got(), expected);
    assert_eq!(stream_kinds(&report), ["dictation-stop", "idle", "end"]);
    let stop_ns = report.marks("dictation-stop")[0].0;
    let idle_ns = report.marks("idle")[0].0;
    assert!(stop_ns <= idle_ns, "{}", report.marks);
    let outcome = report.outcome();
    assert_eq!((outcome.dictation_starts, outcome.dictation_stops), (0, 1));
    assert_eq!(outcome.end, EndReason::ScriptDone);
    assert!(report.returned_ns - idle_ns >= TAIL_NS);
    assert_eq!(report.marks("handles"), []);
}

#[test]
fn each_action_of_a_script_acts_when_it_fires_and_not_when_the_run_ends() {
    let bench = on_two_channels(&bench_script(), |engine, _clock, _far| {
        engine.checked = true;
        engine.fire(&Action::MarkActive).unwrap();
        let marked = engine.session.is_some();
        engine.fire(&Action::DictationStart).unwrap();
        let started = engine.dictation.is_some() && engine.recorder.dictation_starts == 1;
        engine.fire(&Action::DictationStop).unwrap();
        let stopped = engine.dictation.is_none() && engine.recorder.dictation_stops == 1;
        let still_marked = engine.session.is_some();
        engine.fire(&Action::MarkIdle).unwrap();
        let idle = engine.session.is_none() && engine.dropped.idle.take().is_none();
        [marked, started, stopped, still_marked, idle]
    });

    assert_eq!(bench.result, [true; 5]);
    assert_eq!(bench.att_got, [&START[..], &STOP[..]]);
    assert_eq!(bench.got, [SESSION_ACTIVE, SESSION_IDLE]);
    let kinds: Vec<&str> = bench
        .marks
        .lines()
        .map(|line| line.split('\t').nth(1).unwrap())
        .collect();
    let order = ["active", "dictation-start", "dictation-stop", "idle"];
    assert_eq!(kinds, order, "{}", bench.marks);
}

#[test]
fn any_reply_ends_the_wait_for_the_declarations_and_what_the_pods_send_unasked_does_not() {
    let refused = att::error_response(0x08, 0x0001, att::ATTRIBUTE_NOT_FOUND);
    let listing = [0x05, 0x01, 0x07, 0x00, 0x2a, 0x2b];
    let cut_short = [0x09, 0x07, 0x06];
    for (reply, ends) in [
        (&refused[..], true),
        (&listing[..], true),
        (&cut_short[..], true),
        (
            &[0x09, 0x07, 0x06, 0x00, 0x04, 0x07, 0x00, 0x2a, 0x2b][..],
            true,
        ),
        (&STARTED[..], false),
        (&[0x1d, 0x21, 0x00, 0x01][..], false),
        (&[0x02, 0x00, 0x02][..], false),
    ] {
        let bench = on_two_channels(&bench_script(), |engine, _clock, _far| {
            engine.asked = Asked::Declarations { at_ns: 0 };
            engine.on_pdu(1, reply).unwrap();
            engine.waits_on_the_table()
        });
        assert_eq!(bench.result, !ends, "{reply:02x?}");
    }
}
