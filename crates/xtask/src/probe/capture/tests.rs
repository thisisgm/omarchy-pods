//! The capture loop driven against fake pods on the other end of a socket pair.

mod air_command;
mod att;
mod att_command;
mod att_pods;
mod case;
mod command;
mod dictation;
mod dictation_exits;
mod pods;
mod steps;
mod table;
mod walk_cut;

use std::fs::{self, Permissions};
use std::io;
use std::os::unix::fs::PermissionsExt;

use pods_proto::packets::{
    FEATURES_ACK_PREFIX, HANDSHAKE, HANDSHAKE_ACK_PREFIX, REQUEST_NOTIFICATIONS,
    SET_SPECIFIC_FEATURES,
};
use pods_proto::sdulog::Direction;
use pods_proto::uplink::{START, STOP};
use tempfile::TempDir;

use super::super::script::{Event, Route, rescue};
use super::super::summary;
use super::*;
use case::{Case, LogFault, PATIENT_MS, PATIENT_NS, Report, TAIL_NS};
use pods::{EAR_PACKET, Heard, ack, audio_sdu, noise};

/// A wait short enough for a test that expects it to run out.
const SHORT_NS: u64 = 50_000_000;
/// An SDU one receive buffer cannot hold.
const OVERSIZED_LEN: usize = 5000;
const ROOT_NAME: &str = "omapods-captures";
/// The only mode the store accepts for its root.
const ROOT_MODE: u32 = 0o700;
/// The only mode the store accepts for the device file.
const DEVICE_MODE: u32 = 0o600;
/// The device file of the command tests, an address that names no real device.
const DEVICE_LINE: &str = "AA:BB:CC:DD:EE:FF\n";

const fn at(at_ms: u32, action: Action) -> Event {
    Event { at_ms, action }
}

/// The outcome of a run that counted nothing, for a test that then sets the counts it reads.
fn counted_nothing(end: EndReason) -> Outcome {
    Outcome {
        sdus_in: 0,
        audio_sdus: 0,
        starts: 0,
        stops: 0,
        uplink_running: false,
        truncated: 0,
        att_pdus_in: 0,
        audio_notifications: 0,
        dictation_starts: 0,
        dictation_stops: 0,
        att_requests_refused: 0,
        end,
    }
}

fn home_with_device() -> TempDir {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(ROOT_NAME);
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, Permissions::from_mode(ROOT_MODE)).unwrap();
    let device = root.join("device");
    fs::write(&device, DEVICE_LINE).unwrap();
    fs::set_permissions(&device, Permissions::from_mode(DEVICE_MODE)).unwrap();
    home
}

/// The exits a run with the uplink running can take, each with the reason it must report.
fn exits_with_the_uplink_running() -> Vec<(&'static str, Case, Option<EndReason>)> {
    let streaming = [at(0, Action::Start), at(PATIENT_MS, Action::End)];
    let done = Case::new(
        Opening::Handshake,
        &[at(0, Action::Start), at(30, Action::End)],
    );
    let mut interrupted = Case::new(Opening::Handshake, &streaming);
    interrupted.pods.interrupt_after = Some(3);
    let mut log_error = Case::new(Opening::Handshake, &streaming);
    log_error.log_fault = Some(LogFault::Fails);
    let log_failure = IoFailure::Write {
        file: LOG_NAME,
        kind: io::ErrorKind::Other,
    };
    let mut early = Case::new(Opening::EarlyStart, &[at(PATIENT_MS, Action::End)]);
    early.pods.acks = 0;
    early.timing.handshake_step = SHORT_NS;
    let mut panics = Case::new(Opening::Handshake, &streaming);
    panics.log_fault = Some(LogFault::Panics);
    vec![
        ("script done", done, Some(EndReason::ScriptDone)),
        (
            "interrupted mid stream",
            interrupted,
            Some(EndReason::Interrupted),
        ),
        (
            "log writer error",
            log_error,
            Some(EndReason::Io(log_failure)),
        ),
        (
            "handshake timeout after an early start",
            early,
            Some(EndReason::HandshakeTimeout(Step::AwaitHandshakeAck)),
        ),
        ("log writer panic", panics, None),
    ]
}

