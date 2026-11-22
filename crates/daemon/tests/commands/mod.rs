//! The `status`, `watch`, `set` and `do` commands, run against the built daemon in a temp directory.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Child, ChildStdout, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};

use super::support::{BINARY, HANG_LIMIT, Session, run_to_end, wait_for};

/// A device on which every write fails with no space left.
const FULL_DEVICE: &str = "/dev/full";

/// A running `librepods watch`, killed once `HANG_LIMIT` passes so a read of its output ends, and killed and reaped when dropped.
struct Watch {
    child: Arc<Mutex<Child>>,
    /// Dropping it wakes the watchdog, which then ends without a kill.
    disarm: Option<mpsc::Sender<()>>,
    watchdog: Option<JoinHandle<()>>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        drop(self.disarm.take());
        if let Some(watchdog) = self.watchdog.take() {
            // A watchdog that panicked has nothing left to end.
            drop(watchdog.join());
        }
        let mut child = lock(&self.child);
        // A watch that already exited takes the kill as a no-op.
        drop(child.kill());
        // Only reaps it; a test that needs the exit status has read it already.
        drop(child.wait());
    }
}

/// The child, even after a panic while it was held, so a drop during a failed test still kills it.
fn lock(child: &Mutex<Child>) -> MutexGuard<'_, Child> {
    child.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts `librepods watch` with its stdout piped back here.
fn spawn_watch(session: &Session) -> (Watch, BufReader<ChildStdout>) {
    let mut child = session
        .command(Path::new(BINARY), &["watch"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let child = Arc::new(Mutex::new(child));
    let (disarm, disarmed) = mpsc::channel::<()>();
    let watched = Arc::clone(&child);
    let watchdog = thread::spawn(move || {
        if disarmed.recv_timeout(HANG_LIMIT) == Err(RecvTimeoutError::Timeout) {
            // std signals no child it has already reaped, so a late kill cannot reach a reused pid.
            drop(lock(&watched).kill());
        }
    });
    let watch = Watch {
        child,
        disarm: Some(disarm),
        watchdog: Some(watchdog),
    };
    (watch, stdout)
}

/// The next line `watch` printed, or empty once it exited or its watchdog killed it.
fn read_watch_line(stdout: &mut BufReader<ChildStdout>) -> String {
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    line
}

/// Waits for `watch` to exit and returns its code and stderr.
fn finish(watch: &Watch) -> (Option<i32>, String) {
    let status = wait_for("the client to exit", || {
        lock(&watch.child).try_wait().unwrap()
    });
    let mut stderr = String::new();
    lock(&watch.child)
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
    let (watch, mut stdout) = spawn_watch(&session);
    assert_eq!(read_watch_line(&mut stdout), session.status());
    let set = session.run_client(Path::new(BINARY), &["set", "ear_detection", "both"]);
    assert_eq!(set.status.code(), Some(0));
    assert_eq!(read_watch_line(&mut stdout), session.status());
    assert_eq!(session.send(b"ear:one"), b"");
    assert_eq!(read_watch_line(&mut stdout), session.status());
    assert!(daemon.stop().success());
    assert_eq!(read_watch_line(&mut stdout), "");
    assert_eq!(
        finish(&watch),
        (Some(1), "The daemon went away.\n".to_owned())
    );
}

#[test]
fn watch_ends_quietly_when_nothing_reads_its_output() {
    let session = Session::new();
    let _daemon = session.start();
    let (watch, mut stdout) = spawn_watch(&session);
    assert_eq!(read_watch_line(&mut stdout), session.status());
    drop(stdout);
    assert_eq!(session.send(b"ear:both"), b"");
    assert_eq!(finish(&watch), (Some(0), String::new()));
}

#[test]
fn status_fails_like_watch_when_its_line_cannot_be_written_and_ctl_does_not() {
    let session = Session::new();
    let _daemon = session.start();
    let link = session.dir.path().join("librepods-ctl");
    symlink(BINARY, &link).unwrap();
    let to_full = |program: &Path, args: &[&str]| {
        let full = File::options().write(true).open(FULL_DEVICE).unwrap();
        let mut command = session.command(program, args);
        run_to_end(command.stdout(full).stderr(Stdio::piped()))
    };
    let watch = to_full(Path::new(BINARY), &["watch"]);
    assert_eq!(watch.status.code(), Some(1));
    let message = String::from_utf8(watch.stderr).unwrap();
    assert!(
        message.starts_with("Cannot write the state to standard output: "),
        "{message}"
    );
    let status = to_full(Path::new(BINARY), &["status"]);
    assert_eq!(status.status.code(), Some(1));
    assert_eq!(String::from_utf8(status.stderr).unwrap(), message);
    // The legacy client ignores the failed write, as the C++ `librepods-ctl` does.
    for (program, args) in [
        (Path::new(BINARY), &["ctl", "status"][..]),
        (link.as_path(), &["status"]),
    ] {
        let out = to_full(program, args);
        assert_eq!(out.status.code(), Some(0), "{}", program.display());
        assert_eq!(out.stderr, b"", "{}", program.display());
    }
}

#[test]
fn status_and_watch_end_quietly_when_nothing_reads_their_output() {
    let session = Session::new();
    let _daemon = session.start();
    for args in [["status"], ["watch"]] {
        let (reader, writer) = io::pipe().unwrap();
        drop(reader);
        let mut command = session.command(Path::new(BINARY), &args);
        let out = run_to_end(command.stdout(writer).stderr(Stdio::piped()));
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert_eq!(out.stderr, b"", "{args:?}");
    }
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
        (
            &["do", "disconnect"],
            1,
            "There are no AirPods to act on.\n",
        ),
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
    assert!(daemon.stop().success(), "{}", daemon.log());
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
