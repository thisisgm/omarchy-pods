//! Pins the client mode to the C++ `librepods-ctl`, byte for byte, against a stub daemon socket.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    const BINARY: &str = env!("CARGO_BIN_EXE_librepods");

    /// The client's 1000 ms reply timeout, less slack for scheduling.
    const REPLY_TIMEOUT_FLOOR: Duration = Duration::from_millis(900);

    /// The client's 1000 ms reply timeout, plus slack for starting the process on a shared host.
    const REPLY_TIMEOUT_CEILING: Duration = Duration::from_millis(1600);

    /// A client still running after this long is hung, and the test kills it and fails.
    const HANG_LIMIT: Duration = Duration::from_secs(10);

    /// How often `run` checks whether the client has exited.
    const EXIT_POLL: Duration = Duration::from_millis(10);

    /// The usage block of `daemon/librepods-ctl.cpp`, after its first line.
    const USAGE_COMMANDS: &str = "Commands:
  status              Print one-line JSON status snapshot to stdout
  noise:off           Disable noise control
  noise:anc           Enable Active Noise Cancellation
  noise:transparency  Enable Transparency mode
  noise:adaptive      Enable Adaptive mode
  noise:cycle         Advance to the next mode (Off->ANC->Trans->Adaptive)
  ear:off             Disable automatic ear-detection auto-pause/resume
  ear:one             Pause when either pod is removed (default)
  ear:both            Pause only when both pods are removed
  forget              Run `bluetoothctl remove` on the connected device
  ca:on               Enable Conversation Awareness (Pro2 only)
  ca:off              Disable Conversation Awareness
  disconnect          bluetoothctl disconnect on the paired AirPods
  connect             bluetoothctl connect on the paired AirPods
  adaptive:N          Set Adaptive Noise level 0-100 (Pro2/Pro3, only while noise_mode=Adaptive)
  onebud:on           Enable One-Bud ANC (Pro2+: keep ANC active with only one pod in)
  onebud:off          Disable One-Bud ANC
