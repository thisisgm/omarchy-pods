//! The capture and stop commands, run with a temporary home and a connect that the test supplies.

use std::cell::Cell;
use std::fs;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use pods_proto::addr::BdAddr;
use pods_proto::packets::{HANDSHAKE, REQUEST_NOTIFICATIONS, SET_SPECIFIC_FEATURES};
use pods_proto::sdulog::{self, Direction};
use pods_proto::uplink::{START, STOP};
use pods_sys::l2cap::{Channel, ChannelError};
use rustix::io::Errno;
use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};

use super::super::super::handshake::Step;
use super::super::super::script::ScriptName;
use super::super::super::{CaptureArgs, Host, capture, exit_code, stop, summary, watch_signals};
use super::super::{EndReason, IoFailure, Outcome};
use super::pods::{self, DEADLINE, EAR_PACKET, Pods};
use super::{ROOT_NAME, home_with_device};

/// A stamp of the shape `date -u +%Y%m%dT%H%M%SZ` prints.
const STAMP: &str = "20261120T120000Z";
/// The octets of the address in the device file.
const DEVICE_OCTETS: [u8; 6] = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
/// The address in the forms a leak could take, compared after lowering the output.
const ADDRESS_FORMS: [&str; 4] = ["aa:bb", "ff:ee", "aabbccddeeff", "ffeeddccbbaa"];
/// What a formatter could put between two bytes of the address; the last two forms are also looked for with these removed.
const SEPARATORS: [&str; 9] = ["0x", ":", "-", ",", ".", " ", "[", "]", "\t"];

/// How a command exited and what it left on its two streams.
struct Ran {
    code: String,
    stdout: String,
    stderr: String,
}

/// Runs `command` on a thread with a connect that hands over `near` once, and waits for it no longer than the deadline.
fn run_command(
    home: &Path,
    near: Channel,
    command: impl FnOnce(&mut Host<'_>) -> ExitCode + Send + 'static,
) -> Ran {
    let home = home.to_str().unwrap().to_owned();
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let near = Cell::new(Some(near));
        let connect = |_peer: BdAddr| near.take().ok_or(ChannelError::Connect(Errno::ALREADY));
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let code = command(&mut Host {
            home: &home,
            connect: &connect,
            stdout: &mut stdout,
            stderr: &mut stderr,
        });
        done.send(Ran {
            code: format!("{code:?}"),
            stdout: String::from_utf8(stdout).unwrap(),
            stderr: String::from_utf8(stderr).unwrap(),
        })
    });
    let ran = result.recv_timeout(DEADLINE);
    ran.expect("the command did not return before the deadline")
}

#[test]
fn the_summary_says_whether_stop_followed_start() {
    let outcome = |starts, stops, end| Outcome {
        sdus_in: 9,
        audio_sdus: 4,
        starts,
        stops,
        truncated: 0,
        end,
    };
    let done = summary(&outcome(1, 1, EndReason::ScriptDone));
    assert!(done.contains("packets in 9, with audio 4, "), "{done}");
    let stopped = summary(&outcome(1, 1, EndReason::Interrupted));
    assert!(
        stopped.contains("interrupted") && stopped.contains("STOP was sent"),
        "{stopped}"
    );
    let running = summary(&outcome(2, 1, EndReason::PeerClosed));
    assert!(running.contains("STOP was NOT sent"), "{running}");
    assert!(running.contains("cargo xtask probe stop"), "{running}");
    let send = IoFailure::Channel(ChannelError::Send(Errno::PIPE));
    let failed = summary(&outcome(0, 0, EndReason::Io(send)));
    assert!(
        failed.contains("could not send on the L2CAP channel"),
        "{failed}"
    );
    assert!(!failed.contains("STOP was"), "{failed}");
}

#[test]
fn only_a_script_that_ran_to_its_end_exits_zero() {
    let exit = |end| {
        let outcome = Outcome {
            sdus_in: 0,
            audio_sdus: 0,
            starts: 1,
            stops: 1,
            truncated: 0,
            end,
        };
        format!("{:?}", exit_code(&outcome))
    };
    assert_eq!(
        exit(EndReason::ScriptDone),
        format!("{:?}", ExitCode::SUCCESS)
    );
    let send = IoFailure::Channel(ChannelError::Send(Errno::PIPE));
    for end in [
        EndReason::Interrupted,
        EndReason::PeerClosed,
        EndReason::HandshakeTimeout(Step::AwaitFeaturesAck),
        EndReason::NoInitialSide,
        EndReason::Io(send),
    ] {
        assert_eq!(exit(end), format!("{:?}", ExitCode::FAILURE), "{end:?}");
    }
}

