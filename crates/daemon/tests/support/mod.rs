//! The temp session, the daemon process and the waits that the tests against the built binary share.

pub(crate) mod bluez;
pub(crate) mod bus;

use std::fs::{self, File};
use std::io::{self, ErrorKind, Read, Write};
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
const POLL: Duration = Duration::from_millis(10);

/// Bytes taken from a socket per read; a longer reply takes several.
const READ_CHUNK: usize = 4096;

#[expect(
    clippy::disallowed_methods,
    reason = "std has no timed wait for a child process or a file, and this is the test harness, not the core"
)]
pub(crate) fn pause(duration: Duration) {
    thread::sleep(duration);
}

/// Waits for `ready` to give a value, failing the test once `HANG_LIMIT` passes.
pub(crate) fn wait_for<T>(what: &str, ready: impl FnMut() -> Option<T>) -> T {
    wait_within(what, HANG_LIMIT, ready)
}

/// Waits for `ready` to give a value, failing the test once `limit` passes.
pub(crate) fn wait_within<T>(
    what: &str,
    limit: Duration,
    mut ready: impl FnMut() -> Option<T>,
) -> T {
    let started = Instant::now();
    loop {
        if let Some(value) = ready() {
            return value;
        }
        assert!(started.elapsed() < limit, "{what} not seen after {limit:?}");
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

    /// The status document the plugin reads.
    pub(crate) fn state_file(&self) -> PathBuf {
        self.state_home().join("librepods").join("status.json")
    }

    /// The binary with this session's environment and nothing of the real one's XDG or home.
    pub(crate) fn command(&self, program: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(program);
        command
            .args(args)
            .env("XDG_RUNTIME_DIR", self.runtime_dir())
            .env("XDG_STATE_HOME", self.state_home())
            .env("HOME", self.dir.path().join("home"))
            // With no address the daemon tries the bus under the temp runtime directory, which never exists.
            .env_remove("DBUS_SESSION_BUS_ADDRESS");
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
    pub(crate) fn try_ask(&self, verb: &[u8]) -> io::Result<Vec<u8>> {
        let mut stream = UnixStream::connect(self.socket())?;
        stream.write_all(verb)?;
        read_until_close(&mut stream)
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

    /// Runs the client to completion with its output piped back, or kills it and fails once it passes `HANG_LIMIT`.
    pub(crate) fn run_client(&self, program: &Path, args: &[&str]) -> Output {
        let mut command = self.command(program, args);
        run_to_end(command.stdout(Stdio::piped()).stderr(Stdio::piped()))
    }
}

/// Runs `command` to completion, or kills it and fails once it passes `HANG_LIMIT`; only output set to piped comes back.
pub(crate) fn run_to_end(command: &mut Command) -> Output {
    let mut child = command.spawn().unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() >= HANG_LIMIT {
            // Dropping a Child leaves it running; a client that exited meanwhile refuses the kill, which is fine.
            drop(child.kill());
            drop(child.wait());
            panic!("{command:?} still running after {HANG_LIMIT:?}");
        }
        pause(POLL);
    }
    child.wait_with_output().unwrap()
}

/// Reads until the peer closes, failing the test once `HANG_LIMIT` passes.
pub(crate) fn read_to_close(stream: &mut UnixStream) -> Vec<u8> {
    read_until_close(stream).unwrap()
}

/// Reads until the peer closes, or gives a timeout once `HANG_LIMIT` passes in all, however often the peer sends.
fn read_until_close(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let started = Instant::now();
    let mut reply = Vec::new();
    let mut chunk = [0; READ_CHUNK];
    loop {
        let left = HANG_LIMIT.saturating_sub(started.elapsed());
        if left.is_zero() {
            return Err(io::Error::new(
                ErrorKind::TimedOut,
                format!("the peer kept the connection open past {HANG_LIMIT:?}"),
            ));
        }
        stream.set_read_timeout(Some(left))?;
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(reply),
            Ok(read) => reply.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
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