/// Checks one exit: the pods got STOP after START, the counts agree, and the channel stayed open for the tail.
fn stop_failures(name: &str, report: &Report, end: Option<&EndReason>) -> Vec<String> {
    let mut failures = Vec::new();
    if !report.stop_followed_start() {
        failures.push(format!("{name}: the pods got {:02x?}", report.pods.got()));
    }
    if report.end().as_ref() != end {
        failures.push(format!("{name}: ended as {:?}", report.outcome));
    }
    // A panic unwinds past the marks, so its tail counts from the panic, which the STOP follows.
    let stop_ns = match &report.outcome {
        Some(outcome) => {
            if (outcome.starts, outcome.stops) != (1, 1) {
                failures.push(format!("{name}: counted {outcome:?}"));
            }
            report.marks("stop").first().map(|(at_ns, _)| *at_ns)
        }
        None => report.faulted_at_ns,
    };
    let held_ns = stop_ns.and_then(|stop_ns| report.returned_ns.checked_sub(stop_ns));
    if held_ns.is_none_or(|held_ns| held_ns < TAIL_NS) {
        failures.push(format!(
            "{name}: the channel stayed open for {held_ns:?} ns"
        ));
    }
    failures
}

#[test]
fn every_sdu_is_logged_in_order_with_non_decreasing_stamps() {
    let mut case = Case::new(
        Opening::Handshake,
        &[at(0, Action::Start), at(PATIENT_MS, Action::End)],
    );
    case.pods.greeting = vec![vec![0x7f], noise(RECV_BUFFER_LEN)];
    let mut burst: Vec<Vec<u8>> = (0..6).map(audio_sdu).collect();
    burst.extend([noise(300), audio_sdu(6), noise(7), audio_sdu(7)]);
    case.pods.burst = Some(burst);
    let report = case.run();

    let stamps: Vec<u64> = report.records().iter().map(|record| record.at_ns).collect();
    assert!(stamps.is_sorted(), "{stamps:?}");
    assert_eq!(report.logged(Direction::FromPods), report.pods.sent);
    assert_eq!(report.logged(Direction::ToPods), report.pods.got());
    let sent = [
        &HANDSHAKE[..],
        &SET_SPECIFIC_FEATURES,
        &REQUEST_NOTIFICATIONS,
        &START,
    ];
    assert_eq!(report.pods.got(), sent);
    // Two greetings, two acknowledgements, the ear packet and the ten packets of the burst.
    assert_eq!(report.pods.sent.len(), 15);
    let outcome = report.outcome();
    assert_eq!(outcome.sdus_in, 15);
    assert_eq!(outcome.audio_sdus, 8);
    assert_eq!(outcome.truncated, 0);
    assert_eq!(outcome.end, EndReason::PeerClosed);
    // The pods hung up while the uplink ran, so the STOP was tried and could not leave.
    assert_eq!((outcome.starts, outcome.stops), (1, 0));
    let stop = report.mark("stop");
    assert!(
        stop.starts_with("uplink stop failed: could not send"),
        "{stop}"
    );
    assert!(summary(&outcome, Route::Uplink).contains("STOP was NOT sent"));
}

#[test]
fn start_waits_for_the_ear_packet() {
    let mut case = Case::new(
        Opening::Handshake,
        &[at(0, Action::Start), at(30, Action::End)],
    );
    case.pods.ear_after = Some(Duration::from_millis(60));
    let report = case.run();

    let heard = &report.pods.heard;
    let ear = heard.iter().position(|heard| *heard == Heard::SentEar);
    let start = heard
        .iter()
        .position(|heard| *heard == Heard::Got(START.to_vec()));
    assert!(ear.is_some() && ear < start, "{heard:02x?}");
    assert_eq!(report.mark("gate"), "ear");
    assert_eq!(report.outcome().end, EndReason::ScriptDone);
}

#[test]
fn start_goes_after_three_seconds_without_one() {
    let mut case = Case::new(
        Opening::Handshake,
        &[at(0, Action::Start), at(30, Action::End)],
    );
    case.pods.ear_after = None;
    case.timing.ear_report = SHORT_NS;
    let report = case.run();

    assert!(!report.pods.heard.contains(&Heard::SentEar));
    assert_eq!(report.mark("gate"), "timeout");
    let waited_ns = report.sent_at_ns(&START) - report.sent_at_ns(&REQUEST_NOTIFICATIONS);
    assert!(
        waited_ns >= SHORT_NS,
        "START went {waited_ns} ns after the request"
    );
    assert_eq!(report.outcome().end, EndReason::ScriptDone);
}