#[test]
fn every_signal_that_would_end_a_capture_raises_the_flag_the_loop_reads() {
    let interrupted = watch_signals().unwrap();
    assert!(!interrupted.load(Ordering::SeqCst));
    for signal in [SIGINT, SIGTERM, SIGHUP, SIGQUIT] {
        interrupted.store(false, Ordering::SeqCst);
        // A raise is delivered to this thread before it returns, so no wait is needed.
        signal_hook::low_level::raise(signal).unwrap();
        assert!(interrupted.load(Ordering::SeqCst), "signal {signal}");
    }
}

#[test]
fn the_knock_command_records_a_run_and_exits_zero() {
    let home = home_with_device();
    let (near, far) = pods::pair();
    far.send(&EAR_PACKET).unwrap();
    let args = CaptureArgs {
        stamp: STAMP.to_owned(),
        run: "knock-1".to_owned(),
        script: ScriptName::Knock,
        minutes: None,
    };
    let ran = run_command(home.path(), near, move |host| {
        capture(&args, host, &AtomicBool::new(false))
    });

    assert_eq!(
        ran.code,
        format!("{:?}", ExitCode::SUCCESS),
        "{}",
        ran.stderr
    );
    assert_eq!(
        ran.stderr,
        "xtask probe: capture complete: packets in 1, with audio 0, not logged 0, START sent 0, STOP sent 0\n"
    );
    assert_eq!(ran.stdout, "");
    assert_eq!(pods::queued(&far), Vec::<Vec<u8>>::new());
    let run = home.path().join(ROOT_NAME).join(STAMP).join("knock-1");
    let log = fs::read_to_string(run.join("sdu.log")).unwrap();
    let records: Vec<_> = log
        .lines()
        .map(|line| sdulog::parse_line(line).unwrap())
        .collect();
    assert_eq!(records.len(), 1, "{log}");
    assert_eq!(records[0].direction, Direction::FromPods);
    assert_eq!(records[0].sdu, EAR_PACKET);
    let marks = fs::read_to_string(run.join("marks.tsv")).unwrap();
    let kinds: Vec<&str> = marks
        .lines()
        .map(|line| line.split('\t').nth(1).unwrap())
        .collect();
    assert_eq!(kinds, ["connect", "gate", "end"], "{marks}");
    let meta = fs::read_to_string(run.join("meta.txt")).unwrap();
    assert!(
        meta.starts_with("script=knock\ntool=xtask probe capture "),
        "{meta}"
    );
}

#[test]
fn an_interrupted_soak_shows_its_cue_on_stdout_and_says_stop_was_sent() {
    let home = home_with_device();
    let (near, far) = pods::pair();
    let interrupted = Arc::new(AtomicBool::new(false));
    let part = Pods {
        interrupt_after: Some(3),
        ..Pods::default()
    };
    let transcript = pods::spawn(far, part, Arc::clone(&interrupted));
    let args = CaptureArgs {
        stamp: STAMP.to_owned(),
        run: "soak-1".to_owned(),
        script: ScriptName::Soak,
        minutes: Some(1),
    };
    let ran = run_command(home.path(), near, move |host| {
        capture(&args, host, &interrupted)
    });

    let pods = transcript.recv_timeout(DEADLINE);
    let pods = pods.expect("the fake pods did not finish before the deadline");
    let sent = [
        &HANDSHAKE[..],
        &SET_SPECIFIC_FEATURES,
        &REQUEST_NOTIFICATIONS,
        &START,
        &STOP,
    ];
    assert_eq!(pods.got(), sent);
    assert_eq!(ran.code, format!("{:?}", ExitCode::FAILURE));
    assert_eq!(ran.stdout, "cue Stay as you are. Nothing to do.\n");
    let said =
        "xtask probe: capture ended early: interrupted by a signal; STOP was sent (packets in ";
    assert!(ran.stderr.starts_with(said), "{}", ran.stderr);
    assert!(
        ran.stderr.ends_with(", START sent 1, STOP sent 1)\n"),
        "{}",
        ran.stderr
    );
    let run = home.path().join(ROOT_NAME).join(STAMP).join("soak-1");
    let marks = fs::read_to_string(run.join("marks.tsv")).unwrap();
    assert!(
        marks.ends_with("\tend\tinterrupted by a signal\n"),
        "{marks}"
    );
}