";

    /// Exactly what `daemon/main.cpp` writes when it has no window.
    const HEADLESS_REFUSAL: &[u8] = b"error: this daemon runs headless and has no window\n";

    /// What the stub daemon does once a client has connected.
    enum Stub {
        /// Reads until the client closes, as for a verb that expects no reply.
        Drain,
        /// Reads the verb, writes this reply, then closes.
        Reply(&'static [u8]),
        /// Reads the verb, then closes without a word.
        Close,
        /// Reads the verb, then stays silent until the client gives up.
        Hold,
    }

    fn socket_in(dir: &TempDir) -> PathBuf {
        dir.path().join("librepods.sock")
    }

    /// A stub daemon serving one client on the temp socket.
    struct StubDaemon {
        socket: PathBuf,
        thread: JoinHandle<Vec<u8>>,
    }

    impl StubDaemon {
        /// The bytes the stub received, once the client under test has exited.
        fn received(self) -> Vec<u8> {
            // A client that never connected would leave accept blocked forever, so a throwaway connection releases it.
            drop(UnixStream::connect(&self.socket));
            self.thread.join().unwrap()
        }
    }

    /// Starts a stub daemon that serves one client the way `stub` says.
    fn serve_one(dir: &TempDir, stub: Stub) -> StubDaemon {
        let socket = socket_in(dir);
        let listener = UnixListener::bind(&socket).unwrap();
        let thread = thread::spawn(move || {
            let (mut client, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            if let Stub::Drain = stub {
                client.read_to_end(&mut received).unwrap();
                return received;
            }
            let mut chunk = [0_u8; 256];
            let read = client.read(&mut chunk).unwrap();
            received.extend_from_slice(&chunk[..read]);
            match stub {
                Stub::Reply(reply) => client.write_all(reply).unwrap(),
                Stub::Hold => {
                    let mut rest = Vec::new();
                    client.read_to_end(&mut rest).unwrap();
                }
                Stub::Close | Stub::Drain => {}
            }
            received
        });
        StubDaemon { socket, thread }
    }

    /// Runs the client to completion, or kills it and fails the test once it passes `HANG_LIMIT`.
    fn run(program: &Path, args: &[&str], runtime_dir: Option<&Path>) -> Output {
        let mut command = Command::new(program);
        command.args(args).env_remove("XDG_RUNTIME_DIR");
        if let Some(dir) = runtime_dir {
            command.env("XDG_RUNTIME_DIR", dir);
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = Instant::now();
        while child.try_wait().unwrap().is_none() {
            if started.elapsed() > HANG_LIMIT {
                child.kill().unwrap();
                panic!("client {args:?} still running after {HANG_LIMIT:?}, killed");
            }
            #[expect(
                clippy::disallowed_methods,
                reason = "std has no timed wait for a child process, and this is the test harness, not the core"
            )]
            thread::sleep(EXIT_POLL);
        }
        child.wait_with_output().unwrap()
    }

    fn run_ctl(dir: &TempDir, verb: &str) -> Output {
        run(Path::new(BINARY), &["ctl", verb], Some(dir.path()))
    }

    /// A link named `librepods-ctl` to the built binary, the way an install provides the old name.
    fn legacy_link(dir: &TempDir) -> PathBuf {
        let link = dir.path().join("librepods-ctl");
        std::os::unix::fs::symlink(BINARY, &link).unwrap();
        link
    }

    fn text(bytes: &[u8]) -> &str {
        std::str::from_utf8(bytes).unwrap()
    }

    #[test]
    fn usage_when_no_verb() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(Path::new(BINARY), &["ctl"], Some(dir.path()));
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(
            text(&out.stderr),
            format!("Usage: librepods ctl <command>\n{USAGE_COMMANDS}")
        );
    }

    #[test]
    fn usage_when_started_as_librepods_ctl() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(&legacy_link(&dir), &[], Some(dir.path()));
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(
            text(&out.stderr),
            format!("Usage: librepods-ctl <command>\n{USAGE_COMMANDS}")
        );
    }

    #[test]
    fn unset_runtime_dir_is_an_error() {
        let out = run(Path::new(BINARY), &["ctl", "status"], None);
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(
            text(&out.stderr),
            "XDG_RUNTIME_DIR is unset; cannot locate the daemon socket\n"
        );
    }

    #[test]
    fn empty_runtime_dir_is_an_error() {
        let out = run(Path::new(BINARY), &["ctl", "status"], Some(Path::new("")));
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(
            text(&out.stderr),
            "XDG_RUNTIME_DIR is unset; cannot locate the daemon socket\n"
        );
    }

    #[test]
    fn connect_failure_names_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let out = run_ctl(&dir, "status");
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(
            text(&out.stderr),
            format!(
                "Could not connect to OpenPods daemon at {}/librepods.sock (is it running?)\n",
                dir.path().display()
            )
        );
    }

    #[test]
    fn control_verb_writes_bytes_and_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let stub = serve_one(&dir, Stub::Drain);
        let out = run_ctl(&dir, "noise:anc");
        assert_eq!(stub.received(), b"noise:anc");
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(text(&out.stderr), "");
    }

    #[test]
    fn unknown_verb_is_sent_and_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let stub = serve_one(&dir, Stub::Drain);
        let out = run_ctl(&dir, "bogusverb");
        assert_eq!(stub.received(), b"bogusverb");
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(text(&out.stderr), "");
    }

    #[test]
    fn status_reply_goes_to_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let stub = serve_one(&dir, Stub::Reply(b"{\"connected\":true}\n"));
        let out = run_ctl(&dir, "status");
        assert_eq!(stub.received(), b"status");
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(out.stdout, b"{\"connected\":true}\n");
        assert_eq!(text(&out.stderr), "");
    }

    #[test]
    fn error_reply_goes_to_stderr_and_exits_one() {
        let dir = tempfile::tempdir().unwrap();
        let stub = serve_one(&dir, Stub::Reply(HEADLESS_REFUSAL));
        let out = run_ctl(&dir, "reopen");
        assert_eq!(stub.received(), b"reopen");
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(out.stderr, HEADLESS_REFUSAL);
    }

    #[test]
    fn error_reply_to_status_goes_to_stderr_and_exits_one() {
        let dir = tempfile::tempdir().unwrap();
        let stub = serve_one(&dir, Stub::Reply(HEADLESS_REFUSAL));
        let out = run_ctl(&dir, "status");
        assert_eq!(stub.received(), b"status");
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(out.stderr, HEADLESS_REFUSAL);
    }

    #[test]
    fn reopen_with_closed_peer_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let stub = serve_one(&dir, Stub::Close);
        let out = run_ctl(&dir, "reopen");
        assert_eq!(stub.received(), b"reopen");
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(text(&out.stderr), "");
    }

    #[test]
    fn reopen_left_hanging_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let stub = serve_one(&dir, Stub::Hold);
        let out = run_ctl(&dir, "reopen");
        assert_eq!(stub.received(), b"reopen");
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(
            text(&out.stderr),
            "Timed out waiting for a reply to reopen\n"
        );
    }

    #[test]
    fn status_closed_without_reply_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let stub = serve_one(&dir, Stub::Close);
        let started = Instant::now();
        let out = run_ctl(&dir, "status");
        let elapsed = started.elapsed();
        assert_eq!(stub.received(), b"status");
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(
            text(&out.stderr),
            "Timed out waiting for a reply to status\n"
        );
        // The close is seen at once, so the reply timeout never runs.
        assert!(elapsed < REPLY_TIMEOUT_FLOOR, "{elapsed:?}");
    }

    #[test]
    fn status_times_out_after_one_second() {
        let dir = tempfile::tempdir().unwrap();
        let stub = serve_one(&dir, Stub::Hold);
        let started = Instant::now();
        let out = run_ctl(&dir, "status");
        let elapsed = started.elapsed();
        assert_eq!(stub.received(), b"status");
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(
            text(&out.stderr),
            "Timed out waiting for a reply to status\n"
        );
        // The floor proves the wait; the ceiling catches a wait well past the deadline, and `run` catches a hang.
        assert!(elapsed >= REPLY_TIMEOUT_FLOOR, "{elapsed:?}");
        assert!(elapsed <= REPLY_TIMEOUT_CEILING, "{elapsed:?}");
    }

    #[test]
    fn symlink_named_librepods_ctl_dispatches_to_client() {
        let dir = tempfile::tempdir().unwrap();
        let link = legacy_link(&dir);
        let stub = serve_one(&dir, Stub::Drain);
        let out = run(&link, &["noise:anc"], Some(dir.path()));
        assert_eq!(stub.received(), b"noise:anc");
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(text(&out.stderr), "");
    }
}
