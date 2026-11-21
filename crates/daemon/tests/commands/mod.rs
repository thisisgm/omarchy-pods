//! The `status`, `watch`, `set` and `do` commands, run against the built daemon in a temp directory.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, ChildStdout, Stdio};

use rustix::process::{Signal, kill_process};

use super::support::{BINARY, Session, wait_for};

/// Starts `librepods watch` with its stdout piped back here.
fn spawn_watch(session: &Session) -> (Child, BufReader<ChildStdout>) {
    let mut child = session
        .command(Path::new(BINARY), &["watch"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    (child, stdout)
}

fn read_watch_line(stdout: &mut BufReader<ChildStdout>) -> String {
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    line
}

/// Waits for `child` to exit and returns its code and stderr.
fn finish(child: &mut Child) -> (Option<i32>, String) {
    let status = wait_for("the client to exit", || child.try_wait().unwrap());
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    (status.code(), stderr)
}

#[test]
fn watch_prints_a_line_per_change_and_exits_one_when_daemon_stops() {
    let session = Session::new();
    let mut daemon = session.start();
    let (mut watch, mut stdout) = spawn_watch(&session);
    assert_eq!(read_watch_line(&mut stdout), session.status());
    let set = session.run_client(Path::new(BINARY), &["set", "ear_detection", "both"]);
    assert_eq!(set.status.code(), Some(0));
    assert_eq!(read_watch_line(&mut stdout), session.status());
    assert_eq!(session.send(b"ear:one"), b"");
    assert_eq!(read_watch_line(&mut stdout), session.status());
    kill_process(daemon.pid(), Signal::TERM).unwrap();
    assert!(daemon.wait_exit().success());
    assert_eq!(read_watch_line(&mut stdout), "");
    assert_eq!(
        finish(&mut watch),
        (Some(1), "The daemon went away.\n".to_owned())
    );
}

#[test]
fn watch_ends_quietly_when_nothing_reads_its_output() {
    let session = Session::new();
    let _daemon = session.start();
    let (mut watch, mut stdout) = spawn_watch(&session);
    assert_eq!(read_watch_line(&mut stdout), session.status());
    drop(stdout);
    assert_eq!(session.send(b"ear:both"), b"");
    assert_eq!(finish(&mut watch), (Some(0), String::new()));
}

#[test]
fn set_exit_codes_and_messages() {
    let session = Session::new();
    let mut daemon = session.start();
    let binary = Path::new(BINARY);
    let status = session.run_client(binary, &["status"]);
    assert_eq!(status.status.code(), Some(0));
    assert_eq!(String::from_utf8(status.stdout).unwrap(), session.status());
    assert_eq!(status.stderr, b"");
    for (args, code, stderr) in [
        (&["set", "ear_detection", "off"][..], 0, ""),
        (
            &["set", "noise", "anc"],
            1,
            "The AirPods are not connected.\n",
        ),
        (
            &["set", "volume", "3"],
            1,
            "There is no setting by that name.\n",
        ),
        (
            &["set", "adaptive_level", "101"],
            1,
            "The value is outside the range the setting takes.\n",
        ),
        (
            &["set", "noise", "loud"],
            1,
            "The daemon could not read the request.\n",
        ),
        (&["do", "disconnect"], 1, "The AirPods are not connected.\n"),
        (
            &["do", "reopen"],
            1,
            "The daemon could not read the request.\n",
        ),
    ] {
        let out = session.run_client(binary, args);
        assert_eq!(out.status.code(), Some(code), "{args:?}");
        assert_eq!(out.stdout, b"", "{args:?}");
        assert_eq!(String::from_utf8(out.stderr).unwrap(), stderr, "{args:?}");
    }
    kill_process(daemon.pid(), Signal::TERM).unwrap();
    daemon.wait_exit();
    let refused = format!(
        "Could not connect to OpenPods daemon at {} (is it running?)\n",
        session.socket().display()
    );
    for args in [
        &["status"][..],
        &["watch"],
        &["set", "ear_detection", "off"],
        &["do", "connect"],
    ] {
        let out = session.run_client(binary, args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert_eq!(out.stdout, b"", "{args:?}");
        assert_eq!(String::from_utf8(out.stderr).unwrap(), refused, "{args:?}");
    }
}