#[test]
fn stop_is_sent_on_every_exit_path() {
    let mut failures = Vec::new();
    for (name, case, end) in exits_with_the_uplink_running() {
        if end.is_some() {
            failures.extend(stop_failures(name, &case.run(), end.as_ref()));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn stop_is_sent_when_the_loop_panics() {
    let mut failures = Vec::new();
    for (name, case, end) in exits_with_the_uplink_running() {
        if end.is_none() {
            failures.extend(stop_failures(name, &case.run(), end.as_ref()));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn each_cycle_of_a_script_is_started_stopped_and_marked() {
    let events = [
        at(0, Action::Start),
        at(20, Action::Stop),
        at(30, Action::Start),
        at(50, Action::Stop),
        at(55, Action::End),
    ];
    let report = Case::new(Opening::Handshake, &events).run();

    let got = report.pods.got();
    assert_eq!(got[3..], [&START[..], &STOP, &START, &STOP], "{got:02x?}");
    let outcome = report.outcome();
    assert_eq!((outcome.starts, outcome.stops), (2, 2));
    let kinds = report.kinds();
    assert!(
        kinds.ends_with(&["gate", "start", "stop", "start", "stop", "end"]),
        "{kinds:?}"
    );
    assert_eq!(outcome.end, EndReason::ScriptDone);
}

#[test]
fn a_script_stop_is_logged_and_its_tail_is_waited_out() {
    let events = [
        at(0, Action::Start),
        at(20, Action::Stop),
        at(25, Action::Cue("after the stop")),
        at(40, Action::End),
    ];
    let report = Case::new(Opening::Handshake, &events).run();

    assert!(report.stop_followed_start(), "{:02x?}", report.pods.got());
    // The STOP leaves when the script says, not when the run ends.
    let kinds = report.kinds();
    assert!(
        kinds.ends_with(&["start", "stop", "cue", "end"]),
        "{kinds:?}"
    );
    assert_eq!(report.mark("start"), "uplink start");
    assert_eq!(report.mark("stop"), "uplink stop");
    let stop_ns = report.sent_at_ns(&STOP);
    assert!(report.returned_ns - stop_ns >= TAIL_NS);
    let outcome = report.outcome();
    assert_eq!((outcome.starts, outcome.stops), (1, 1));
    assert_eq!(outcome.end, EndReason::ScriptDone);
    assert!(outcome.audio_sdus > 0);
}

#[test]
fn a_hang_up_during_the_tail_ends_the_wait() {
    let events = [at(0, Action::Start), at(20, Action::End)];
    let mut case = Case::new(Opening::Handshake, &events);
    case.pods.hang_up_on_stop = true;
    case.timing.stop_tail = PATIENT_NS;
    let report = case.run();

    assert!(report.stop_followed_start(), "{:02x?}", report.pods.got());
    assert_eq!(report.outcome().end, EndReason::ScriptDone);
    // A closed channel has nothing left to keep open, so the run does not sit out the tail.
    let held_ns = report.returned_ns - report.sent_at_ns(&STOP);
    assert!(held_ns < PATIENT_NS / 2, "held for {held_ns} ns");
}

#[test]
fn a_start_while_the_uplink_runs_goes_out_again_and_one_stop_ends_both() {
    let events = [
        at(0, Action::Start),
        at(10, Action::Start),
        at(30, Action::End),
    ];
    let report = Case::new(Opening::Handshake, &events).run();

    assert_eq!(report.pods.count(&START), 2);
    assert_eq!(report.pods.count(&STOP), 1);
    assert!(report.stop_followed_start(), "{:02x?}", report.pods.got());
    let outcome = report.outcome();
    assert_eq!((outcome.starts, outcome.stops), (2, 1));
    assert!(!outcome.uplink_running);
    assert_eq!(outcome.end, EndReason::ScriptDone);
}

#[test]
fn no_stop_is_sent_when_no_start_was_sent() {
    let done = Case::new(Opening::Handshake, &[at(20, Action::End)]);
    let mut timeout = Case::new(Opening::Handshake, &[at(PATIENT_MS, Action::End)]);
    timeout.pods.acks = 1;
    timeout.timing.handshake_step = SHORT_NS;
    let mut hang_up = Case::new(Opening::Handshake, &[at(PATIENT_MS, Action::End)]);
    hang_up.pods.hang_up_when_ready = true;
    let mut interrupted = Case::new(Opening::Handshake, &[at(0, Action::Start)]);
    interrupted.interrupted = true;
    let timed_out = EndReason::HandshakeTimeout(Step::AwaitFeaturesAck);
    for (name, case, sent, end) in [
        ("script done", done, 3, EndReason::ScriptDone),
        ("handshake timeout", timeout, 2, timed_out),
        ("peer hang up", hang_up, 3, EndReason::PeerClosed),
        (
            "interrupted before the run",
            interrupted,
            0,
            EndReason::Interrupted,
        ),
    ] {
        let report = case.run();
        let got = report.pods.got();
        assert!(!got.contains(&&STOP[..]), "{name}: {got:02x?}");
        assert!(!got.contains(&&START[..]), "{name}: {got:02x?}");
        assert_eq!(got.len(), sent, "{name}: {got:02x?}");
        let outcome = report.outcome();
        assert_eq!(outcome.end, end, "{name}");
        assert_eq!((outcome.starts, outcome.stops), (0, 0), "{name}");
        assert!(report.marks("stop").is_empty(), "{name}");
    }
}

#[test]
fn peer_hang_up_ends_the_run_as_peer_closed() {
    let mut case = Case::new(Opening::Handshake, &[at(PATIENT_MS, Action::End)]);
    case.pods.hang_up_when_ready = true;
    let report = case.run();

    let outcome = report.outcome();
    assert_eq!(outcome.end, EndReason::PeerClosed);
    assert_eq!(report.mark("end"), "the pods closed the channel");
    assert!(summary(&outcome, Route::Uplink).contains("the pods closed the channel"));
}

#[test]
fn handshake_timeout_names_the_step() {
    for (acks, step, named) in [
        (0, Step::AwaitHandshakeAck, "the handshake acknowledgement"),
        (1, Step::AwaitFeaturesAck, "the features acknowledgement"),
    ] {
        let mut case = Case::new(Opening::Handshake, &[at(0, Action::Start)]);
        case.pods.acks = acks;
        case.timing.handshake_step = SHORT_NS;
        let report = case.run();

        let outcome = report.outcome();
        assert_eq!(outcome.end, EndReason::HandshakeTimeout(step));
        assert!(report.mark("end").contains(named), "{}", report.mark("end"));
        let sentence = summary(&outcome, Route::Uplink);
        assert!(sentence.contains(named), "{sentence}");
        assert!(!sentence.contains("STOP was"), "{sentence}");
        let waited_ns = report.marks("end")[0].0 - report.marks("sent").last().unwrap().0;
        assert!(waited_ns >= SHORT_NS, "gave up after {waited_ns} ns");
    }
}

#[test]
fn knock_sends_no_byte() {
    let mut case = Case::new(Opening::Knock, &[at(30, Action::End)]);
    case.pods.greeting = vec![
        ack(&HANDSHAKE_ACK_PREFIX),
        ack(&FEATURES_ACK_PREFIX),
        EAR_PACKET.to_vec(),
    ];
    let report = case.run();

    assert_eq!(report.pods.heard, []);
    assert_eq!(report.logged(Direction::ToPods), Vec::<Vec<u8>>::new());
    assert_eq!(report.logged(Direction::FromPods), report.pods.sent);
    assert_eq!(report.mark("gate"), "connect");
    let outcome = report.outcome();
    assert_eq!(outcome.sdus_in, 3);
    assert_eq!(outcome.end, EndReason::ScriptDone);
    let held_ns = report.marks("end")[0].0 - report.marks("gate")[0].0;
    assert!(held_ns >= 30_000_000, "ended after {held_ns} ns");
}

#[test]
fn early_start_goes_out_before_the_handshake_packet() {
    let events = [at(10, Action::Stop), at(40, Action::End)];
    let report = Case::new(Opening::EarlyStart, &events).run();

    let got = report.pods.got();
    assert_eq!(got[..2], [&START[..], &HANDSHAKE[..]], "{got:02x?}");
    assert_eq!(
        report.logged(Direction::ToPods)[..2],
        [&START[..], &HANDSHAKE[..]]
    );
    assert!(report.stop_followed_start(), "{got:02x?}");
    assert_eq!(report.outcome().end, EndReason::ScriptDone);
}

#[test]
fn side_refuses_to_run_without_an_initial_value() {
    let events = [
        at(0, Action::Start),
        at(0, Action::SetSide(MicSide::Left)),
        at(PATIENT_MS, Action::End),
    ];
    let mut flagged = Case::new(Opening::Handshake, &events);
    flagged.script.needs_initial_side = true;
    flagged.timing.side_report = SHORT_NS;
    let report = flagged.run();

    let handshake = [
        &HANDSHAKE[..],
        &SET_SPECIFIC_FEATURES,
        &REQUEST_NOTIFICATIONS,
    ];
    assert_eq!(report.pods.got(), handshake);
    assert_eq!(report.outcome().end, EndReason::NoInitialSide);
    assert_eq!(report.outcome().starts, 0);
    assert_eq!(report.marks("side"), []);

    // A script that changes the side without saying it needs the report is refused at the change.
    let report = Case::new(Opening::Handshake, &events).run();
    let side = MicSide::Left.command().encode();
    assert_eq!(report.pods.count(&side), 0, "{:02x?}", report.pods.got());
    assert!(report.stop_followed_start());
    assert_eq!(report.outcome().end, EndReason::NoInitialSide);
}

#[test]
fn side_is_restored_on_every_exit_path() {
    let set = MicSide::Left.command().encode();
    let restore = MicSide::Right.command().encode();
    let mut failures = Vec::new();
    for (name, mut case, end) in exits_with_the_uplink_running() {
        if case.script.opening == Opening::EarlyStart {
            // No side can change before the gate, so the handshake timeout has no row here.
            continue;
        }
        case.script.needs_initial_side = true;
        case.script
            .events
            .insert(1, at(0, Action::SetSide(MicSide::Left)));
        case.pods.side = Some(MicSide::Right);
        let report = case.run();
        let got = report.pods.got();
        let set_at = got.iter().position(|sdu| *sdu == set);
        let restored_at = got.iter().position(|sdu| *sdu == restore);
        let stopped_at = got.iter().position(|sdu| *sdu == STOP);
        let once = report.pods.count(&set) == 1 && report.pods.count(&restore) == 1;
        // The STOP leaves first, so the uplink is not running when the side changes back.
        if !(once && set_at < stopped_at && stopped_at < restored_at) {
            failures.push(format!("{name}: the pods got {got:02x?}"));
        }
        if !report.stop_followed_start() {
            failures.push(format!("{name}: no STOP in {got:02x?}"));
        }
        if report.end() != end {
            failures.push(format!("{name}: ended as {:?}", report.outcome));
        }
        let texts: Vec<&str> = report.marks("side").iter().map(|mark| mark.1).collect();
        if report.outcome.is_some() && texts != ["set left", "restored"] {
            failures.push(format!("{name}: side marks in\n{}", report.marks));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn a_script_restore_puts_back_the_side_the_pods_reported() {
    let events = [
        at(0, Action::SetSide(MicSide::Right)),
        at(10, Action::SetSide(MicSide::Automatic)),
        at(20, Action::RestoreSide),
        at(30, Action::Cue("after the restore")),
        at(40, Action::End),
    ];
    let mut case = Case::new(Opening::Handshake, &events);
    case.script.needs_initial_side = true;
    // The reported side is neither the default nor the last one set, so only a restore of what was read sends it.
    case.pods.side = Some(MicSide::Left);
    let report = case.run();

    let sides: Vec<Vec<u8>> = [MicSide::Right, MicSide::Automatic, MicSide::Left]
        .map(|side| side.command().encode())
        .to_vec();
    assert_eq!(report.pods.got()[3..], sides);
    let texts: Vec<&str> = report.marks("side").iter().map(|mark| mark.1).collect();
    assert_eq!(texts, ["set right", "set automatic", "restored"]);
    // The side goes back when the script says, not when the run ends.
    let kinds = report.kinds();
    assert!(
        kinds.ends_with(&["side", "side", "side", "cue", "end"]),
        "{kinds:?}"
    );
    assert_eq!(report.outcome().end, EndReason::ScriptDone);
}

#[test]
fn the_automatic_segment_is_sent_when_the_pods_start_on_a_fixed_side() {
    // The shape of the `side` script: three settings with the uplink running, the restore after the STOP.
    let events = [
        at(0, Action::Start),
        at(0, Action::SetSide(MicSide::Left)),
        at(10, Action::SetSide(MicSide::Right)),
        at(20, Action::SetSide(MicSide::Automatic)),
        at(30, Action::Stop),
        at(40, Action::RestoreSide),
        at(50, Action::End),
    ];
    let mut case = Case::new(Opening::Handshake, &events);
    case.script.needs_initial_side = true;
    // A fixed side to begin with, so a restore in place of the automatic setting would send it twice more.
    case.pods.side = Some(MicSide::Left);
    let report = case.run();

    let [left, right, automatic] =
        [MicSide::Left, MicSide::Right, MicSide::Automatic].map(|side| side.command().encode());
    let got = report.pods.got();
    let sides: Vec<&[u8]> = got
        .iter()
        .map(|sdu| &sdu[..])
        .filter(|sdu| [&left[..], &right[..], &automatic[..]].contains(sdu))
        .collect();
    assert_eq!(sides, [&left[..], &right, &automatic, &left], "{got:02x?}");
    let automatic_at = got.iter().position(|sdu| *sdu == automatic);
    let stopped_at = got.iter().position(|sdu| *sdu == STOP);
    let restored_at = got.iter().rposition(|sdu| *sdu == left);
    assert!(
        automatic_at < stopped_at && stopped_at < restored_at,
        "{got:02x?}"
    );
    let texts: Vec<&str> = report.marks("side").iter().map(|mark| mark.1).collect();
    assert_eq!(
        texts,
        ["set left", "set right", "set automatic", "restored"]
    );
    assert_eq!(report.outcome().end, EndReason::ScriptDone);
}

#[test]
fn a_truncated_sdu_is_marked_and_counted_not_logged() {
    let mut case = Case::new(Opening::Handshake, &[at(30, Action::End)]);
    case.pods.greeting = vec![vec![0x11; OVERSIZED_LEN], vec![0x22; 3]];
    let report = case.run();

    assert_eq!(
        report.mark("truncated"),
        "5000 bytes: longer than the 4096 byte receive buffer"
    );
    let from_pods = report.logged(Direction::FromPods);
    assert_eq!(from_pods, report.pods.sent[1..]);
    let outcome = report.outcome();
    assert_eq!(outcome.truncated, 1);
    assert_eq!(outcome.sdus_in, u64::try_from(from_pods.len()).unwrap());
    assert_eq!(outcome.end, EndReason::ScriptDone);
}

#[test]
fn cues_reach_marks_and_the_cue_writer() {
    let events = [
        at(0, Action::Cue("Stay as you are. Nothing to do.")),
        at(10, Action::Cue("Tap the left bud twice")),
        at(20, Action::End),
    ];
    let report = Case::new(Opening::Handshake, &events).run();

    assert_eq!(
        report.cues,
        "cue Stay as you are. Nothing to do.\ncue Tap the left bud twice\n"
    );
    let cues = report.marks("cue");
    let texts: Vec<&str> = cues.iter().map(|mark| mark.1).collect();
    assert_eq!(
        texts,
        ["Stay as you are. Nothing to do.", "Tap the left bud twice"]
    );
    assert!(cues[1].0 - cues[0].0 >= 10_000_000, "{cues:?}");
    assert_eq!(report.outcome().end, EndReason::ScriptDone);
}

#[test]
fn the_rescue_script_sends_stop_without_a_start() {
    let mut case = Case::new(Opening::Handshake, &[]);
    case.script = rescue(Route::Uplink).unwrap();
    let report = case.run();

    let sent = [
        &HANDSHAKE[..],
        &SET_SPECIFIC_FEATURES,
        &REQUEST_NOTIFICATIONS,
        &STOP,
    ];
    assert_eq!(report.pods.got(), sent);
    let outcome = report.outcome();
    assert_eq!((outcome.starts, outcome.stops), (0, 1));
    assert_eq!(outcome.end, EndReason::ScriptDone);
    assert!(report.returned_ns - report.sent_at_ns(&STOP) >= TAIL_NS);
}
