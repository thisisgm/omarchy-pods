//! Single steps of the loop, taken on a socket pair that no thread plays, for what the fake pods cannot bring about.

use std::io;

use pods_proto::packets::{HANDSHAKE_ACK_PREFIX, SET_SPECIFIC_FEATURES};
use pods_proto::sdulog::{self, Direction};
use pods_proto::uplink::{START, STOP};

use super::super::super::script::rescue;
use super::super::*;
use super::pods::{pair, queued};

/// What the clock of a loop built by hand always reads.
const BENCH_NOW_NS: u64 = 5;

/// What a loop built by hand left behind.
struct Bench<T> {
    result: T,
    /// Whether what the guards sent as they dropped was sent and recorded.
    recorded: Result<(), IoFailure>,
    /// The packets the far end got, in order; none when `drive` closed it.
    got: Vec<Vec<u8>>,
    starts: u32,
    stops: u32,
    marks: String,
}

/// Builds the loop in `phase` on a socket pair that no thread plays, hands it and the far end to `drive`, and drops it as a return would.
fn on_a_bench<T>(
    phase: Phase,
    drive: impl FnOnce(&mut Engine<'_, '_>, &mut Option<Channel>) -> T,
) -> Bench<T> {
    let (near, far) = pair();
    let mut far = Some(far);
    let now_ns = || BENCH_NOW_NS;
    let link = Link {
        channel: &near,
        now_ns: &now_ns,
    };
    let script = rescue();
    let (mut log, mut marks, mut cues) = (Vec::new(), Vec::new(), Vec::new());
    let mut out = Outputs {
        log: &mut log,
        marks: &mut marks,
        cues: &mut cues,
    };
    let mut recorder = Recorder::new(&mut out);
    let dropped = Dropped::default();
    let interrupted = AtomicBool::new(false);
    let result = {
        let mut engine = Engine {
            link,
            script: &script,
            recorder: &mut recorder,
            interrupted: &interrupted,
            timing: &Timing::REAL,
            dropped: &dropped,
            phase,
            ear_seen: false,
            initial_side: None,
            uplink: None,
            side: None,
        };
        drive(&mut engine, &mut far)
    };
    let guards = record_dropped(&mut recorder, &dropped);
    let (starts, stops) = (recorder.starts, recorder.stops);
    Bench {
        result,
        recorded: guards,
        got: far.as_ref().map_or_default(queued),
        starts,
        stops,
        marks: String::from_utf8(marks).unwrap(),
    }
}

#[test]
fn a_timeout_and_a_signal_wake_the_loop_and_no_other_error_does() {
    let buf = [0x09_u8; 4];
    for idle in [Ok(Received::TimedOut), Err(ChannelError::Recv(Errno::INTR))] {
        assert!(matches!(wake_of(idle, &buf), Wake::Idle), "{idle:?}");
    }
    for failed in [
        ChannelError::Recv(Errno::IO),
        ChannelError::Recv(Errno::CONNRESET),
        ChannelError::Send(Errno::INTR),
    ] {
        let wake = wake_of(Err(failed), &buf);
        assert!(
            matches!(wake, Wake::Failed(error) if error == failed),
            "{failed:?}"
        );
    }
    assert!(matches!(wake_of(Ok(Received::Closed), &buf), Wake::Closed));
    let whole = wake_of(Ok(Received::Sdu(3)), &buf);
    assert!(matches!(whole, Wake::Sdu(sdu) if sdu == [0x09; 3]));
    let cut = wake_of(Ok(Received::Truncated { full_len: 9 }), &buf);
    assert!(matches!(cut, Wake::TooLong { full_len: 9 }));
    let past_the_buffer = wake_of(Ok(Received::Sdu(5)), &buf);
    assert!(matches!(past_the_buffer, Wake::TooLong { full_len: 5 }));
}

#[test]
fn a_receive_error_ends_the_run_as_io_and_stop_still_goes_out() {
    // A socket pair cannot fail a receive while its far end lives to see STOP, so the error is handed to the loop's own step.
    let failed = ChannelError::Recv(Errno::IO);
    let scripted = Phase::Scripted {
        gate_ns: 0,
        next: 0,
    };
    let bench = on_a_bench(scripted, |engine, _far| {
        engine.start().unwrap();
        engine.take(BENCH_NOW_NS, wake_of(Err(failed), &[]))
    });

    assert_eq!(bench.result, Err(EndReason::Io(IoFailure::Channel(failed))));
    assert_eq!(bench.got, [&START[..], &STOP[..]]);
    assert_eq!(bench.recorded, Ok(()));
    assert_eq!((bench.starts, bench.stops), (1, 1));
    assert!(
        bench.marks.ends_with("5\tstop\tuplink stop\n"),
        "{}",
        bench.marks
    );
}

