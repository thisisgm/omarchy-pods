//! Drives the built daemon through its socket, its state file, its signals and the client, each run in a temp directory.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::{ErrorKind, Read, Write};
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, ExitStatus, Output, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use cpp_source::MAIN;
    use pods_core::legacy::{LegacyRequest, parse_verb};
    use pods_core::state::State;
    use pods_core::status::to_status_line;
    use rustix::fs::{CWD, Mode, OFlags, mkfifoat, open};
    use rustix::process::{Pid, Signal, kill_process};
    use tempfile::TempDir;

    const BINARY: &str = env!("CARGO_BIN_EXE_librepods");

    /// A daemon or client still running after this long is hung, and the test kills it and fails.
    const HANG_LIMIT: Duration = Duration::from_secs(10);

    /// How often a wait checks its condition again.
    const POLL: Duration = Duration::from_millis(10);

    /// The silent client test's bound on another client's reply.
    const SERVED_WHILE_SILENT_LIMIT: Duration = Duration::from_millis(200);

    /// The daemon's 1000 ms request deadline, plus slack for a shared host.
    const SILENT_CLIENT_DROPPED_BY: Duration = Duration::from_millis(1500);

    /// How long the state file must stay untouched when nothing changes.
    const UNCHANGED_FOR: Duration = Duration::from_secs(3);

    /// The size of the oversized request.
    const OVERSIZED_REQUEST_BYTES: usize = 1024 * 1024;

    /// How much the daemon's resident memory may grow while refusing the oversized request.
    const OVERSIZED_RSS_GROWTH_KIB: u64 = 2 * 1024;

    /// The pause between bytes of a request sent one byte at a time, so each lands in its own read.
    const BYTE_PAUSE: Duration = Duration::from_millis(10);

    /// Clients the daemon serves at once, from `MAX_CONNECTIONS` in `src/daemon/socket.rs`.
    const MAX_CONNECTIONS: usize = 64;

    /// The daemon's request deadline, from `REQUEST_DEADLINE` in `src/daemon/legacy_dialect.rs`; a tokio timeout never fires early.
    const REQUEST_DEADLINE: Duration = Duration::from_millis(1000);

    /// Half the daemon's 1000 ms request deadline: a client closed sooner was closed at accept, not by the deadline.
    const CLOSED_AT_ACCEPT_WITHIN: Duration = Duration::from_millis(500);

    /// Twice the daemon's 4096 byte cap, `MAX_REQUEST_BYTES` in `src/daemon/legacy_dialect.rs`: small enough to sit in the socket buffer.
    const PAST_THE_CAP_BYTES: usize = 2 * 4096;

    /// Two and a half of the daemon's one second write retries, long enough to see a repeated log line.
    const RETRIES_WATCHED: Duration = Duration::from_millis(2500);

    /// The lock file's extension beside the socket, from `LOCK_EXTENSION` in `src/daemon/socket.rs`.
    const LOCK_EXTENSION: &str = "lock";

    /// SIGTERM is signal 15, so bit 14 of a signal mask in `/proc/<pid>/status`.
    const SIGTERM_BIT: u64 = 1 << 14;

    /// The level the adaptive verb sends.
    const ADAPTIVE_VERB: &str = "adaptive:37";

    #[expect(
        clippy::disallowed_methods,
        reason = "std has no timed wait for a child process or a file, and this is the test harness, not the core"
    )]
    fn pause(duration: Duration) {
        thread::sleep(duration);
    }

    /// Waits for `ready` to give a value, failing the test once `HANG_LIMIT` passes.
    fn wait_for<T>(what: &str, mut ready: impl FnMut() -> Option<T>) -> T {
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
    struct Session {
        dir: TempDir,
    }

    impl Session {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            for sub in ["run", "state", "home"] {
                fs::create_dir(dir.path().join(sub)).unwrap();
            }
            fs::set_permissions(dir.path().join("run"), fs::Permissions::from_mode(0o700)).unwrap();
            Self { dir }
        }

        fn runtime_dir(&self) -> PathBuf {
            self.dir.path().join("run")
        }

        fn state_home(&self) -> PathBuf {
            self.dir.path().join("state")
        }

        fn socket(&self) -> PathBuf {
            self.runtime_dir().join("librepods.sock")
        }

        fn state_file(&self) -> PathBuf {
            self.state_home().join("librepods").join("status.json")
        }

        /// The binary with this session's environment and nothing of the real one's XDG or home.
        fn command(&self, program: &Path, args: &[&str]) -> Command {
            let mut command = Command::new(program);
            command
                .args(args)
                .env("XDG_RUNTIME_DIR", self.runtime_dir())
                .env("XDG_STATE_HOME", self.state_home())
                .env("HOME", self.dir.path().join("home"));
            command
        }

        /// Starts a daemon with `args`, its stderr going to a log file in the session.
        fn spawn(&self, args: &[&str], log_name: &str) -> Daemon {
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
        fn start(&self) -> Daemon {
            let daemon = self.spawn(&["daemon"], "daemon.log");
            wait_for("the daemon socket", || {
                UnixStream::connect(self.socket()).ok()
            });
            daemon
        }

        /// Sends `verb` without closing the write side, as `librepods-ctl` does, and reads until the daemon closes.
        fn ask(&self, verb: &[u8]) -> Vec<u8> {
            self.try_ask(verb).unwrap()
        }

        /// `ask`, giving back the error of a connection the daemon closed at accept.
        fn try_ask(&self, verb: &[u8]) -> std::io::Result<Vec<u8>> {
            let mut stream = UnixStream::connect(self.socket())?;
            stream.write_all(verb)?;
            stream.set_read_timeout(Some(HANG_LIMIT))?;
            let mut reply = Vec::new();
            stream.read_to_end(&mut reply)?;
            Ok(reply)
        }

        /// Sends `verb`, closes the write side, and reads until the daemon closes.
        fn send(&self, verb: &[u8]) -> Vec<u8> {
            let mut stream = UnixStream::connect(self.socket()).unwrap();
            stream.write_all(verb).unwrap();
            stream.shutdown(std::net::Shutdown::Write).unwrap();
            read_to_close(&mut stream)
        }

        fn status(&self) -> String {
            String::from_utf8(self.ask(b"status")).unwrap()
        }

        /// Runs the client to completion, or kills it and fails once it passes `HANG_LIMIT`.
        fn run_client(&self, program: &Path, args: &[&str]) -> Output {
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
    fn read_to_close(stream: &mut UnixStream) -> Vec<u8> {
        stream.set_read_timeout(Some(HANG_LIMIT)).unwrap();
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).unwrap();
        reply
    }

    /// A daemon process, killed when the test ends if it is still running.
    struct Daemon {
        child: Child,
        log: PathBuf,
    }

    impl Daemon {
        fn pid(&self) -> Pid {
            Pid::from_child(&self.child)
        }

        fn wait_exit(&mut self) -> ExitStatus {
            wait_for("the daemon to exit", || self.child.try_wait().unwrap())
        }

        fn running(&mut self) -> bool {
            self.child.try_wait().unwrap().is_none()
        }

        fn log(&self) -> String {
            fs::read_to_string(&self.log).unwrap()
        }

        /// The daemon's resident memory in KiB, from `/proc/<pid>/status`.
        // Sample input: `VmRSS:\t    4512 kB`
        fn rss_kib(&self) -> u64 {
            let status = fs::read_to_string(format!("/proc/{}/status", self.child.id())).unwrap();
            let line = status
                .lines()
                .find(|line| line.starts_with("VmRSS:"))
                .unwrap();
            line.split_whitespace().nth(1).unwrap().parse().unwrap()
        }

        /// Whether the daemon has installed its SIGTERM handler, from `/proc/<pid>/status`.
        // Sample input: `SigCgt:\t0000000000004003`
        fn catches_sigterm(&self) -> bool {
            let status = fs::read_to_string(format!("/proc/{}/status", self.child.id())).unwrap();
            let mask = status
                .lines()
                .find_map(|line| line.strip_prefix("SigCgt:"))
                .unwrap();
            u64::from_str_radix(mask.trim(), 16).unwrap() & SIGTERM_BIT != 0
        }
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            // A daemon that already exited refuses the kill, which is the outcome wanted.
            drop(self.child.kill());
            drop(self.child.wait());
        }
    }

    /// The reply a headless daemon gives to `reopen`, read from `daemon/main.cpp`.
    fn headless_refusal() -> Vec<u8> {
        MAIN.between("clientSocket->write(\"", "\");")
            .replace("\\n", "\n")
            .into_bytes()
    }

    /// Every verb of the C++ dispatch, and one adaptive level.
    fn every_verb() -> Vec<&'static str> {
        let mut verbs = MAIN.all_between("msg == \"", "\"");
        verbs.push(ADAPTIVE_VERB);
        verbs
    }

    /// One `*_total` value from a status line.
    fn total(status: &str, key: &str) -> u64 {
        let status: serde_json::Value = serde_json::from_str(status).unwrap();
        status[key].as_u64().unwrap()
    }

    #[test]
    fn status_over_the_socket_matches_the_state_file() {
        let session = Session::new();
        let _daemon = session.start();
        let status = session.status();
        assert_eq!(status, to_status_line(&State::default()));
        assert_eq!(fs::read_to_string(session.state_file()).unwrap(), status);
        assert_eq!(session.send(b"ear:off"), b"");
        let changed = session.status();
        assert_ne!(changed, status);
        wait_for("the state file to follow the change", || {
            (fs::read_to_string(session.state_file()).unwrap() == changed).then_some(())
        });
    }

    #[test]
    fn failed_write_is_retried_until_it_lands() {
        let session = Session::new();
        let daemon = session.start();
        let dir = session.state_file().parent().unwrap().to_path_buf();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(session.send(b"ear:off"), b"");
        let changed = session.status();
        wait_for("the failed write in the log", || {
            daemon
                .log()
                .contains("cannot publish the state file")
                .then_some(())
        });
        assert_ne!(fs::read_to_string(session.state_file()).unwrap(), changed);
        pause(RETRIES_WATCHED);
        let failures = daemon
            .log()
            .matches("cannot publish the state file")
            .count();
        assert_eq!(failures, 1, "{}", daemon.log());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        wait_for("the retried write", || {
            (fs::read_to_string(session.state_file()).unwrap() == changed).then_some(())
        });
        let recovered = format!(
            "state file {} is published again",
            session.state_file().display()
        );
        wait_for("the recovery in the log", || {
            daemon.log().contains(&recovered).then_some(())
        });
        assert_eq!(session.send(b"ear:one"), b"");
        let next = session.status();
        wait_for("the next write", || {
            (fs::read_to_string(session.state_file()).unwrap() == next).then_some(())
        });
        assert_eq!(
            daemon.log().matches(&recovered).count(),
            1,
            "{}",
            daemon.log()
        );
    }

    #[test]
    fn clients_past_the_limit_are_closed_at_once() {
        let session = Session::new();
        let daemon = session.start();
        // One finished exchange first: the daemon reaps finished clients at each accept, so the start probe's slot is free.
        assert_eq!(session.status(), to_status_line(&State::default()));
        let clients: Vec<UnixStream> = (0..=MAX_CONNECTIONS)
            .map(|_| UnixStream::connect(session.socket()).unwrap())
            .collect();
        let connected = Instant::now();
        let (within, past) = clients.split_at(MAX_CONNECTIONS);
        let mut past = &past[0];
        past.set_read_timeout(Some(CLOSED_AT_ACCEPT_WITHIN))
            .unwrap();
        let mut byte = [0_u8; 1];
        assert_eq!(
            past.read(&mut byte).unwrap(),
            0,
            "the client past the limit"
        );
        let waited = connected.elapsed();
        assert!(waited < CLOSED_AT_ACCEPT_WITHIN, "{waited:?}");
        for (index, mut client) in within.iter().enumerate() {
            client.set_nonblocking(true).unwrap();
            let read = client.read(&mut byte);
            assert!(
                matches!(&read, Err(error) if error.kind() == ErrorKind::WouldBlock),
                "client {index} within the limit: {read:?}"
            );
        }
        assert!(
            daemon.log().contains("clients already connected"),
            "{}",
            daemon.log()
        );
        drop(clients);
        // The daemon frees a slot only once it has run the departed clients' tasks, so the first try may be closed at accept.
        let default = to_status_line(&State::default()).into_bytes();
        wait_for("a slot freed by the departed clients", || {
            (session.try_ask(b"status").ok()? == default).then_some(())
        });
    }

    #[test]
    fn socket_and_state_file_are_mode_0600() {
        let session = Session::new();
        let _daemon = session.start();
        for path in [session.socket(), session.state_file()] {
            let mode = fs::symlink_metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{}", path.display());
        }
    }

    #[test]
    fn every_legacy_verb_is_accepted_and_only_status_and_reopen_reply() {
        let session = Session::new();
        let daemon = session.start();
        let mut expected = State::default();
        for verb in every_verb() {
            let reply = session.send(verb.as_bytes());
            match parse_verb(verb.as_bytes()).unwrap() {
                LegacyRequest::Status => assert_eq!(reply, to_status_line(&expected).as_bytes()),
                LegacyRequest::Command(command) => {
                    pods_core::command::apply(&mut expected, &command);
                    let wanted = if verb == "reopen" {
                        headless_refusal()
                    } else {
                        Vec::new()
                    };
                    assert_eq!(reply, wanted, "{verb}");
                }
            }
        }
        let status = session.status();
        assert_eq!(status, to_status_line(&expected));
        assert_eq!(total(&status, "reopen_calls_total"), 1);
        assert_eq!(total(&status, "ear_detection_changes_total"), 3);
        assert_eq!(daemon.log(), "");
    }

    #[test]
    fn reopen_reply_is_the_headless_refusal() {
        let session = Session::new();
        let _daemon = session.start();
        assert_eq!(session.ask(b"reopen"), headless_refusal());
        assert_eq!(total(&session.status(), "reopen_calls_total"), 1);
    }

    #[test]
    fn unknown_verb_gets_no_reply_and_daemon_keeps_serving() {
        let session = Session::new();
        let daemon = session.start();
        assert_eq!(session.send(b"bogusverb"), b"");
        assert_eq!(session.send(b"status\n"), b"");
        assert_eq!(session.status(), to_status_line(&State::default()));
        assert!(daemon.log().contains("bogusverb"), "{}", daemon.log());
    }

    #[test]
    fn second_instance_exits_zero_and_first_keeps_serving() {
        let session = Session::new();
        let mut first = session.start();
        // Paused, the first daemon reads `reopen` only after the second has exited, so its reply always meets a closed peer.
        kill_process(first.pid(), Signal::STOP).unwrap();
        let mut second = session.spawn(&["--headless"], "second.log");
        assert_eq!(second.wait_exit().code(), Some(0));
        kill_process(first.pid(), Signal::CONT).unwrap();
        assert!(first.running());
        wait_for("the second instance's reopen", || {
            (total(&session.status(), "reopen_calls_total") == 1).then_some(())
        });
        assert_eq!(
            first.log(),
            "",
            "a normal second launch logged on the first daemon"
        );
    }

    // Shows a simultaneous start leaves one serving daemon; the loser may stop at the probe or the lock, so the lock's own proof is the unit test `lock_is_refused_while_another_holds_it`.
    #[test]
    fn two_daemons_started_together_leave_exactly_one() {
        let session = Session::new();
        let mut one = session.spawn(&["daemon"], "one.log");
        let mut two = session.spawn(&["--headless"], "two.log");
        let exited = wait_for("one of the two daemons to exit", || {
            match (one.child.try_wait().unwrap(), two.child.try_wait().unwrap()) {
                (Some(status), None) | (None, Some(status)) => Some(status),
                (Some(_), Some(_)) => panic!("both daemons exited"),
                (None, None) => None,
            }
        });
        assert_eq!(exited.code(), Some(0));
        wait_for("the survivor's socket", || {
            UnixStream::connect(session.socket()).ok()
        });
        // The loser either saw the survivor serving and sent it `reopen`, or found the lock taken.
        let status = session.status();
        let reopens = total(&status, "reopen_calls_total");
        assert!(reopens <= 1, "{status}");
        let mut expected = State::default();
        expected.counters.reopen_calls = reopens;
        assert_eq!(status, to_status_line(&expected));
        assert_ne!(one.running(), two.running());
    }

    #[test]
    fn stale_socket_is_replaced() {
        let session = Session::new();
        drop(UnixListener::bind(session.socket()).unwrap());
        assert!(session.socket().exists());
        let _daemon = session.start();
        assert_eq!(session.status(), to_status_line(&State::default()));
    }

    #[test]
    fn sigterm_removes_socket_and_state_file() {
        for signal in [Signal::TERM, Signal::INT, Signal::HUP] {
            let session = Session::new();
            let mut daemon = session.start();
            assert!(session.state_file().exists());
            kill_process(daemon.pid(), signal).unwrap();
            assert_eq!(daemon.wait_exit().code(), Some(0), "{signal:?}");
            assert!(!session.socket().exists(), "{signal:?}");
            assert!(!session.state_file().exists(), "{signal:?}");
        }
    }

    #[test]
    fn sigterm_during_startup_is_handled_once_startup_finishes() {
        let session = Session::new();
        let lock = session.socket().with_extension(LOCK_EXTENSION);
        // Opening a FIFO to write waits for a reader, so the daemon cannot pass its lock until the test opens one.
        mkfifoat(CWD, &lock, Mode::from_raw_mode(0o600)).unwrap();
        let mut daemon = session.spawn(&["daemon"], "daemon.log");
        wait_for("the daemon's SIGTERM handler", || {
            daemon.catches_sigterm().then_some(())
        });
        kill_process(daemon.pid(), Signal::TERM).unwrap();
        let reader = open(&lock, OFlags::RDONLY | OFlags::NONBLOCK, Mode::empty()).unwrap();
        assert_eq!(daemon.wait_exit().code(), Some(0));
        assert_eq!(daemon.log(), "SIGTERM received, stopping\n");
        assert!(!session.socket().exists());
        assert!(!session.state_file().exists());
        drop(reader);
    }

    #[test]
    fn state_file_is_not_rewritten_when_nothing_changes() {
        let session = Session::new();
        let _daemon = session.start();
        let before = fs::metadata(session.state_file()).unwrap();
        assert_eq!(session.send(b"noise:off"), b"");
        assert_eq!(session.status(), to_status_line(&State::default()));
        pause(UNCHANGED_FOR);
        let after = fs::metadata(session.state_file()).unwrap();
        assert_eq!(after.ino(), before.ino());
        assert_eq!(after.modified().unwrap(), before.modified().unwrap());
    }

    #[test]
    fn silent_client_is_dropped_and_others_are_served() {
        let session = Session::new();
        let _daemon = session.start();
        let connected = Instant::now();
        let mut silent = UnixStream::connect(session.socket()).unwrap();
        let asked = Instant::now();
        assert_eq!(session.status(), to_status_line(&State::default()));
        let served = asked.elapsed();
        assert!(served < SERVED_WHILE_SILENT_LIMIT, "{served:?}");
        silent
            .set_read_timeout(Some(SILENT_CLIENT_DROPPED_BY))
            .unwrap();
        let mut byte = [0_u8; 1];
        assert_eq!(silent.read(&mut byte).unwrap(), 0);
        let dropped = connected.elapsed();
        assert!(
            (REQUEST_DEADLINE..SILENT_CLIENT_DROPPED_BY).contains(&dropped),
            "{dropped:?}"
        );
    }

    #[test]
    fn one_byte_at_a_time_request_still_parses() {
        let session = Session::new();
        let _daemon = session.start();
        let mut stream = UnixStream::connect(session.socket()).unwrap();
        for byte in b"status" {
            stream.write_all(&[*byte]).unwrap();
            pause(BYTE_PAUSE);
        }
        assert_eq!(
            read_to_close(&mut stream),
            to_status_line(&State::default()).as_bytes()
        );
        let mut stream = UnixStream::connect(session.socket()).unwrap();
        for byte in b"noise:anc" {
            stream.write_all(&[*byte]).unwrap();
            pause(BYTE_PAUSE);
        }
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        assert_eq!(read_to_close(&mut stream), b"");
        assert_eq!(total(&session.status(), "noise_control_changes_total"), 1);
    }

    #[test]
    fn oversized_request_is_dropped() {
        let session = Session::new();
        let daemon = session.start();
        let before = daemon.rss_kib();
        let mut stream = UnixStream::connect(session.socket()).unwrap();
        stream.set_write_timeout(Some(HANG_LIMIT)).unwrap();
        let written = stream.write_all(&vec![b'a'; OVERSIZED_REQUEST_BYTES]);
        let error = written.unwrap_err();
        assert!(
            matches!(
                error.kind(),
                ErrorKind::BrokenPipe | ErrorKind::ConnectionReset
            ),
            "{error}"
        );
        let grown = daemon.rss_kib().saturating_sub(before);
        assert!(grown < OVERSIZED_RSS_GROWTH_KIB, "grew {grown} KiB");
        // Linux resets the peer of a socket closed with bytes unread, which only a daemon that stopped at its cap leaves.
        let mut stream = UnixStream::connect(session.socket()).unwrap();
        stream.write_all(&[b'a'; PAST_THE_CAP_BYTES]).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        stream.set_read_timeout(Some(HANG_LIMIT)).unwrap();
        let error = stream.read_to_end(&mut Vec::new()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ConnectionReset, "{error}");
        assert_eq!(session.status(), to_status_line(&State::default()));
    }

    #[test]
    fn unset_runtime_dir_fails_with_the_cpp_message() {
        let session = Session::new();
        let mut daemon = {
            let log = session.dir.path().join("daemon.log");
            let child = session
                .command(Path::new(BINARY), &["daemon"])
                .env_remove("XDG_RUNTIME_DIR")
                .stderr(File::create(&log).unwrap())
                .spawn()
                .unwrap();
            Daemon { child, log }
        };
        assert_eq!(daemon.wait_exit().code(), Some(1));
        let cpp = MAIN.between("LOG_ERROR(\"XDG_RUNTIME_DIR", "\");");
        assert_eq!(daemon.log(), format!("XDG_RUNTIME_DIR{cpp}\n"));
    }

    #[test]
    fn missing_runtime_dir_fails_with_the_path_named() {
        let session = Session::new();
        let missing = session.dir.path().join("gone");
        let log = session.dir.path().join("daemon.log");
        let child = session
            .command(Path::new(BINARY), &["daemon"])
            .env("XDG_RUNTIME_DIR", &missing)
            .stderr(File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let mut daemon = Daemon { child, log };
        assert_eq!(daemon.wait_exit().code(), Some(1));
        assert!(
            daemon.log().contains(&missing.display().to_string()),
            "{}",
            daemon.log()
        );
    }

    /// Waits until the daemon has logged a failed write naming the state file, then checks it still serves.
    fn assert_failed_write_logged_and_serving(session: &Session, daemon: &mut Daemon) {
        let named = format!(
            "cannot publish the state file {}:",
            session.state_file().display()
        );
        wait_for("the failed write in the log", || {
            daemon.log().contains(&named).then_some(())
        });
        wait_for("the daemon socket", || {
            UnixStream::connect(session.socket()).ok()
        });
        assert_eq!(session.status(), to_status_line(&State::default()));
        assert!(daemon.running());
    }

    #[test]
    fn read_only_state_dir_fails_with_the_path_named() {
        let session = Session::new();
        let dir = session.state_file().parent().unwrap().to_path_buf();
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        let mut daemon = session.spawn(&["daemon"], "daemon.log");
        assert_failed_write_logged_and_serving(&session, &mut daemon);
        assert!(!session.state_file().exists());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        wait_for("the retried first write", || {
            fs::read_to_string(session.state_file()).ok()
        });
    }

    #[test]
    fn socket_path_that_cannot_be_bound_fails_and_leaves_no_state_file() {
        let session = Session::new();
        fs::create_dir(session.socket()).unwrap();
        let mut daemon = session.spawn(&["daemon"], "daemon.log");
        assert_eq!(daemon.wait_exit().code(), Some(1));
        let log = daemon.log();
        assert!(
            log.contains(&session.socket().display().to_string()),
            "{log}"
        );
        assert!(!session.state_file().exists());
        // Only the first publish creates the state directory, so it ran before the bind failed.
        assert!(session.state_file().parent().unwrap().is_dir());
        assert!(session.socket().is_dir());
    }

    #[test]
    fn held_lock_exits_zero_before_the_first_publish() {
        let session = Session::new();
        let held = File::create(session.socket().with_extension(LOCK_EXTENSION)).unwrap();
        held.try_lock().unwrap();
        let mut daemon = session.spawn(&["daemon"], "daemon.log");
        assert_eq!(daemon.wait_exit().code(), Some(0));
        assert!(
            daemon.log().contains("another daemon is starting"),
            "{}",
            daemon.log()
        );
        assert!(!session.state_file().parent().unwrap().exists());
        assert!(!session.socket().exists());
    }

    #[test]
    fn state_file_path_that_is_a_symlink_is_not_written_through() {
        let session = Session::new();
        let target = session.dir.path().join("target");
        fs::write(&target, "original").unwrap();
        fs::create_dir(session.state_file().parent().unwrap()).unwrap();
        symlink(&target, session.state_file()).unwrap();
        let mut daemon = session.spawn(&["daemon"], "daemon.log");
        assert_failed_write_logged_and_serving(&session, &mut daemon);
        assert_eq!(fs::read_to_string(&target).unwrap(), "original");
        assert!(
            fs::symlink_metadata(session.state_file())
                .unwrap()
                .is_symlink()
        );
    }

    #[test]
    fn rust_client_gets_the_status_line_and_the_exit_code_of_every_verb() {
        let session = Session::new();
        let _daemon = session.start();
        let link = session.dir.path().join("librepods-ctl");
        symlink(BINARY, &link).unwrap();
        for verb in every_verb() {
            let expected_status = session.status();
            for (program, args) in [
                (Path::new(BINARY), vec!["ctl", verb]),
                (link.as_path(), vec![verb]),
            ] {
                let out = session.run_client(program, &args);
                let (code, stdout, stderr) = match verb {
                    "status" => (0, expected_status.clone().into_bytes(), Vec::new()),
                    "reopen" => (1, Vec::new(), headless_refusal()),
                    _ => (0, Vec::new(), Vec::new()),
                };
                assert_eq!(
                    out.status.code(),
                    Some(code),
                    "{verb} via {}",
                    program.display()
                );
                assert_eq!(out.stdout, stdout, "{verb} via {}", program.display());
                assert_eq!(out.stderr, stderr, "{verb} via {}", program.display());
            }
        }
    }
}
