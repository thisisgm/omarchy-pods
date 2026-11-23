//! The temp session, the daemon process and the waits that the tests against the built binary share.

pub(crate) mod advert;
pub(crate) mod audio;
pub(crate) mod bluez;
pub(crate) mod bus;
pub(crate) mod children;
pub(crate) mod logind;
pub(crate) mod omarchy;
pub(crate) mod parked;
pub(crate) mod pipewire;
pub(crate) mod players;
pub(crate) mod relay;
pub(crate) mod rig;
pub(crate) mod settings;
pub(crate) mod sleep;
pub(crate) mod uplink;
pub(crate) mod watch;

use std::env::{self, VarError};
use std::fs::{self, DirBuilder, File};
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};
use serde_json::Value;
use tempfile::TempDir;

/// Names a release file for the suite to run in place of this build's binary.
pub(crate) const BINARY_VARIABLE: &str = "LIBREPODS_E2E_BINARY";

/// The daemon under test: the file `LIBREPODS_E2E_BINARY` names when it is set, else this build's binary.
pub(crate) fn binary() -> &'static str {
    static BINARY: OnceLock<String> = OnceLock::new();
    BINARY.get_or_init(|| match env::var(BINARY_VARIABLE) {
        Err(VarError::NotPresent) => env!("CARGO_BIN_EXE_librepods").to_owned(),
        // Made absolute here, since a spawn looks a bare name up in `PATH` and a link resolves from its own directory.
        Ok(path) => match fs::canonicalize(&path) {
            Ok(file) if file.is_file() => {
                file.into_os_string().into_string().unwrap_or_else(|file| {
                    panic!(
                        "{BINARY_VARIABLE} names {}, which is not a UTF-8 path",
                        file.display()
                    )
                })
            }
            Ok(_) => panic!("{BINARY_VARIABLE} names {path}, which is no file"),
            Err(error) => panic!("{BINARY_VARIABLE} names {path}, which is no file: {error}"),
        },
        Err(VarError::NotUnicode(path)) => panic!(
            "{BINARY_VARIABLE} names {}, which is not a UTF-8 path",
            path.display()
        ),
    })
}

const SHELL: &str = "/bin/sh";

/// Clears the mask the test runner passes on, then becomes the daemon named by `$0`.
const UNMASKED: &str = "umask 0; exec \"$0\" daemon";

/// The mode of a file the daemon keeps owner only.
pub(crate) const FILE_MODE: u32 = 0o600;

/// The mode of the directory holding the daemon's owner only files.
pub(crate) const DIR_MODE: u32 = 0o700;

/// The log of a daemon that `Session::start` or the rig starts, in the session directory.
pub(crate) const DAEMON_LOG: &str = "daemon.log";

/// A daemon or client still running after this long is hung, and the test kills it and fails.
pub(crate) const HANG_LIMIT: Duration = Duration::from_secs(10);

/// The bus a daemon outside the rig is pointed at, which never runs, so no test reaches the real system bus.
const NO_SYSTEM_BUS: &str = "no-system-bus";

/// How the daemon's line about that missing bus starts, from `watch` in `src/daemon/bluez.rs`.
const NO_SYSTEM_BUS_LOG: &str = "bluez: cannot reach the system bus:";

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

/// The lines of a daemon's log but the one about the system bus that never runs.
pub(crate) fn log_lines(log: &str) -> Vec<&str> {
    log.lines()
        .filter(|line| !line.starts_with(NO_SYSTEM_BUS_LOG))
        .collect()
}

/// Names an absolute directory where `Session::keep_state` copies a daemon's state for a later look; unset, nothing is kept.
pub(crate) const KEEP_STATE: &str = "LIBREPODS_E2E_KEEP_STATE";

/// The end of the sentence a test fails with when `KEEP_STATE` names anything else.
pub(crate) const KEEP_STATE_REFUSED: &str = "which is not an absolute directory that exists";

/// The end of the sentence a test fails with when its label directory exists, so two runs never mix in one directory.
pub(crate) const KEEP_STATE_TAKEN: &str =
    "already exists, so point it at a new directory for each run";

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

/// The temp session one test runs in: runtime directory, state home, config home, home and the directory on its path.
pub(crate) struct Session {
    pub(crate) dir: TempDir,
}