#[test]
fn a_start_that_does_not_leave_ends_the_run_and_is_not_counted() {
    let scripted = Phase::Scripted {
        gate_ns: 0,
        next: 0,
    };
    let bench = on_a_bench(scripted, |engine, far| {
        // With the far end closed, nothing the loop sends can leave.
        drop(far.take());
        engine.start()
    });

    let pipe = IoFailure::Channel(ChannelError::Send(Errno::PIPE));
    assert_eq!(bench.result, Err(EndReason::Io(pipe)));
    assert_eq!(bench.recorded, Err(pipe));
    assert_eq!((bench.starts, bench.stops), (0, 0));
    let texts: Vec<&str> = bench
        .marks
        .lines()
        .map(|line| line.split_once(": ").unwrap().0)
        .collect();
    assert_eq!(
        texts,
        [
            "5\tstart\tuplink start failed",
            "5\tstop\tuplink stop failed"
        ]
    );
}

#[test]
fn each_handshake_request_restarts_the_wait_for_its_answer() {
    let handshaking = Phase::Handshaking {
        handshake: Handshake::default(),
        asked_at_ns: 0,
    };
    let bench = on_a_bench(handshaking, |engine, _far| {
        let acked = engine.on_packet(BENCH_NOW_NS, &HANDSHAKE_ACK_PREFIX);
        acked.unwrap();
        match engine.phase {
            Phase::Handshaking { asked_at_ns, .. } => Some(asked_at_ns),
            Phase::Settling { .. } | Phase::Scripted { .. } => None,
        }
    });

    assert_eq!(bench.result, Some(BENCH_NOW_NS));
    assert_eq!(bench.got, [&SET_SPECIFIC_FEATURES[..]]);
}

#[test]
fn a_late_failure_fails_a_finished_run_and_an_earlier_reason_stands() {
    let late = IoFailure::Write {
        file: MARKS_NAME,
        kind: io::ErrorKind::Other,
    };
    assert_eq!(
        first_failure(EndReason::ScriptDone, Err(late)),
        EndReason::Io(late)
    );
    assert_eq!(
        first_failure(EndReason::ScriptDone, Ok(())),
        EndReason::ScriptDone
    );
    assert_eq!(
        first_failure(EndReason::PeerClosed, Err(late)),
        EndReason::PeerClosed
    );
}

#[test]
fn the_side_is_read_only_before_the_gate() {
    let reported = MicSide::Right.command().encode();
    let read = |phase| {
        let bench = on_a_bench(phase, |engine, _far| {
            engine.on_packet(BENCH_NOW_NS, &reported).unwrap();
            engine.initial_side.map(|side| side.encode())
        });
        assert_eq!(bench.got, Vec::<Vec<u8>>::new());
        bench.result
    };
    assert_eq!(
        read(Phase::Settling { asked_at_ns: 0 }).as_ref(),
        Some(&reported)
    );
    let scripted = Phase::Scripted {
        gate_ns: 0,
        next: 0,
    };
    assert_eq!(read(scripted), None);
}

#[test]
fn a_packet_the_log_format_refuses_is_marked_and_counted() {
    let (mut log, mut marks, mut cues) = (Vec::new(), Vec::new(), Vec::new());
    let mut out = Outputs {
        log: &mut log,
        marks: &mut marks,
        cues: &mut cues,
    };
    let mut recorder = Recorder::new(&mut out);
    recorder.packet(7, Direction::FromPods, &[]).unwrap();
    let too_long = vec![0x33; sdulog::MAX_SDU_LEN + 1];
    recorder.packet(9, Direction::ToPods, &too_long).unwrap();
    assert_eq!(recorder.truncated, 2);
    assert_eq!(log, b"");
    let marks = String::from_utf8(marks).unwrap();
    let refused: Vec<&str> = marks.lines().collect();
    assert_eq!(refused.len(), 2, "{marks}");
    assert!(refused[0].starts_with("7\ttruncated\t0 bytes: "), "{marks}");
    assert!(
        refused[1].starts_with("9\ttruncated\t4097 bytes: "),
        "{marks}"
    );
}
