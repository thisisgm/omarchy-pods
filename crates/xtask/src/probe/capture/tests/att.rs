//! The loop on the attribute channel, alone and beside the control channel.

use std::cell::Cell;

use pods_proto::sdulog::Direction;
use pods_proto::uplink::START;

use super::super::*;
use super::case::{ATT_ONLY, BOTH_CHANNELS, Case};
use super::pods::{pair, queued};
use super::steps::engine_in;
use super::{OVERSIZED_LEN, PATIENT_MS, at};

/// A value the fake pods send unasked on handle 0x0021, which the dictation stream does not use.
pub(super) const NOTIFICATION: [u8; 5] = [0x1b, 0x21, 0x00, 0xb8, 0x00];
/// An Exchange MTU request for 512 bytes, which a peer sends and then waits on.
const MTU_REQUEST: [u8; 3] = [0x02, 0x00, 0x02];
/// An indication on handle 0x0021, which a peer sends and then waits on.
const INDICATION: [u8; 4] = [0x1d, 0x21, 0x00, 0x01];
const HOLD_MS: u32 = 30;
/// The same hold in the nanoseconds the marks are stamped in.
const HOLD_NS: u64 = 30_000_000;
/// The tail of the guards dropped by hand, in readings of a clock that moves by one at each reading.
const TAIL_READINGS: u64 = 6;
/// More readings than a tail that ends at once takes.
const LONG_TAIL_READINGS: u64 = 1_000;

/// The notification with this number as its value, so each one in a log is told from the others.
fn notification(number: u8) -> Vec<u8> {
    vec![0x1b, 0x21, 0x00, number]
}

#[test]
fn att_knock_sends_no_byte_and_opens_no_accessory_channel() {
    let mut case = Case::new(Opening::AttKnock, &[at(HOLD_MS, Action::End)]);
    case.wired = ATT_ONLY;
    // A request and an indication both ask for an answer, and a knock still sends none.
    case.pods.att.greeting = vec![
        NOTIFICATION.to_vec(),
        MTU_REQUEST.to_vec(),
        INDICATION.to_vec(),
    ];
    let report = case.run();

    assert_eq!(report.pods.att_got, Vec::<Vec<u8>>::new());
    assert_eq!(report.pods.heard, []);
    assert_eq!(report.att_logged(Direction::ToPods), Vec::<Vec<u8>>::new());
    assert_eq!(report.att_logged(Direction::FromPods), report.pods.att_sent);
    assert_eq!(report.pods.att_sent.len(), 3);
    assert_eq!(report.log, "");
    let kinds = ["att-connect", "att-connect", "gate", "att-request", "end"];
    assert_eq!(report.kinds(), kinds);
    let unanswered = "opcode 0x02 not answered: a knock sends nothing";
    assert_eq!(report.mark("att-request"), unanswered);
    let connect = ["connecting", "attribute channel open"];
    assert_eq!(report.texts("att-connect"), connect);
    assert_eq!(report.mark("gate"), "connect");
    let outcome = report.outcome();
    assert_eq!((outcome.att_pdus_in, outcome.sdus_in), (3, 0));
    assert_eq!(outcome.att_requests_refused, 0);
    assert_eq!(outcome.end, EndReason::ScriptDone);
    let held_ns = report.marks("end")[0].0 - report.marks("gate")[0].0;
    assert!(held_ns >= HOLD_NS, "ended after {held_ns} ns");
}

