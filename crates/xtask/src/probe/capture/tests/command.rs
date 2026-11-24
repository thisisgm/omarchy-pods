//! The capture and stop commands, run with a temporary home and a connect that the test supplies.

use std::cell::{Cell, RefCell};
use std::fs;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use pods_proto::addr::BdAddr;
use pods_proto::handshake::Step;
use pods_proto::packets::{CONTROL_PSM, HANDSHAKE, REQUEST_NOTIFICATIONS, SET_SPECIFIC_FEATURES};
use pods_proto::sdulog::{self, Direction};
use pods_proto::uplink::{START, STOP};
use pods_sys::l2cap::{Channel, ChannelError};
use rustix::io::Errno;
use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};

use super::super::super::analyse::input::parse_meta;
use super::super::super::dictate::HandleCheck;
use super::super::super::script::{Route, ScriptName};
use super::super::super::service::Stopped;
use super::super::super::{CaptureArgs, Host, capture, exit_code, stop, summary, watch_signals};
use super::super::{Bearer, EndReason, IoFailure, Outcome};
use super::pods::{self, DEADLINE, EAR_PACKET, Pods};
use super::{DEVICE_LINE, ROOT_NAME, counted_nothing, home_with_device};

/// A stamp of the shape `date -u +%Y%m%dT%H%M%SZ` prints.
pub(super) const STAMP: &str = "20261120T120000Z";
/// The octets of the address in the device file.
pub(super) const DEVICE_OCTETS: [u8; 6] = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
/// How many octets in a row count as the address: half of it, which already names the maker or the device.
const TRACE_OCTETS: usize = 3;

/// A packet of the accessory header and an opcode the loop does not know, which the fake pods send here once STOP has reached them.
const PARTING: [u8; 7] = [0x04, 0x00, 0x04, 0x00, 0x7f, 0x00, 0x01];

pub(super) struct Ran {
    pub(super) code: String,
    pub(super) stdout: String,
    pub(super) stderr: String,
    pub(super) dialled: Vec<u16>,
}

/// Runs `command` on a thread with a connect that hands over `near` once for the control PSM, and waits for it no longer than the deadline.
fn run_command(
    home: &Path,
    near: Channel,
    command: impl FnOnce(&mut Host<'_>) -> ExitCode + Send + 'static,
) -> Ran {
    run_dialled(home, vec![(CONTROL_PSM, near)], command)
}

/// Runs `command` on a thread with a connect that hands over each of `lines` once to the PSM it is listed under and refuses every other PSM.
pub(super) fn run_dialled(
    home: &Path,
    lines: Vec<(u16, Channel)>,
    command: impl FnOnce(&mut Host<'_>) -> ExitCode + Send + 'static,
) -> Ran {
    let lines = RefCell::new(lines);
    let answer = move |psm| {
        let mut lines = lines.borrow_mut();
        let line = lines.iter().position(|(listed, _)| *listed == psm);
        let line = line.ok_or(ChannelError::Connect(Errno::CONNREFUSED))?;
        Ok(lines.remove(line).1)
    };
    run_answered(home, answer, command)
}

pub(super) fn run_answered(
    home: &Path,
    answer: impl Fn(u16) -> Result<Channel, ChannelError> + Send + 'static,
    command: impl FnOnce(&mut Host<'_>) -> ExitCode + Send + 'static,
) -> Ran {
    let home = home.to_str().unwrap().to_owned();
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let dialled = RefCell::new(Vec::new());
        let connect = |_peer: BdAddr, psm: u16| {
            dialled.borrow_mut().push(psm);
            answer(psm)
        };
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let code = command(&mut Host {
            home: &home,
            connect: &connect,
            stop_service: &|| Ok(Stopped::nothing()),
            stdout: &mut stdout,
            stderr: &mut stderr,
        });
        // The send fails only when the test has stopped waiting for it, and a wait that ran out has already failed the test.
        done.send(Ran {
            code: format!("{code:?}"),
            stdout: String::from_utf8(stdout).unwrap(),
            stderr: String::from_utf8(stderr).unwrap(),
            dialled: dialled.into_inner(),
        })
    });
    let ran = result.recv_timeout(DEADLINE);
    ran.expect("the command did not return before the deadline")
}

/// Part of the device address that `text` holds: octets in a row, in either order, as hex or as decimal numbers, so `AA:BB:CC`, `ccbbaa`, `[170, 187, 204]` and `0xdd 0xee 0xff` each hold a part and `aa bb` and `170` hold none.
fn address_trace(text: &str) -> Option<String> {
    let lowered = text.to_lowercase();
    let words: Vec<&str> = lowered
        .split(|letter: char| !letter.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| word.strip_prefix("0x").unwrap_or(word))
        .collect();
    let mut backwards = DEVICE_OCTETS;
    backwards.reverse();
    for octets in [DEVICE_OCTETS, backwards] {
        for run in octets.windows(TRACE_OCTETS) {
            let hex: Vec<String> = run.iter().map(|octet| format!("{octet:02x}")).collect();
            let decimal: Vec<String> = run.iter().map(u8::to_string).collect();
            if lowered.contains(&hex.concat()) {
                return Some(hex.concat());
            }
            for spelled in [hex, decimal] {
                if words.windows(TRACE_OCTETS).any(|at| at == spelled) {
                    return Some(spelled.join(" "));
                }
            }
        }
    }
    None
}

/// Every entry under `root`, directories too, by its path from `root` and in order; a file comes with its bytes as text.
pub(super) fn entries_under(root: &Path) -> Vec<(String, Option<String>)> {
    let mut pending = vec![root.to_path_buf()];
    let mut entries = Vec::new();
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.strip_prefix(root).unwrap().display().to_string();
            if path.is_dir() {
                entries.push((name, None));
                pending.push(path);
            } else {
                let text = String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned();
                entries.push((name, Some(text)));
            }
        }
    }
    entries.sort();
    entries
}