#[test]
fn the_stop_command_sends_stop_and_keeps_nothing() {
    let home = home_with_device();
    let (near, far) = pods::pair();
    let transcript = pods::spawn(far, Pods::default(), Arc::new(AtomicBool::new(false)));
    let ran = run_command(home.path(), near, stop);

    let pods = transcript.recv_timeout(DEADLINE);
    let pods = pods.expect("the fake pods did not finish before the deadline");
    let sent = [
        &HANDSHAKE[..],
        &SET_SPECIFIC_FEATURES,
        &REQUEST_NOTIFICATIONS,
        &STOP,
    ];
    assert_eq!(pods.got(), sent);
    assert_eq!(
        ran.code,
        format!("{:?}", ExitCode::SUCCESS),
        "{}",
        ran.stderr
    );
    assert_eq!(
        ran.stderr,
        "xtask probe: STOP was sent and the channel was kept open for 1000 ms\n"
    );
    assert_eq!(ran.stdout, "");
    let kept = files_under(&home.path().join(ROOT_NAME));
    assert_eq!(kept.len(), 1, "{kept:?}");
}

#[test]
fn the_stop_command_says_when_stop_was_not_sent() {
    let home = home_with_device();
    let (near, far) = pods::pair();
    let part = Pods {
        hang_up_when_ready: true,
        ..Pods::default()
    };
    let transcript = pods::spawn(far, part, Arc::new(AtomicBool::new(false)));
    let ran = run_command(home.path(), near, stop);

    let pods = transcript.recv_timeout(DEADLINE);
    let pods = pods.expect("the fake pods did not finish before the deadline");
    assert_eq!(pods.count(&STOP), 0);
    assert_eq!(ran.code, format!("{:?}", ExitCode::FAILURE));
    assert_eq!(
        ran.stderr,
        "xtask probe: STOP was not sent: the pods closed the channel\n"
    );
}

/// Every regular file under `dir`, with its path and its bytes read as text.
fn files_under(dir: &Path) -> Vec<(String, String)> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(files_under(&path));
        } else {
            let text = String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned();
            files.push((path.display().to_string(), text));
        }
    }
    files
}

#[test]
fn output_never_contains_the_device_address() {
    let home = home_with_device();
    let root = home.path().join(ROOT_NAME);
    let args = CaptureArgs {
        stamp: STAMP.to_owned(),
        run: "page".to_owned(),
        script: ScriptName::Listen,
        minutes: Some(1),
    };
    let dialled = Cell::new(None);
    let connect = |peer: BdAddr| {
        dialled.set(Some(peer.octets()));
        Err(ChannelError::Connect(Errno::HOSTDOWN))
    };
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut host = Host {
        home: home.path().to_str().unwrap(),
        connect: &connect,
        stdout: &mut stdout,
        stderr: &mut stderr,
    };
    let code = capture(&args, &mut host, &AtomicBool::new(false));

    assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
    assert_eq!(
        dialled.get(),
        Some(DEVICE_OCTETS),
        "the connect was not reached"
    );
    let stderr = String::from_utf8(stderr).unwrap();
    assert!(
        stderr.contains("could not connect the L2CAP channel"),
        "{stderr}"
    );
    let run = root.join(STAMP).join("page");
    let mut outputs = vec![
        ("stdout".to_owned(), String::from_utf8(stdout).unwrap()),
        ("stderr".to_owned(), stderr),
    ];
    outputs.extend(files_under(&run));
    let names: Vec<&str> = outputs.iter().map(|(name, _)| name.as_str()).collect();
    for file in ["sdu.log", "marks.tsv", "meta.txt"] {
        assert!(names.iter().any(|name| name.ends_with(file)), "{names:?}");
    }
    for (name, text) in &outputs {
        let lowered = format!("{name}\n{text}").to_lowercase();
        let mut bare = lowered.clone();
        for separator in SEPARATORS {
            bare = bare.replace(separator, "");
        }
        for form in ADDRESS_FORMS {
            assert!(!lowered.contains(form), "{form} is in {name}:\n{text}");
            assert!(!bare.contains(form), "{form} is in {name}:\n{text}");
        }
        if name.ends_with("marks.tsv") {
            assert!(
                text.contains("\tend\tcould not connect the L2CAP channel"),
                "{text}"
            );
        }
        if name.ends_with("meta.txt") {
            let keys: Vec<&str> = text
                .lines()
                .map(|line| line.split('=').next().unwrap())
                .collect();
            assert_eq!(keys, ["script", "tool", "realtime_at_zero_ns"], "{text}");
            assert!(text.starts_with("script=listen\n"), "{text}");
        }
    }
}
