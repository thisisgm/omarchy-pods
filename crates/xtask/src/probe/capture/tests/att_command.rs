//! The capture command on the dictation route, run with a temporary home and a connect that the test supplies.

use std::cell::Cell;
use std::fs;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use pods_proto::addr::BdAddr;
use pods_proto::dictation::{SESSION_ACTIVE, SESSION_IDLE};
use pods_proto::packets::{
    ATT_PSM, CONTROL_PSM, HANDSHAKE, REQUEST_NOTIFICATIONS, SET_SPECIFIC_FEATURES,
};
use pods_proto::sdulog::{self, Direction};
use pods_sys::l2cap::ChannelError;
use rustix::io::Errno;

use super::super::super::dictate::{HandleCheck, START, STOP};
use super::super::super::script::{Route, ScriptName};
use super::super::super::{CaptureArgs, Host, capture, stop, summary};
use super::super::{EndReason, Outcome};
use super::att::NOTIFICATION;
use super::att_pods::{DECLARED, TABLE_END};
use super::command::{DEVICE_OCTETS, Ran, STAMP, assert_no_address, entries_under, run_dialled};
use super::dictation::HANDSHAKE_REQUESTS;
use super::pods::{self, DEADLINE, Pods, Transcript};
use super::table::walk_requests;
use super::{DEVICE_LINE, ROOT_NAME, counted_nothing, home_with_device};

/// A minute of the dictation `soak`, which a test ends long before with a signal.
pub(super) fn soak_args(run: &str, no_marker: bool) -> CaptureArgs {
    CaptureArgs {
        stamp: STAMP.to_owned(),
        run: run.to_owned(),
        route: Route::Dictation,
        script: Some(ScriptName::Soak),
        steps: None,
        allow_control: false,
        minutes: Some(1),
        no_marker,
        unchecked: false,
    }
}

pub(super) fn interrupting_pods() -> Pods {
    let mut part = Pods::default();
    part.att.interrupt_after = Some(3);
    part
}