#[test]
fn pdus_go_to_att_log_and_sdus_to_sdu_log_on_one_clock() {
    let events = [at(0, Action::Start), at(HOLD_MS, Action::End)];
    let mut case = Case::new(Opening::Handshake, &events);
    case.wired = BOTH_CHANNELS;
    case.pods.att.greeting = (0..5).map(notification).collect();
    let report = case.run();

    assert_eq!(
        report.att_logged(Direction::FromPods),
        report.pods.att_sent,
        "{}",
        report.att_log
    );
    assert_eq!(report.pods.att_sent.len(), 5);
    assert_eq!(report.att_logged(Direction::ToPods), Vec::<Vec<u8>>::new());
    assert_eq!(report.logged(Direction::ToPods), report.pods.got());
    let from_pods = report.logged(Direction::FromPods);
    assert!(
        from_pods.iter().all(|sdu| !sdu.starts_with(&[0x1b, 0x21])),
        "{}",
        report.log
    );
    assert_eq!(from_pods, report.pods.sent[..from_pods.len()]);
    let outcome = report.outcome();
    assert_eq!(outcome.att_pdus_in, 5);
    assert_eq!(outcome.sdus_in, u64::try_from(from_pods.len()).unwrap());
    assert!(outcome.audio_sdus > 0);
    assert_eq!(outcome.end, EndReason::ScriptDone);
    // One clock: every stamp of a log lies between the mark that opens its channel and the mark that closes the run.
    let opened_ns = report.marks("connect")[0].0;
    let att_opened_ns = report.marks("att-connect")[0].0;
    let ended_ns = report.marks("end")[0].0;
    assert!(att_opened_ns >= opened_ns);
    for (name, records, from_ns) in [
        ("sdu.log", report.records(), opened_ns),
        ("att.log", report.att_records(), att_opened_ns),
    ] {
        let stamps: Vec<u64> = records.iter().map(|record| record.at_ns).collect();
        assert!(stamps.is_sorted(), "{name}: {stamps:?}");
        let inside = stamps
            .iter()
            .all(|at_ns| (from_ns..=ended_ns).contains(at_ns));
        assert!(inside, "{name}: {stamps:?} outside {from_ns} to {ended_ns}");
    }
}

#[test]
fn a_truncated_pdu_is_marked_with_its_channel_and_counted_not_logged() {
    let mut case = Case::new(Opening::AttKnock, &[at(HOLD_MS, Action::End)]);
    case.wired = ATT_ONLY;
    case.pods.att.greeting = vec![vec![0x1b; OVERSIZED_LEN], NOTIFICATION.to_vec()];
    let report = case.run();

    assert_eq!(
        report.mark("truncated"),
        "5000 bytes: longer than the 4096 byte receive buffer, on the attribute channel"
    );
    assert_eq!(report.att_logged(Direction::FromPods), [NOTIFICATION]);
    let outcome = report.outcome();
    assert_eq!((outcome.truncated, outcome.att_pdus_in), (1, 1));
    assert_eq!(outcome.end, EndReason::ScriptDone);
}

#[test]
fn att_closed_by_the_peer_ends_the_run() {
    let events = [at(0, Action::Start), at(PATIENT_MS, Action::End)];
    let mut case = Case::new(Opening::Handshake, &events);
    case.wired = BOTH_CHANNELS;
    case.pods.att.greeting = vec![NOTIFICATION.to_vec()];
    case.pods.att.hang_up = true;
    case.pods.ear_after = None;
    let report = case.run();

    let outcome = report.outcome();
    assert_eq!(outcome.end, EndReason::AttClosed);
    assert_eq!(report.mark("end"), "the pods closed the attribute channel");
    // The pods report no ear, so the gate is still shut when the hang up is read.
    let got = report.pods.got();
    assert!(!got.contains(&&START[..]), "{got:02x?}");
    assert_eq!((outcome.starts, outcome.stops), (0, 0));
    assert_eq!(report.att_logged(Direction::FromPods), [NOTIFICATION]);
}

#[test]
fn a_script_that_needs_a_channel_the_run_lacks_ends_the_run() {
    let mut case = Case::new(Opening::Handshake, &[at(0, Action::Start)]);
    case.wired = ATT_ONLY;
    let report = case.run();

    let what = "control channel";
    let lacking = EndReason::Io(IoFailure::NotGiven { what });
    assert_eq!(report.outcome().end, lacking);
    assert_eq!(report.mark("end"), "the run was given no control channel");
    assert_eq!(report.pods.att_got, Vec::<Vec<u8>>::new());

    // A packet of the attribute channel with no log to go to is refused, not dropped and not counted.
    let (mut log, mut marks, mut cues) = (Vec::new(), Vec::new(), Vec::new());
    let mut out = Outputs {
        log: &mut log,
        att_log: None,
        handles: None,
        marks: &mut marks,
        cues: &mut cues,
    };
    let mut recorder = Recorder::new(&mut out);
    let what = ATT_LOG_NAME;
    let refused = recorder.received_pdu(7, &NOTIFICATION);
    assert_eq!(refused, Err(IoFailure::NotGiven { what }));
    assert_eq!(recorder.att_pdus_in, 0);
    assert_eq!(log, b"");
}