pub(super) fn assert_no_address(stdout: &str, stderr: &str, run: Option<&Path>) {
    let streams = [("stdout", stdout), ("stderr", stderr)].map(|(name, text)| {
        let text = Some(text.to_owned());
        (name.to_owned(), text)
    });
    let files = run.map_or_default(entries_under);
    for (name, text) in streams.into_iter().chain(files) {
        let text = text.unwrap_or_default();
        let found = address_trace(&format!("{name}\n{text}"));
        assert_eq!(found, None, "in {name}:\n{text}");
    }
}

#[test]
fn the_address_scan_finds_every_form_a_leak_could_take() {
    for leak in [
        "peer AA:BB:CC:DD:EE:FF",
        "aabbccddeeff",
        "ffeeddccbbaa",
        "XX:XX:XX:DD:EE:FF",
        "ff-ee-dd",
        "[170, 187, 204, 221, 238, 255]",
        "BdAddr([170, 187, 204, 221, 238, 255])",
        "0xbb, 0xcc, 0xdd",
        "bb cc\tdd",
        "255.238.221",
    ] {
        assert!(address_trace(leak).is_some(), "{leak}");
    }
    for clean in [
        "",
        "packets in 170, with audio 187, not logged 204",
        "aa:bb",
        "170 187",
        "1701 187 204",
        "aa bb xx cc",
        "could not connect the L2CAP channel: Host is down (os error 112)",
    ] {
        assert_eq!(address_trace(clean), None, "{clean}");
    }
}

#[test]
fn the_summary_says_whether_stop_followed_start() {
    let outcome = |starts, stops, end| Outcome {
        sdus_in: 9,
        audio_sdus: 4,
        starts,
        stops,
        uplink_running: starts > stops,
        truncated: 7,
        att_pdus_in: 11,
        ..counted_nothing(end)
    };
    let summary = |outcome: &Outcome| summary(outcome, Route::Uplink);
    // Every count differs from the others, so one printed under another's name is seen.
    let done = summary(&outcome(3, 5, EndReason::ScriptDone));
    assert_eq!(
        done,
        "capture complete: packets in 9, with audio 4, not logged 7, START sent 3, STOP sent 5"
    );
    let stopped = summary(&outcome(1, 1, EndReason::Interrupted));
    assert_eq!(
        stopped,
        "capture ended early: interrupted by a signal; STOP was sent (packets in 9, with audio 4, not logged 7, START sent 1, STOP sent 1)"
    );
    let running = summary(&outcome(2, 1, EndReason::PeerClosed));
    assert!(running.contains("STOP was NOT sent"), "{running}");
    assert!(running.contains("cargo xtask probe stop"), "{running}");
    // One STOP ends an uplink two STARTs asked for.
    let twice = Outcome {
        uplink_running: false,
        ..outcome(2, 1, EndReason::Interrupted)
    };
    assert!(summary(&twice).contains("; STOP was sent ("), "{twice:?}");
    let send = IoFailure::Channel(Bearer::Accessory, ChannelError::Send(Errno::PIPE));
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
            starts: 1,
            stops: 1,
            ..counted_nothing(end)
        };
        format!("{:?}", exit_code(&outcome))
    };
    assert_eq!(
        exit(EndReason::ScriptDone),
        format!("{:?}", ExitCode::SUCCESS)
    );
    let send = IoFailure::Channel(Bearer::Accessory, ChannelError::Send(Errno::PIPE));
    for end in [
        EndReason::Interrupted,
        EndReason::PeerClosed,
        EndReason::HandshakeTimeout(Step::AwaitFeaturesAck),
        EndReason::NoInitialSide,
        EndReason::AttConnect(ChannelError::Connect(Errno::HOSTDOWN)),
        EndReason::AttClosed,
        EndReason::HandleCheck(HandleCheck::NoListing),
        EndReason::HandleCheck(HandleCheck::Missing(vec![0x0007])),
        EndReason::Io(send),
    ] {
        let code = exit(end.clone());
        assert_eq!(code, format!("{:?}", ExitCode::FAILURE), "{end:?}");
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
        route: Route::Uplink,
        script: Some(ScriptName::Knock),
        steps: None,
        allow_control: false,
        minutes: None,
        no_marker: false,
        unchecked: false,
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
    assert_eq!(ran.dialled, [CONTROL_PSM]);
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
    assert_no_address(&ran.stdout, &ran.stderr, Some(&run));
}