fn run_against_pods(
    home: &std::path::Path,
    part: Pods,
    command: impl FnOnce(&mut Host<'_>, &AtomicBool) -> ExitCode + Send + 'static,
) -> (Ran, Transcript) {
    let (near, far) = pods::pair();
    let (near_att, far_att) = pods::pair();
    let interrupted = Arc::new(AtomicBool::new(false));
    let transcript = pods::spawn_on(Some(far), Some(far_att), part, Arc::clone(&interrupted));
    let lines = vec![(CONTROL_PSM, near), (ATT_PSM, near_att)];
    let ran = run_dialled(home, lines, move |host| command(host, &interrupted));
    let transcript = transcript.recv_timeout(DEADLINE);
    let transcript = transcript.expect("the fake pods did not finish before the deadline");
    (ran, transcript)
}

/// Asserts what an interrupted dictation soak does whether or not it sends the markers: its dials, its writes, what it says and the files of `run`.
fn assert_soak_ran(ran: &Ran, pods: &Transcript, run: &std::path::Path) {
    // The control channel is asked for first, then the attribute channel on its own PSM.
    assert_eq!(ran.dialled, [CONTROL_PSM, ATT_PSM]);
    let mut att = walk_requests();
    att.extend([START.to_vec(), STOP.to_vec()]);
    assert_eq!(pods.att_got, att);
    assert_eq!(ran.code, format!("{:?}", ExitCode::FAILURE));
    assert_eq!(ran.stdout, "cue Stay as you are. Nothing to do.\n");
    let said = "xtask probe: capture ended early: interrupted by a signal; the dictation stop was sent (packets in ";
    assert!(ran.stderr.starts_with(said), "{}", ran.stderr);
    let counted = ", dictation start sent 1, dictation stop sent 1, requests refused 0)\n";
    assert!(ran.stderr.ends_with(counted), "{}", ran.stderr);
    let files = entries_under(run);
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    let expected = ["att.log", "handles.tsv", "marks.tsv", "meta.txt", "sdu.log"];
    assert_eq!(names, expected);
    let marks = fs::read_to_string(run.join("marks.tsv")).unwrap();
    assert!(marks.contains("\thandles\tpresent\n"), "{marks}");
    assert!(
        marks.ends_with("\tend\tinterrupted by a signal\n"),
        "{marks}"
    );
}

fn knock_args(run: &str) -> CaptureArgs {
    CaptureArgs {
        stamp: STAMP.to_owned(),
        run: run.to_owned(),
        route: Route::Dictation,
        script: Some(ScriptName::AttKnock),
        steps: None,
        allow_control: false,
        minutes: None,
        no_marker: false,
        unchecked: false,
    }
}

#[test]
fn the_att_knock_command_dials_the_attribute_psm_alone_and_records_a_run() {
    let home = home_with_device();
    let (near, far) = pods::pair();
    far.send(&NOTIFICATION).unwrap();
    let args = knock_args("att-knock-1");
    let ran = run_dialled(home.path(), vec![(ATT_PSM, near)], move |host| {
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
        "xtask probe: capture complete: packets in 0, with audio 0, not logged 0, START sent 0, STOP sent 0, attribute packets in 1, with audio 0, dictation start sent 0, dictation stop sent 0, requests refused 0\n"
    );
    assert_eq!(ran.stdout, "");
    // The control channel is never asked for, and the one connect names the attribute PSM.
    assert_eq!(ran.dialled, [ATT_PSM]);
    assert_eq!(ATT_PSM, 0x001f);
    assert_eq!(pods::queued(&far), Vec::<Vec<u8>>::new());
    let run = home.path().join(ROOT_NAME).join(STAMP).join("att-knock-1");
    let files = entries_under(&run);
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["att.log", "marks.tsv", "meta.txt", "sdu.log"]);
    let att_log = fs::read_to_string(run.join("att.log")).unwrap();
    let records: Vec<_> = att_log
        .lines()
        .map(|line| sdulog::parse_line(line).unwrap())
        .collect();
    assert_eq!(records.len(), 1, "{att_log}");
    assert_eq!(records[0].direction, Direction::FromPods);
    assert_eq!(records[0].sdu, NOTIFICATION);
    assert_eq!(fs::read_to_string(run.join("sdu.log")).unwrap(), "");
    let marks = fs::read_to_string(run.join("marks.tsv")).unwrap();
    let kinds: Vec<&str> = marks
        .lines()
        .map(|line| line.split('\t').nth(1).unwrap())
        .collect();
    let expected = ["att-connect", "att-connect", "gate", "end"];
    assert_eq!(kinds, expected, "{marks}");
    let meta = fs::read_to_string(run.join("meta.txt")).unwrap();
    let keys: Vec<&str> = meta
        .lines()
        .map(|line| line.split('=').next().unwrap())
        .collect();
    let expected = [
        "script",
        "tool",
        "realtime_at_zero_ns",
        "route",
        "no_marker",
        "unchecked",
    ];
    assert_eq!(keys, expected);
    assert!(meta.starts_with("script=att-knock\n"), "{meta}");
    let flags = "\nroute=dictation\nno_marker=false\nunchecked=false\n";
    assert!(meta.ends_with(flags), "{meta}");
    assert_no_address(&ran.stdout, &ran.stderr, Some(&run));
}

#[test]
fn output_never_contains_the_device_address() {
    let home = home_with_device();
    let args = knock_args("page");
    let dialled = Cell::new(None);
    let connect = |peer: BdAddr, psm: u16| {
        dialled.set(Some((peer.octets(), psm)));
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
        Some((DEVICE_OCTETS, ATT_PSM)),
        "the connect was not reached"
    );
    // One sentence: the step, the PSM and the kernel's name for the error.
    let failed = "the attribute channel on PSM 0x001f did not open: could not connect the L2CAP channel: Host is down";
    let stderr = String::from_utf8(stderr).unwrap();
    assert!(
        stderr.starts_with(&format!("xtask probe: capture ended early: {failed}")),
        "{stderr}"
    );
    assert!(
        stderr.ends_with("dictation stop sent 0, requests refused 0)\n"),
        "{stderr}"
    );
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    let run = home.path().join(ROOT_NAME).join(STAMP).join("page");
    assert_no_address(&String::from_utf8(stdout).unwrap(), &stderr, Some(&run));
    let marks = fs::read_to_string(run.join("marks.tsv")).unwrap();
    let lines: Vec<&str> = marks.lines().collect();
    assert_eq!(lines.len(), 2, "{marks}");
    assert!(lines[0].ends_with("\tatt-connect\tconnecting"), "{marks}");
    assert!(lines[1].contains(&format!("\tend\t{failed}")), "{marks}");
    assert_eq!(fs::read_to_string(run.join("att.log")).unwrap(), "");
    assert_eq!(fs::read_to_string(run.join("sdu.log")).unwrap(), "");
}

#[test]
fn the_summary_of_a_dictation_run_adds_the_attribute_counts() {
    // Every count differs from the others, so one printed under another's name is seen.
    let outcome = Outcome {
        sdus_in: 9,
        audio_sdus: 4,
        starts: 3,
        stops: 5,
        uplink_running: false,
        truncated: 7,
        att_pdus_in: 11,
        audio_notifications: 8,
        dictation_starts: 2,
        dictation_stops: 6,
        att_requests_refused: 12,
        end: EndReason::ScriptDone,
    };
    let control = "packets in 9, with audio 4, not logged 7, START sent 3, STOP sent 5";
    let att = "attribute packets in 11, with audio 8, dictation start sent 2, dictation stop sent 6, requests refused 12";
    for route in [Route::Dictation, Route::Both] {
        let sentence = summary(&outcome, route);
        assert_eq!(sentence, format!("capture complete: {control}, {att}"));
    }
    let sentence = summary(&outcome, Route::Uplink);
    assert_eq!(sentence, format!("capture complete: {control}"));
    let closed = Outcome {
        end: EndReason::AttClosed,
        ..outcome
    };
    assert_eq!(
        summary(&closed, Route::Both),
        format!(
            "capture ended early: the pods closed the attribute channel; STOP was sent; the dictation stop was sent ({control}, {att})"
        )
    );
}

#[test]
fn the_summary_says_whether_the_dictation_stop_followed_its_start() {
    let outcome = |dictation_starts, dictation_stops, end| Outcome {
        dictation_starts,
        dictation_stops,
        ..counted_nothing(end)
    };
    let summary = |outcome: &Outcome| summary(outcome, Route::Dictation);
    let stopped = summary(&outcome(1, 1, EndReason::Interrupted));
    assert!(
        stopped.starts_with(
            "capture ended early: interrupted by a signal; the dictation stop was sent (packets in 0"
        ),
        "{stopped}"
    );
    let running = summary(&outcome(2, 1, EndReason::AttClosed));
    let rescue =
        "; the dictation stop was NOT sent, so run cargo xtask probe stop --route dictation (";
    assert!(running.contains(rescue), "{running}");
    // No uplink was started, so nothing is said of its STOP.
    assert!(!running.contains("STOP was"), "{running}");
    let unchecked = EndReason::HandleCheck(HandleCheck::Missing(vec![0x000a, 0x0010]));
    let refused = summary(&outcome(0, 0, unchecked));
    assert!(
        refused.starts_with(
            "capture ended early: the pods' listing of the handles the stream uses read missing 0x000a 0x0010, so the start write was not sent (packets in 0"
        ),
        "{refused}"
    );
    assert!(!refused.contains("dictation stop was"), "{refused}");
    let silent = summary(&outcome(
        0,
        0,
        EndReason::HandleCheck(HandleCheck::NoListing),
    ));
    assert!(
        silent.starts_with(
            "capture ended early: the pods listed no attribute, so the start write was not sent ("
        ),
        "{silent}"
    );
}

#[test]
fn an_interrupted_dictation_soak_dials_both_channels_and_says_the_stop_was_sent() {
    let home = home_with_device();
    let args = soak_args("soak-1", false);
    let (ran, pods) = run_against_pods(
        home.path(),
        interrupting_pods(),
        move |host, interrupted| capture(&args, host, interrupted),
    );

    let control = [
        &HANDSHAKE[..],
        &SET_SPECIFIC_FEATURES,
        &REQUEST_NOTIFICATIONS,
        &SESSION_ACTIVE,
        &SESSION_IDLE,
    ];
    assert_eq!(pods.got(), control);
    let run = home.path().join(ROOT_NAME).join(STAMP).join("soak-1");
    assert_soak_ran(&ran, &pods, &run);
    let handles = fs::read_to_string(run.join("handles.tsv")).unwrap();
    assert!(handles.contains("attribute\t0x0010\t0x2b2a\n"), "{handles}");
    // A run that sends the markers marks both, which is what the tests that look for neither rest on.
    let marks = fs::read_to_string(run.join("marks.tsv")).unwrap();
    assert!(marks.contains("\tactive\tsession active\n"), "{marks}");
    assert!(marks.contains("\tidle\tsession idle\n"), "{marks}");
    let meta = fs::read_to_string(run.join("meta.txt")).unwrap();
    let flags = "\nroute=dictation\nno_marker=false\nunchecked=false\n";
    assert!(meta.ends_with(flags), "{meta}");
    assert_no_address(&ran.stdout, &ran.stderr, Some(&run));
}

#[test]
fn no_marker_keeps_both_markers_off_the_control_channel_and_changes_nothing_else() {
    let home = home_with_device();
    let args = soak_args("soak-2", true);
    let (ran, pods) = run_against_pods(
        home.path(),
        interrupting_pods(),
        move |host, interrupted| capture(&args, host, interrupted),
    );

    let handshake = [
        &HANDSHAKE[..],
        &SET_SPECIFIC_FEATURES,
        &REQUEST_NOTIFICATIONS,
    ];
    assert_eq!(pods.got(), handshake);
    let run = home.path().join(ROOT_NAME).join(STAMP).join("soak-2");
    assert_soak_ran(&ran, &pods, &run);
    let meta = fs::read_to_string(run.join("meta.txt")).unwrap();
    let flags = "\nroute=dictation\nno_marker=true\nunchecked=false\n";
    assert!(meta.ends_with(flags), "{meta}");
    let marks = fs::read_to_string(run.join("marks.tsv")).unwrap();
    assert!(
        !marks.contains("\tactive\t") && !marks.contains("\tidle\t"),
        "{marks}"
    );
}

#[test]
fn the_stop_command_of_the_dictation_route_sends_the_stop_and_the_idle_marker() {
    let home = home_with_device();
    let (ran, pods) = run_against_pods(home.path(), Pods::default(), |host, _interrupted| {
        stop(host, Route::Dictation)
    });

    assert_eq!(ran.dialled, [CONTROL_PSM, ATT_PSM]);
    assert_eq!(pods.att_got, [STOP]);
    let control = [
        &HANDSHAKE[..],
        &SET_SPECIFIC_FEATURES,
        &REQUEST_NOTIFICATIONS,
        &SESSION_IDLE,
    ];
    assert_eq!(pods.got(), control);
    assert_eq!(
        ran.code,
        format!("{:?}", ExitCode::SUCCESS),
        "{}",
        ran.stderr
    );
    assert_eq!(
        ran.stderr,
        "xtask probe: the dictation stop and the idle session marker were sent and the channels were kept open for 1000 ms\n"
    );
    assert_eq!(ran.stdout, "");
    let kept = entries_under(&home.path().join(ROOT_NAME));
    let device = ("device".to_owned(), Some(DEVICE_LINE.to_owned()));
    assert_eq!(kept, [device]);
    assert_no_address(&ran.stdout, &ran.stderr, None);
}

#[test]
fn the_stop_command_says_what_kept_it_from_stopping_and_takes_one_route() {
    let home = home_with_device();
    let (near, far) = pods::pair();
    let transcript = pods::spawn(far, Pods::default(), Arc::default());
    // The attribute channel is refused once the handshake is done, so the stop ends there with no marker sent.
    let lines = vec![(CONTROL_PSM, near)];
    let ran = run_dialled(home.path(), lines, |host| stop(host, Route::Dictation));
    assert_eq!(ran.dialled, [CONTROL_PSM, ATT_PSM]);
    assert_eq!(ran.code, format!("{:?}", ExitCode::FAILURE));
    assert_eq!(
        ran.stderr,
        "xtask probe: the stop ended early: the attribute channel on PSM 0x001f did not open: could not connect the L2CAP channel: Connection refused (os error 111) (dictation stop sent 0)\n"
    );
    let transcript = transcript.recv_timeout(DEADLINE).unwrap();
    let handshake = [
        &HANDSHAKE[..],
        &SET_SPECIFIC_FEATURES,
        &REQUEST_NOTIFICATIONS,
    ];
    assert_eq!(transcript.got(), handshake);

    let ran = run_dialled(home.path(), Vec::new(), |host| stop(host, Route::Both));
    assert_eq!(ran.dialled, Vec::<u16>::new());
    assert_eq!(ran.code, format!("{:?}", ExitCode::FAILURE));
    assert_eq!(
        ran.stderr,
        "xtask probe: stop takes one route at a time: run it with --route uplink, then with --route dictation\n"
    );
}

#[test]
fn a_dictation_run_whose_attribute_channel_is_refused_sends_no_marker_and_no_write() {
    let home = home_with_device();
    let (near, far) = pods::pair();
    let transcript = pods::spawn(far, Pods::default(), Arc::default());
    let args = soak_args("refused", false);
    let lines = vec![(CONTROL_PSM, near)];
    let ran = run_dialled(home.path(), lines, move |host| {
        capture(&args, host, &AtomicBool::new(false))
    });

    assert_eq!(ran.dialled, [CONTROL_PSM, ATT_PSM]);
    // The handshake was done by then, and nothing followed it on the control channel.
    let transcript = transcript.recv_timeout(DEADLINE).unwrap();
    let handshake = [
        &HANDSHAKE[..],
        &SET_SPECIFIC_FEATURES,
        &REQUEST_NOTIFICATIONS,
    ];
    assert_eq!(transcript.got(), handshake);
    assert_eq!(ran.code, format!("{:?}", ExitCode::FAILURE));
    let failed = "the attribute channel on PSM 0x001f did not open: could not connect the L2CAP channel: Connection refused";
    assert!(
        ran.stderr
            .starts_with(&format!("xtask probe: capture ended early: {failed}")),
        "{}",
        ran.stderr
    );
    assert_eq!(ran.stderr.lines().count(), 1, "{}", ran.stderr);
    assert_eq!(ran.stdout, "");
    let run = home.path().join(ROOT_NAME).join(STAMP).join("refused");
    let marks = fs::read_to_string(run.join("marks.tsv")).unwrap();
    let last = marks.lines().last().unwrap();
    assert!(last.contains(&format!("\tend\t{failed}")), "{marks}");
    assert!(!marks.contains("\tactive\t"), "{marks}");
    assert_no_address(&ran.stdout, &ran.stderr, Some(&run));
}

#[test]
fn the_att_map_command_dials_the_attribute_psm_alone_and_writes_the_table() {
    let home = home_with_device();
    let (near, far) = pods::pair();
    let never = Arc::new(AtomicBool::new(false));
    let transcript = pods::spawn_on(None, Some(far), Pods::default(), never);
    let args = CaptureArgs {
        script: Some(ScriptName::AttMap),
        steps: None,
        allow_control: false,
        ..knock_args("map-1")
    };
    let ran = run_dialled(home.path(), vec![(ATT_PSM, near)], move |host| {
        capture(&args, host, &AtomicBool::new(false))
    });
    let pods = transcript.recv_timeout(DEADLINE);
    let pods = pods.expect("the fake pods did not finish before the deadline");

    assert_eq!(ran.dialled, [ATT_PSM]);
    assert_eq!(
        ran.code,
        format!("{:?}", ExitCode::SUCCESS),
        "{}",
        ran.stderr
    );
    // The read of the declarations is spelled out here, so a change to the request the probe builds is seen.
    let mut att = walk_requests();
    att.push(vec![0x08, 0x01, 0x00, 0xff, 0xff, 0x03, 0x28]);
    assert_eq!(pods.att_got, att);
    assert_eq!(pods.got(), Vec::<&[u8]>::new());
    let run = home.path().join(ROOT_NAME).join(STAMP).join("map-1");
    let handles = fs::read_to_string(run.join("handles.tsv")).unwrap();
    let rows = usize::from(TABLE_END) + DECLARED.len();
    assert_eq!(handles.lines().count(), rows, "{handles}");
    assert!(
        handles.ends_with("declaration\t0x000f\t0x10\t0x0010\t0x2b2a\n"),
        "{handles}"
    );
    let marks = fs::read_to_string(run.join("marks.tsv")).unwrap();
    assert!(marks.contains("\thandles\tpresent\n"), "{marks}");
    assert_no_address(&ran.stdout, &ran.stderr, Some(&run));
}

#[test]
fn a_capture_of_both_routes_dials_both_channels_and_stops_what_it_started() {
    let home = home_with_device();
    let args = CaptureArgs {
        route: Route::Both,
        script: Some(ScriptName::Both),
        steps: None,
        allow_control: false,
        minutes: None,
        ..soak_args("both-1", false)
    };
    // The signal comes while only the dictation stream runs, five seconds before the script starts the uplink.
    let (ran, pods) = run_against_pods(
        home.path(),
        interrupting_pods(),
        move |host, interrupted| capture(&args, host, interrupted),
    );

    assert_eq!(ran.dialled, [CONTROL_PSM, ATT_PSM]);
    let mut att = walk_requests();
    att.extend([START.to_vec(), STOP.to_vec()]);
    assert_eq!(pods.att_got, att);
    assert_eq!(pods.count(&SESSION_ACTIVE), 1);
    assert_eq!(pods.count(&SESSION_IDLE), 1);
    let said = "xtask probe: capture ended early: interrupted by a signal; the dictation stop was sent (packets in ";
    assert!(ran.stderr.starts_with(said), "{}", ran.stderr);
    assert!(
        ran.stderr.contains(", START sent 0, STOP sent 0, "),
        "{}",
        ran.stderr
    );
    let run = home.path().join(ROOT_NAME).join(STAMP).join("both-1");
    let meta = fs::read_to_string(run.join("meta.txt")).unwrap();
    assert!(meta.starts_with("script=both\n"), "{meta}");
    assert!(meta.contains("\nroute=both\n"), "{meta}");
}

#[test]
fn unchecked_on_the_command_line_lets_the_start_follow_a_table_the_pods_did_not_list() {
    let unlisted = || {
        let mut part = interrupting_pods();
        // The pods answer the first request of the walk with the error that ends a table.
        part.att.table = Some(Vec::new());
        part
    };
    // A table the pods do not list gets the first request of the walk and no other.
    let first = walk_requests().remove(0);
    let home = home_with_device();
    let args = soak_args("checked", false);
    let (ran, pods) = run_against_pods(home.path(), unlisted(), move |host, interrupted| {
        capture(&args, host, interrupted)
    });
    assert_eq!(pods.att_got, [&first[..]]);
    assert_eq!(pods.got(), HANDSHAKE_REQUESTS);
    assert_eq!(ran.code, format!("{:?}", ExitCode::FAILURE));
    let said = "xtask probe: capture ended early: the pods listed no attribute, so the start write was not sent (packets in ";
    assert!(ran.stderr.starts_with(said), "{}", ran.stderr);
    assert_eq!(ran.stdout, "");

    let args = CaptureArgs {
        unchecked: true,
        ..soak_args("unchecked", false)
    };
    let (ran, pods) = run_against_pods(home.path(), unlisted(), move |host, interrupted| {
        capture(&args, host, interrupted)
    });
    let sent = [&first[..], &START, &STOP];
    assert_eq!(pods.att_got, sent);
    let said = "xtask probe: capture ended early: interrupted by a signal; the dictation stop was sent (packets in ";
    assert!(ran.stderr.starts_with(said), "{}", ran.stderr);
    let run = home.path().join(ROOT_NAME).join(STAMP).join("unchecked");
    let marks = fs::read_to_string(run.join("marks.tsv")).unwrap();
    assert!(marks.contains("\thandles\tunchecked\n"), "{marks}");
    let meta = fs::read_to_string(run.join("meta.txt")).unwrap();
    let flags = "\nroute=dictation\nno_marker=false\nunchecked=true\n";
    assert!(meta.ends_with(flags), "{meta}");
}
