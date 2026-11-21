//! Single steps of the loop, taken on a socket pair that no thread plays, for what the fake pods cannot bring about.

use std::cell::Cell;
use std::io;

use pods_proto::packets::{HANDSHAKE_ACK_PREFIX, SET_SPECIFIC_FEATURES};
use pods_proto::sdulog::{self, Direction};
use pods_proto::uplink::{START, STOP};

use super::super::super::script::rescue;
use super::super::*;
use super::pods::{audio_sdu, pair, queued};

/// What the clock of a loop built by hand always reads.
const BENCH_NOW_NS: u64 = 5;
/// The tail of the guards dropped by hand, in readings of a clock that moves by one at each reading.
const TAIL_READINGS: u64 = 6;
/// How long one wait of that tail lasts.
const TAIL_POLL: Duration = Duration::from_millis(1);

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
fn the_channel_stays_open_after_a_restore_that_no_stop_preceded() {
    // No socket pair can refuse a STOP and then take the restore, so the guards are dropped by hand with no STOP sent.
    let (near, far) = pair();
    near.set_recv_timeout(TAIL_POLL).unwrap();
    let clock = Cell::new(0_u64);
    let now_ns = || clock.replace(clock.get() + 1);
    let link = Link {
        channel: &near,
        now_ns: &now_ns,
    };
    let original = MicSide::Right.command();
    let dropped = Dropped::default();
    {
        let _tail = Tail {
            link,
            length_ns: TAIL_READINGS,
            dropped: &dropped,
        };
        let _side = SideChange {
            link,
            original,
            dropped: &dropped,
        };
    }
    let (_, restore) = dropped.restore.get().unwrap();
    assert_eq!(restore.result, Ok(()));
    assert_eq!(queued(&far), [original.encode()]);
    assert_eq!(dropped.tail_from_ns.get(), Some(restore.at_ns));
    assert!(clock.get() > restore.at_ns + TAIL_READINGS, "{clock:?}");

    drop(far);
    let dropped = Dropped::default();
    drop(SideChange {
        link,
        original,
        dropped: &dropped,
    });
    let (_, restore) = dropped.restore.get().unwrap();
    assert_eq!(restore.result, Err(ChannelError::Send(Errno::PIPE)));
    assert_eq!(dropped.tail_from_ns.get(), None);
}

/// A writer that takes nothing, as a full disk would.
struct Full;

impl io::Write for Full {
    fn write(&mut self, _data: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("no space left"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_packet_the_log_format_refuses_is_marked_and_counted_once() {
    let (mut log, mut marks, mut cues) = (Vec::new(), Vec::new(), Vec::new());
    let mut out = Outputs {
        log: &mut log,
        marks: &mut marks,
        cues: &mut cues,
    };
    let mut recorder = Recorder::new(&mut out);
    recorder.received(7, &[]).unwrap();
    let too_long = vec![0x33; sdulog::MAX_SDU_LEN + 1];
    let logged = recorder.packet(9, Direction::ToPods, &too_long);
    assert_eq!(logged, Ok(false));
    recorder.received(11, &audio_sdu(0)).unwrap();
    let counts = (recorder.sdus_in, recorder.audio_sdus, recorder.truncated);
    assert_eq!(counts, (1, 1, 2));
    let log = String::from_utf8(log).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 1, "{log}");
    assert_eq!(sdulog::parse_line(lines[0]).unwrap().sdu, audio_sdu(0));
    let marks = String::from_utf8(marks).unwrap();
    let refused: Vec<&str> = marks.lines().collect();
    assert_eq!(refused.len(), 2, "{marks}");
    assert!(refused[0].starts_with("7\ttruncated\t0 bytes: "), "{marks}");
    assert!(
        refused[1].starts_with("9\ttruncated\t4097 bytes: "),
        "{marks}"
    );
}

#[test]
fn a_packet_is_counted_only_once_its_line_or_mark_was_written() {
    let (mut log, mut marks, mut cues) = (Full, Full, Vec::new());
    let mut out = Outputs {
        log: &mut log,
        marks: &mut marks,
        cues: &mut cues,
    };
    let mut recorder = Recorder::new(&mut out);
    let full = |file| {
        Err(IoFailure::Write {
            file,
            kind: io::ErrorKind::Other,
        })
    };
    assert_eq!(recorder.received(7, &audio_sdu(0)), full(LOG_NAME));
    assert_eq!(recorder.received(8, &[]), full(MARKS_NAME));
    let counts = (recorder.sdus_in, recorder.audio_sdus, recorder.truncated);
    assert_eq!(counts, (0, 0, 0));
}
