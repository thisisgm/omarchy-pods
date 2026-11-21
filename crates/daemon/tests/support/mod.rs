//! The temp session, the daemon process and the waits that the tests against the built binary share.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::Pid;
use tempfile::TempDir;

pub(crate) const BINARY: &str = env!("CARGO_BIN_EXE_librepods");

/// A daemon or client still running after this long is hung, and the test kills it and fails.
pub(crate) const HANG_LIMIT: Duration = Duration::from_secs(10);

/// How often a wait checks its condition again.
pub(crate) const POLL: Duration = Duration::from_millis(10);

#[expect(
    clippy::disallowed_methods,
    reason = "std has no timed wait for a child process or a file, and this is the test harness, not the core"
)]
pub(crate) fn pause(duration: Duration) {
    thread::sleep(duration);
}

/// Waits for `ready` to give a value, failing the test once `HANG_LIMIT` passes.
pub(crate) fn wait_for<T>(what: &str, mut ready: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(value) = ready() {
            return value;
        }
        assert!(
            started.elapsed() < HANG_LIMIT,
            "{what} not seen after {HANG_LIMIT:?}"
        );
        pause(POLL);
    }
}

/// The temp session one test runs in: runtime directory, state home and home.
pub(crate) struct Session {
    pub(crate) dir: TempDir,
}

impl Session {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["run", "state", "home"] {
            fs::create_dir(dir.path().join(sub)).unwrap();
        }
        fs::set_permissions(dir.path().join("run"), fs::Permissions::from_mode(0o700)).unwrap();
        Self { dir }
    }

    pub(crate) fn runtime_dir(&self) -> PathBuf {
        self.dir.path().join("run")
    }

    pub(crate) fn state_home(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    pub(crate) fn socket(&self) -> PathBuf {
        self.runtime_dir().join("librepods.sock")
    }

    /// The binary with this session's environment and nothing of the real one's XDG or home.
    pub(crate) fn command(&self, program: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(program);
        command
            .args(args)
            .env("XDG_RUNTIME_DIR", self.runtime_dir())
            .env("XDG_STATE_HOME", self.state_home())
            .env("HOME", self.dir.path().join("home"));
        command
    }

    /// Starts a daemon with `args`, its stderr going to a log file in the session.
    pub(crate) fn spawn(&self, args: &[&str], log_name: &str) -> Daemon {
        let log = self.dir.path().join(log_name);
        let child = self
            .command(Path::new(BINARY), args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(&log).unwrap())
            .spawn()
            .unwrap();
        Daemon { child, log }
    }

    /// Starts a daemon and waits until its socket accepts, which the daemon binds only after its first state file write.
    pub(crate) fn start(&self) -> Daemon {
        let daemon = self.spawn(&["daemon"], "daemon.log");
        wait_for("the daemon socket", || {
            UnixStream::connect(self.socket()).ok()
        });
        daemon
    }

    /// Sends `verb` without closing the write side, as `librepods-ctl` does, and reads until the daemon closes.
    pub(crate) fn ask(&self, verb: &[u8]) -> Vec<u8> {
        self.try_ask(verb).unwrap()
    }

    /// `ask`, giving back the error of a connection the daemon closed at accept.
    pub(crate) fn try_ask(&self, verb: &[u8]) -> std::io::Result<Vec<u8>> {
        let mut stream = UnixStream::connect(self.socket())?;
        stream.write_all(verb)?;
        stream.set_read_timeout(Some(HANG_LIMIT))?;
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply)?;
        Ok(reply)
    }

    /// Sends `verb`, closes the write side, and reads until the daemon closes.
    pub(crate) fn send(&self, verb: &[u8]) -> Vec<u8> {
        let mut stream = UnixStream::connect(self.socket()).unwrap();
        stream.write_all(verb).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        read_to_close(&mut stream)
    }

    pub(crate) fn status(&self) -> String {
        String::from_utf8(self.ask(b"status")).unwrap()
    }

    /// Runs the client to completion, or kills it and fails once it passes `HANG_LIMIT`.
    pub(crate) fn run_client(&self, program: &Path, args: &[&str]) -> Output {
        let mut child = self
            .command(program, args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        wait_for("the client to exit", || child.try_wait().unwrap());
        child.wait_with_output().unwrap()
    }
}

/// Reads until the peer closes, failing the test once `HANG_LIMIT` passes.
pub(crate) fn read_to_close(stream: &mut UnixStream) -> Vec<u8> {
    stream.set_read_timeout(Some(HANG_LIMIT)).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    reply
}

/// A daemon process, killed when the test ends if it is still running.
pub(crate) struct Daemon {
    pub(crate) child: Child,
    pub(crate) log: PathBuf,
}

impl Daemon {
    pub(crate) fn pid(&self) -> Pid {
        Pid::from_child(&self.child)
    }

    pub(crate) fn wait_exit(&mut self) -> ExitStatus {
        wait_for("the daemon to exit", || self.child.try_wait().unwrap())
    }

    pub(crate) fn log(&self) -> String {
        fs::read_to_string(&self.log).unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // A daemon that already exited refuses the kill, which is the outcome wanted.
        drop(self.child.kill());
        drop(self.child.wait());
    }
}
