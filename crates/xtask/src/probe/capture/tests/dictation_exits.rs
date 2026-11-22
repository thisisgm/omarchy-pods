//! Every way out of a run with the dictation stream running: the stop write, the idle marker after it, and the tail.

use std::io;

use pods_proto::dictation::{SESSION_ACTIVE, SESSION_IDLE, START, STOP};
use pods_proto::uplink;
use rustix::io::Errno;

use super::super::super::script::Route;
use super::super::super::summary;
use super::super::wires::wake_of;
use super::super::*;
use super::case::{Case, LogFault, PATIENT_MS, Report, TAIL_NS};
use super::dictation::{
    OPEN_STREAM, STREAM, bench_script, dictation_case, on_two_channels, stream_kinds,
};
use super::{SHORT_NS, at};

/// The exits a run with the stream running can take, each with the reason it must report; `None` is a panic.
fn exits_with_the_stream_running() -> Vec<(&'static str, Case, Option<EndReason>)> {
    let done = dictation_case(&[
        at(0, Action::MarkActive),
        at(5, Action::DictationStart),
        at(40, Action::End),
    ]);
    let mut interrupted = dictation_case(&OPEN_STREAM);
    interrupted.pods.att.interrupt_after = Some(3);
    let mut log_error = dictation_case(&OPEN_STREAM);
    log_error.att_log_fault = Some(LogFault::Fails);
    let log_failure = IoFailure::Write {
        file: ATT_LOG_NAME,
        kind: io::ErrorKind::Other,
    };
    let mut control_closed = dictation_case(&OPEN_STREAM);
    control_closed.pods.att.hang_up_control_after = Some(3);
    let mut panics = dictation_case(&OPEN_STREAM);
    panics.att_log_fault = Some(LogFault::Panics);
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
            "the pods closing the control channel",
            control_closed,
            Some(EndReason::PeerClosed),
        ),
        ("log writer panic", panics, None),
    ]
}

fn in_order_and_last(packets: &[&[u8]], first: &[u8], second: &[u8]) -> bool {
    let count = |wanted: &[u8]| packets.iter().filter(|got| **got == wanted).count();
    let first_at = packets.iter().position(|got| *got == first);
    let second_at = packets.iter().position(|got| *got == second);
    let last = packets.len().checked_sub(1);
    count(first) == 1 && count(second) == 1 && first_at < second_at && second_at == last
}

/// Checks one exit: the pods got the stop write once after the start write, the counts agree, and the channels stayed open for the tail.
fn stop_failures(name: &str, report: &Report, end: Option<&EndReason>) -> Vec<String> {
    let mut failures = Vec::new();
    let att_got: Vec<&[u8]> = report.pods.att_got.iter().map(Vec::as_slice).collect();
    if !in_order_and_last(&att_got, &START, &STOP) {
        failures.push(format!("{name}: the pods got {att_got:02x?}"));
    }
    if report.end().as_ref() != end {
        failures.push(format!("{name}: ended as {:?}", report.outcome));
    }
    // A panic unwinds past the marks, so its tail counts from the panic, which the stop write follows.
    let stop_ns = match &report.outcome {
        Some(outcome) => {
            if (outcome.dictation_starts, outcome.dictation_stops) != (1, 1) {
                failures.push(format!("{name}: counted {outcome:?}"));
            }
            let stops = report.marks("dictation-stop");
            stops.first().map(|(at_ns, _)| *at_ns)
        }
        None => report.faulted_at_ns,
    };
    let held_ns = stop_ns.and_then(|stop_ns| report.returned_ns.checked_sub(stop_ns));
    if held_ns.is_none_or(|held_ns| held_ns < TAIL_NS) {
        failures.push(format!(
            "{name}: the channels stayed open for {held_ns:?} ns"
        ));
    }
    failures
}