impl Session {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["run", "state", "config", "home", "bin"] {
            fs::create_dir(dir.path().join(sub)).unwrap();
        }
        fs::set_permissions(dir.path().join("run"), fs::Permissions::from_mode(0o700)).unwrap();
        Self { dir }
    }

    pub(crate) fn runtime_dir(&self) -> PathBuf {
        self.dir.path().join("run")
    }

    /// The only directory on the path of every program this session runs.
    pub(crate) fn bin(&self) -> PathBuf {
        self.dir.path().join("bin")
    }

    pub(crate) fn state_home(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    pub(crate) fn config_home(&self) -> PathBuf {
        self.dir.path().join("config")
    }

    pub(crate) fn socket(&self) -> PathBuf {
        self.runtime_dir().join("librepods.sock")
    }

    /// The status document the plugin reads.
    pub(crate) fn state_file(&self) -> PathBuf {
        self.state_home().join("librepods").join("status.json")
    }

    /// The preferences the daemon keeps across restarts, beside the status document.
    pub(crate) fn settings_file(&self) -> PathBuf {
        self.state_home().join("librepods").join("settings.json")
    }

    /// The proximity keys the daemon keeps, beside the status document.
    pub(crate) fn keys_file(&self) -> PathBuf {
        self.state_home().join("librepods").join("keys.json")
    }

    /// Copies the daemon's state directory to `<KEEP_STATE>/<label>/librepods/` when `KEEP_STATE` is set.
    pub(crate) fn keep_state(&self, label: &str) {
        let alphabet =
            |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-';
        assert!(
            !label.is_empty() && label.bytes().all(alphabet),
            "keep_state: the label {label:?} holds more than lower case letters, digits and -"
        );
        let Some(root) = env::var_os(KEEP_STATE).map(PathBuf::from) else {
            return;
        };
        assert!(
            root.is_absolute() && root.is_dir(),
            "{KEEP_STATE} names {root:?}, {KEEP_STATE_REFUSED}"
        );
        let labelled = root.join(label);
        match fs::create_dir(&labelled) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                panic!(
                    "keep_state: {KEEP_STATE} names {root:?}, and {labelled:?} {KEEP_STATE_TAKEN}"
                )
            }
            Err(error) => panic!("keep_state: creating {labelled:?}: {error}"),
        }
        let kept = labelled.join("librepods");
        fs::create_dir(&kept).unwrap();
        for entry in fs::read_dir(self.state_home().join("librepods")).unwrap() {
            let entry = entry.unwrap();
            match fs::copy(entry.path(), kept.join(entry.file_name())) {
                Ok(_) => {}
                // A temporary file the daemon renamed between the listing and the copy.
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => panic!("keep_state: copying {:?}: {error}", entry.path()),
            }
        }
    }

    /// Stores the sample keys as the daemon writes them: `{"identity":"<32 hex digits>","payload":"<32 hex digits>"}` and a newline.
    pub(crate) fn store_sample_keys(&self) {
        let hex = |key: [u8; 16]| key.map(|byte| format!("{byte:02x}")).concat();
        let text = format!(
            "{{\"identity\":\"{}\",\"payload\":\"{}\"}}\n",
            hex(pods_samples::IDENTITY_KEY),
            hex(pods_samples::PAYLOAD_KEY)
        );
        let path = self.keys_file();
        DirBuilder::new()
            .recursive(true)
            .mode(DIR_MODE)
            .create(path.parent().unwrap())
            .unwrap();
        fs::write(path, text).unwrap();
    }

    /// The binary with this session's environment and nothing of the real one's XDG, home or path.
    pub(crate) fn command(&self, program: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(program);
        command
            .args(args)
            .env("XDG_RUNTIME_DIR", self.runtime_dir())
            .env("XDG_STATE_HOME", self.state_home())
            .env("XDG_CONFIG_HOME", self.config_home())
            .env("HOME", self.dir.path().join("home"))
            .env("PATH", self.bin())
            // With no address the daemon tries the bus under the temp runtime directory, which never exists.
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .env(
                "DBUS_SYSTEM_BUS_ADDRESS",
                bus::bus_address(self, NO_SYSTEM_BUS),
            );
        command
    }

    /// The daemon under umask 0, so the modes it sets are its own and not the runner's mask.
    pub(crate) fn unmasked_daemon(&self) -> Command {
        self.command(Path::new(SHELL), &["-c", UNMASKED, binary()])
    }

    /// Starts a daemon with `args`, its stderr going to a log file in the session.
    pub(crate) fn spawn(&self, args: &[&str], log_name: &str) -> Daemon {
        self.spawn_command(&mut self.command(Path::new(binary()), args), log_name)
    }

    /// Starts `command` as a daemon, its stderr going to a log file in the session.
    pub(crate) fn spawn_command(&self, command: &mut Command, log_name: &str) -> Daemon {
        let log = self.dir.path().join(log_name);
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(&log).unwrap())
            .spawn()
            .unwrap();
        Daemon { child, log }
    }

    /// Starts a daemon and waits until its socket accepts, which the daemon binds only after its first state file write.
    pub(crate) fn start(&self) -> Daemon {
        let daemon = self.spawn(&["daemon"], DAEMON_LOG);
        self.wait_accepting();
        daemon
    }

    pub(crate) fn wait_accepting(&self) {
        wait_for("the daemon socket", || {
            UnixStream::connect(self.socket()).ok()
        });
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

/// One protocol 1 connection: request lines out, reply lines back.
pub(crate) struct Proto1 {
    pub(crate) reader: BufReader<UnixStream>,
}

impl Proto1 {
    pub(crate) fn connect(session: &Session) -> Self {
        let stream = UnixStream::connect(session.socket()).unwrap();
        stream.set_read_timeout(Some(HANG_LIMIT)).unwrap();
        Self {
            reader: BufReader::new(stream),
        }
    }

    /// Writes `bytes` as they are, a partial or malformed line included.
    pub(crate) fn write(&mut self, bytes: &[u8]) {
        self.reader.get_mut().write_all(bytes).unwrap();
    }

    pub(crate) fn send(&mut self, request: &str) {
        self.write(format!("{request}\n").as_bytes());
    }

    /// The next line, newline included, or empty at a clean close, failing on a reset or once `HANG_LIMIT` passes; sample input `{"event":"state","state":{...}}\n`.
    pub(crate) fn line(&mut self) -> String {
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        line
    }

    /// The next line, parsed, failing once `HANG_LIMIT` passes or the daemon closes; sample input `{"id":1,"ok":true,"result":"confirmed"}`.
    pub(crate) fn reply(&mut self) -> Value {
        let line = self.line();
        assert!(!line.is_empty(), "the daemon closed the connection");
        serde_json::from_str(&line).unwrap()
    }

    pub(crate) fn ask(&mut self, request: &str) -> Value {
        self.send(request);
        self.reply()
    }

    /// Sends one request line and gives the reply line back as the daemon wrote it.
    pub(crate) fn ask_line(&mut self, request: &str) -> String {
        self.send(request);
        self.line()
    }

    /// Whether the daemon closed this connection with nothing more to read, failing on any other read error; unread request bytes make the close a reset.
    pub(crate) fn closed(&mut self) -> bool {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(read) => read == 0,
            Err(error) if error.kind() == ErrorKind::ConnectionReset => true,
            Err(error) => panic!("reading for the daemon's close: {error}"),
        }
    }
}

pub(crate) fn set_request(id: usize, key: &str, value: &str) -> String {
    format!(r#"{{"v":1,"id":{id},"op":"set","key":"{key}","value":"{value}"}}"#)
}

/// A protocol 1 `get`, answered with the state.
pub(crate) const GET: &str = r#"{"v":1,"id":7,"op":"get"}"#;

pub(crate) fn do_request(id: usize, action: &str) -> String {
    format!(r#"{{"v":1,"id":{id},"op":"do","action":"{action}"}}"#)
}

/// A reply's `result`, or the `code` of its `error`.
pub(crate) fn outcome(reply: &Value) -> &str {
    reply["result"]
        .as_str()
        .or_else(|| reply["error"]["code"].as_str())
        .unwrap_or_else(|| panic!("a reply with neither a result nor an error: {reply}"))
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

    /// Sends SIGTERM and waits for the exit, failing once `HANG_LIMIT` passes.
    pub(crate) fn stop(&mut self) -> ExitStatus {
        kill_process(self.pid(), Signal::TERM).unwrap();
        self.wait_exit()
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