#[test]
fn neither_channel_waits_behind_a_busy_one() {
    let (near, far) = pair();
    let (near_att, far_att) = pair();
    for number in 0..3 {
        far.send(&notification(number)).unwrap();
        far_att.send(&notification(number)).unwrap();
    }
    let now_ns = || 0;
    let wires = Wires {
        accessory: Some(&near),
        att: &RefCell::new(Some(near_att)),
        now_ns: &now_ns,
    };
    let mut buf = [0_u8; RECV_BUFFER_LEN];
    let mut last = Bearer::Att;
    let mut order = Vec::new();
    for _ in 0..6 {
        let Wake::Sdu(bearer, _) = receive(&wires, last, &mut buf).1 else {
            panic!("a queued packet did not arrive");
        };
        order.push(bearer);
        last = bearer;
    }
    let (control, att) = (Bearer::Accessory, Bearer::Att);
    assert_eq!(order, [control, att, control, att, control, att]);

    // The loop itself remembers the channel it read, which is what makes the next wait ask the other first.
    let (near, far) = pair();
    let (near_att, far_att) = pair();
    let script = Script {
        opening: Opening::Knock,
        needs_initial_side: false,
        unchecked: false,
        events: Vec::new(),
    };
    for number in 0..2 {
        far.send(&notification(number)).unwrap();
        far_att.send(&notification(number)).unwrap();
    }
    let wires = Wires {
        accessory: Some(&near),
        att: &RefCell::new(Some(near_att)),
        now_ns: &now_ns,
    };
    let (mut log, mut att_log, mut marks, mut cues) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut out = Outputs {
        log: &mut log,
        att_log: Some(&mut att_log),
        handles: None,
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
    let mut engine = engine_in(
        scripted,
        wires,
        &script,
        &mut recorder,
        &interrupted,
        &dropped,
    );
    let mut order = Vec::new();
    for _ in 0..4 {
        engine.turn(&mut buf).unwrap();
        order.push(engine.last_read);
    }
    assert_eq!(order, [control, att, control, att]);
}

#[test]
fn the_tail_outlasts_a_channel_that_closes_and_ends_when_both_have() {
    let (near, far) = pair();
    let (near_att, far_att) = pair();
    let clock = Cell::new(0_u64);
    let now_ns = || clock.replace(clock.get() + 1);
    let wires = Wires {
        accessory: Some(&near),
        att: &RefCell::new(Some(near_att)),
        now_ns: &now_ns,
    };
    let dropped = Dropped::default();
    dropped.tail_from_ns.set(Some(0));
    far.send(&NOTIFICATION).unwrap();
    drop(far_att);
    drop(Tail {
        wires,
        length_ns: TAIL_READINGS,
        dropped: &dropped,
    });
    // The attribute channel was gone at once, and the control channel still got its whole tail.
    assert!(clock.get() > TAIL_READINGS, "{clock:?}");
    // What arrived during the tail was read, so the queue of the open channel is empty.
    far.send(&MTU_REQUEST).unwrap();
    assert_eq!(queued(&near), [MTU_REQUEST]);

    drop(far);
    clock.set(0);
    // A tail counts from the last packet of a guard only once, so the second one is given its own.
    dropped.tail_from_ns.set(Some(0));
    drop(Tail {
        wires,
        length_ns: LONG_TAIL_READINGS,
        dropped: &dropped,
    });
    // With both channels gone nothing is left to keep open, so the tail starts and does not sit out its length.
    assert!((1..TAIL_READINGS).contains(&clock.get()), "{clock:?}");
}

#[test]
fn the_tail_outlasts_the_control_channel_when_it_closes_first() {
    let (near, far) = pair();
    let (near_att, far_att) = pair();
    let clock = Cell::new(0_u64);
    let now_ns = || clock.replace(clock.get() + 1);
    let att = RefCell::new(Some(near_att));
    let wires = Wires {
        accessory: Some(&near),
        att: &att,
        now_ns: &now_ns,
    };
    let dropped = Dropped::default();
    dropped.tail_from_ns.set(Some(0));
    far_att.send(&NOTIFICATION).unwrap();
    drop(far);
    drop(Tail {
        wires,
        length_ns: TAIL_READINGS,
        dropped: &dropped,
    });
    // The control channel was gone at once, and the attribute channel still got its whole tail.
    assert!(clock.get() > TAIL_READINGS, "{clock:?}");
    // What arrived during the tail was read, so the queue of the open channel is empty.
    far_att.send(&MTU_REQUEST).unwrap();
    let still_open = att.borrow();
    assert_eq!(queued(still_open.as_ref().unwrap()), [MTU_REQUEST]);
}