#[test]
fn an_interrupted_soak_shows_its_cue_on_stdout_and_says_stop_was_sent() {
    let home = home_with_device();
    let (near, far) = pods::pair();
    let interrupted = Arc::new(AtomicBool::new(false));
    let part = Pods {
        interrupt_after: Some(3),
        parting: vec![PARTING.to_vec()],
        ..Pods::default()
    };
    let transcript = pods::spawn(far, part, Arc::clone(&interrupted));
    let args = CaptureArgs {
        stamp: STAMP.to_owned(),
        run: "soak-1".to_owned(),
        route: Route::Uplink,
        script: Some(ScriptName::Soak),
        steps: None,
        allow_control: false,
        minutes: Some(1),
        no_marker: false,
        unchecked: false,
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
    // What the pods send once the stop of the way out has reached them is in the log, after that stop.
    let log = fs::read_to_string(run.join("sdu.log")).unwrap();
    let records: Vec<_> = log
        .lines()
        .map(|line| sdulog::parse_line(line).unwrap())
        .collect();
    let stop = records.iter().position(|record| record.sdu == STOP);
    let parting = records.iter().position(|record| record.sdu == PARTING);
    assert!(stop.is_some() && stop < parting, "{log}");
    assert_no_address(&ran.stdout, &ran.stderr, Some(&run));
}

#[test]
fn the_stop_command_sends_stop_and_keeps_nothing() {
    let home = home_with_device();
    let (near, far) = pods::pair();
    let transcript = pods::spawn(far, Pods::default(), Arc::new(AtomicBool::new(false)));
    let ran = run_command(home.path(), near, |host| {
        stop(host, Route::Uplink, &AtomicBool::new(false))
    });

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
    let kept = entries_under(&home.path().join(ROOT_NAME));
    let device = ("device".to_owned(), Some(DEVICE_LINE.to_owned()));
    assert_eq!(kept, [device]);
    assert_no_address(&ran.stdout, &ran.stderr, None);
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
    let ran = run_command(home.path(), near, |host| {
        stop(host, Route::Uplink, &AtomicBool::new(false))
    });

    let pods = transcript.recv_timeout(DEADLINE);
    let pods = pods.expect("the fake pods did not finish before the deadline");
    assert_eq!(pods.count(&STOP), 0);
    assert_eq!(ran.code, format!("{:?}", ExitCode::FAILURE));
    assert_eq!(
        ran.stderr,
        "xtask probe: STOP was not sent: the pods closed the channel\n"
    );
    assert_no_address(&ran.stdout, &ran.stderr, None);
}

#[test]
fn output_never_contains_the_device_address() {
    let home = home_with_device();
    let root = home.path().join(ROOT_NAME);
    let args = CaptureArgs {
        stamp: STAMP.to_owned(),
        run: "page".to_owned(),
        route: Route::Uplink,
        script: Some(ScriptName::Listen),
        steps: None,
        allow_control: false,
        minutes: Some(1),
        no_marker: false,
        unchecked: false,
    };
    let dialled = Cell::new(None);
    let connect = |peer: BdAddr, _psm: u16| {
        dialled.set(Some(peer.octets()));
        Err(ChannelError::Connect(Errno::HOSTDOWN))
    };
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut host = Host {
        home: home.path().to_str().unwrap(),
        connect: &connect,
        stop_service: &|| Ok(Stopped::nothing()),
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
    assert_no_address(&String::from_utf8(stdout).unwrap(), &stderr, Some(&run));
    let files = entries_under(&run);
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["marks.tsv", "meta.txt", "sdu.log"]);
    let marks = fs::read_to_string(run.join("marks.tsv")).unwrap();
    assert!(
        marks.contains("\tend\tcould not connect the L2CAP channel"),
        "{marks}"
    );
    let meta = fs::read_to_string(run.join("meta.txt")).unwrap();
    let keys: Vec<&str> = meta
        .lines()
        .map(|line| line.split('=').next().unwrap())
        .collect();
    assert_eq!(keys, ["script", "tool", "realtime_at_zero_ns"], "{meta}");
    // The reader of a run finds the wall clock under the key the capture wrote it with.
    let read = parse_meta(&meta).unwrap();
    assert!(read.realtime_at_zero_ns.is_some(), "{meta}");
    assert!(meta.starts_with("script=listen\n"), "{meta}");

    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut host = Host {
        home: home.path().to_str().unwrap(),
        connect: &connect,
        stop_service: &|| Ok(Stopped::nothing()),
        stdout: &mut stdout,
        stderr: &mut stderr,
    };
    let code = stop(&mut host, Route::Uplink, &AtomicBool::new(false));
    assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
    let stderr = String::from_utf8(stderr).unwrap();
    assert!(
        stderr.contains("could not connect the L2CAP channel"),
        "{stderr}"
    );
    assert_no_address(&String::from_utf8(stdout).unwrap(), &stderr, None);
}