#[test]
fn dictation_stop_is_sent_on_every_exit_path() {
    let mut failures = Vec::new();
    for (name, case, end) in exits_with_the_stream_running() {
        if end.is_some() {
            failures.extend(stop_failures(name, &case.run(), end.as_ref()));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn dictation_stop_is_sent_when_the_loop_panics() {
    let mut failures = Vec::new();
    for (name, case, end) in exits_with_the_stream_running() {
        if end.is_none() {
            let report = case.run();
            failures.extend(stop_failures(name, &report, end.as_ref()));
            // The idle marker leaves in the same unwind.
            if !in_order_and_last(&report.pods.got(), &SESSION_ACTIVE, &SESSION_IDLE) {
                failures.push(format!("{name}: the pods got {:02x?}", report.pods.got()));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn the_idle_marker_follows_the_stop_on_every_exit_path() {
    let mut failures = Vec::new();
    for (name, case, end) in exits_with_the_stream_running() {
        if end.is_none() {
            continue;
        }
        let report = case.run();
        let kinds = stream_kinds(&report);
        let order = ["active", "dictation-start", "dictation-stop", "idle", "end"];
        if kinds != order {
            failures.push(format!("{name}: marked {kinds:?}"));
            continue;
        }
        let stop_ns = report.marks("dictation-stop")[0].0;
        let (idle_ns, idle) = report.marks("idle")[0];
        if stop_ns > idle_ns {
            failures.push(format!("{name}: idle at {idle_ns}, stop at {stop_ns}"));
        }
        let got = report.pods.got();
        if end == Some(EndReason::PeerClosed) {
            // The marker was tried on a channel the pods had closed, and the mark says that it did not leave.
            if !idle.starts_with("session idle failed: could not send") {
                failures.push(format!("{name}: idle marked as {idle}"));
            }
        } else if idle != "session idle" || !in_order_and_last(&got, &SESSION_ACTIVE, &SESSION_IDLE)
        {
            failures.push(format!("{name}: {idle}, and the pods got {got:02x?}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn the_guards_drop_in_the_order_stop_stop_idle_whatever_ends_the_run() {
    let bench = on_two_channels(&bench_script(), |engine, _clock, _far| {
        engine.checked = true;
        engine.mark_active().unwrap();
        engine.dictation_start().unwrap();
        engine.start().unwrap();
    });

    // Nothing in the script stopped anything, so the three packets below left as the loop was dropped.
    assert_eq!(bench.recorded, Ok(()));
    let sent: Vec<(u64, &str)> = bench
        .marks
        .lines()
        .map(|line| {
            // A mark is a stamp in nanoseconds, a kind and a text, as `430563\tactive\tsession active`.
            let mut fields = line.splitn(3, '\t');
            let at_ns = fields.next().unwrap().parse().unwrap();
            (at_ns, fields.next().unwrap())
        })
        .collect();
    let kinds: Vec<&str> = sent.iter().map(|mark| mark.1).collect();
    let order = [
        "active",
        "dictation-start",
        "start",
        "stop",
        "dictation-stop",
        "idle",
    ];
    assert_eq!(kinds, order, "{}", bench.marks);
    // The clock moves at each reading, so the stamps say which left first.
    let stamps: Vec<u64> = sent.iter().map(|mark| mark.0).collect();
    assert!(
        stamps.is_sorted_by(|before, after| before < after),
        "{stamps:?}"
    );
    assert_eq!(bench.att_got, [&START[..], &STOP[..]]);
    let control = [
        &SESSION_ACTIVE[..],
        &uplink::START,
        &uplink::STOP,
        &SESSION_IDLE,
    ];
    assert_eq!(bench.got, control);
    assert_eq!((bench.dictation_starts, bench.dictation_stops), (1, 1));
}

#[test]
fn no_stop_and_no_idle_are_sent_when_nothing_was_started() {
    let done = dictation_case(&[at(20, Action::End)]);
    let mut timeout = dictation_case(&STREAM);
    timeout.pods.acks = 1;
    timeout.timing.handshake_step = SHORT_NS;
    let mut hang_up = dictation_case(&STREAM);
    hang_up.pods.hang_up_when_ready = true;
    let mut interrupted = dictation_case(&STREAM);
    interrupted.interrupted = true;
    let mut unlisted = dictation_case(&STREAM);
    unlisted.pods.att.table = Some(vec![0x0001]);
    for (name, case) in [
        ("script done", done),
        ("handshake timeout", timeout),
        ("peer hang up", hang_up),
        ("interrupted before the run", interrupted),
        ("handles missing", unlisted),
    ] {
        let report = case.run();
        let att_got = &report.pods.att_got;
        let written = att_got.iter().any(|pdu| *pdu == START || *pdu == STOP);
        assert!(!written, "{name}: {att_got:02x?}");
        let got = report.pods.got();
        let marked = [&SESSION_ACTIVE[..], &SESSION_IDLE];
        assert!(
            !got.iter().any(|sdu| marked.contains(sdu)),
            "{name}: {got:02x?}"
        );
        let outcome = report.outcome();
        let counts = (outcome.dictation_starts, outcome.dictation_stops);
        assert_eq!(counts, (0, 0), "{name}");
        assert_eq!(stream_kinds(&report), ["end"], "{name}");
    }
}

#[test]
fn att_closed_by_the_peer_ends_the_run_and_still_sends_the_idle_marker() {
    let mut case = dictation_case(&OPEN_STREAM);
    case.pods.att.hang_up_after = Some(3);
    let report = case.run();

    let outcome = report.outcome();
    assert_eq!(outcome.end, EndReason::AttClosed);
    assert_eq!(report.mark("end"), "the pods closed the attribute channel");
    assert!(in_order_and_last(
        &report.pods.got(),
        &SESSION_ACTIVE,
        &SESSION_IDLE
    ));
    assert_eq!(report.mark("idle"), "session idle");
    // The stop write was tried on the channel the pods had closed, so it is marked and not counted.
    let stop = report.mark("dictation-stop");
    assert!(
        stop.starts_with("dictation stop failed: could not send"),
        "{stop}"
    );
    assert_eq!((outcome.dictation_starts, outcome.dictation_stops), (1, 0));
    let sentence = summary(&outcome, Route::Dictation);
    let rescue = "the dictation stop was NOT sent, so run cargo xtask probe stop --route dictation";
    assert!(sentence.contains(rescue), "{sentence}");
    assert!(report.returned_ns - report.marks("idle")[0].0 >= TAIL_NS);
}

#[test]
fn both_streams_are_stopped_in_the_order_of_the_script() {
    let uplink_first = [
        "active",
        "dictation-start",
        "start",
        "stop",
        "dictation-stop",
        "idle",
        "end",
    ];
    let dictation_first = [
        "active",
        "dictation-start",
        "start",
        "dictation-stop",
        "stop",
        "idle",
        "end",
    ];
    let scripted = [
        at(0, Action::MarkActive),
        at(5, Action::DictationStart),
        at(10, Action::Start),
        at(30, Action::Stop),
        at(40, Action::DictationStop),
        at(45, Action::MarkIdle),
        at(60, Action::End),
    ];
    // The guards drop the uplink first, so only a script that stops the dictation stream first tells its order from theirs.
    let reversed = [
        at(0, Action::MarkActive),
        at(5, Action::DictationStart),
        at(10, Action::Start),
        at(30, Action::DictationStop),
        at(40, Action::Stop),
        at(45, Action::MarkIdle),
        at(60, Action::End),
    ];
    let running = [
        at(0, Action::MarkActive),
        at(5, Action::DictationStart),
        at(10, Action::Start),
        at(PATIENT_MS, Action::End),
    ];
    let mut cut_short = dictation_case(&running);
    // The uplink is the later of the two to start, so both run when its third packet ends the run.
    cut_short.pods.interrupt_after = Some(3);
    for (name, case, end, order) in [
        (
            "the script stops the uplink first",
            dictation_case(&scripted),
            EndReason::ScriptDone,
            uplink_first,
        ),
        (
            "the script stops the dictation stream first",
            dictation_case(&reversed),
            EndReason::ScriptDone,
            dictation_first,
        ),
        (
            "a signal stops both",
            cut_short,
            EndReason::Interrupted,
            uplink_first,
        ),
    ] {
        let report = case.run();
        assert_eq!(stream_kinds(&report), order, "{name}");
        let stamps: Vec<u64> = order.map(|kind| report.marks(kind)[0].0).to_vec();
        assert!(stamps.is_sorted(), "{name}: {stamps:?}");
        let got = report.pods.got();
        let uplink_start = got.iter().position(|sdu| *sdu == uplink::START);
        let uplink_stop = got.iter().position(|sdu| *sdu == uplink::STOP);
        assert!(
            uplink_start.is_some() && uplink_start < uplink_stop,
            "{name}: {got:02x?}"
        );
        assert_eq!(report.pods.count(&uplink::STOP), 1, "{name}");
        // On the control channel the uplink STOP leaves before the idle marker, which is the last packet of the run.
        let idle = got.iter().position(|sdu| *sdu == SESSION_IDLE);
        assert!(
            uplink_stop < idle && idle == got.len().checked_sub(1),
            "{name}: {got:02x?}"
        );
        let att_got: Vec<&[u8]> = report.pods.att_got.iter().map(Vec::as_slice).collect();
        assert!(
            in_order_and_last(&att_got, &START, &STOP),
            "{name}: {att_got:02x?}"
        );
        let outcome = report.outcome();
        assert_eq!((outcome.starts, outcome.stops), (1, 1), "{name}");
        let counts = (outcome.dictation_starts, outcome.dictation_stops);
        assert_eq!(counts, (1, 1), "{name}");
        assert_eq!(outcome.end, end, "{name}");
    }
}

#[test]
fn a_receive_error_on_the_attribute_channel_ends_the_run_and_the_stop_still_goes_out() {
    // A socket pair cannot fail a receive while its far end lives to see the stop, so the error is handed to the loop's own step.
    let failed = ChannelError::Recv(Errno::IO);
    let bench = on_two_channels(&bench_script(), |engine, clock, _far| {
        engine.checked = true;
        engine.mark_active().unwrap();
        engine.dictation_start().unwrap();
        engine.take(clock.get(), wake_of(Bearer::Att, Err(failed), &[]))
    });

    let end = bench.result.unwrap_err();
    assert_eq!(end, EndReason::Io(IoFailure::Channel(Bearer::Att, failed)));
    assert_eq!(
        end.to_string(),
        format!("{failed}, on the attribute channel")
    );
    assert_eq!(bench.att_got, [&START[..], &STOP[..]]);
    assert_eq!(bench.got, [&SESSION_ACTIVE[..], &SESSION_IDLE]);
    assert_eq!(bench.recorded, Ok(()));
    assert_eq!((bench.dictation_starts, bench.dictation_stops), (1, 1));
}

#[test]
fn a_dictation_start_that_does_not_leave_ends_the_run_and_is_not_counted() {
    let bench = on_two_channels(&bench_script(), |engine, _clock, far| {
        engine.checked = true;
        // With the far end closed, nothing the loop sends on the attribute channel can leave.
        drop(far.take());
        let started = engine.dictation_start();
        (started, engine.start_sent_ns)
    });

    let pipe = IoFailure::Channel(Bearer::Att, ChannelError::Send(Errno::PIPE));
    assert_eq!(bench.result, (Err(EndReason::Io(pipe)), None));
    assert_eq!(bench.recorded, Err(pipe));
    assert_eq!((bench.dictation_starts, bench.dictation_stops), (0, 0));
    // A write that failed is marked as `0\tdictation-start\tdictation start failed: could not send on the L2CAP channel: Broken pipe (os error 32)`; the stamp and the error are cut off.
    let texts: Vec<&str> = bench
        .marks
        .lines()
        .map(|line| line.split_once(": ").unwrap().0)
        .map(|line| line.split_once('\t').unwrap().1)
        .collect();
    let expected = [
        "dictation-start\tdictation start failed",
        "dictation-stop\tdictation stop failed",
    ];
    assert_eq!(texts, expected);
}
