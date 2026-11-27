//! Pins the client mode to the C++ `pods-ctl`, byte for byte, against a stub daemon socket.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "these tests run the client against a stub, so the daemon helpers the other suites share go unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::process::{Output, Stdio};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use super::support::{Session, binary, run_to_end};

    /// The client's 1000 ms reply timeout, less slack for scheduling.
    const REPLY_TIMEOUT_FLOOR: Duration = Duration::from_millis(900);

    /// The client's 1000 ms reply timeout, plus slack for starting the process on a shared host.
    const REPLY_TIMEOUT_CEILING: Duration = Duration::from_millis(1600);

    /// The command list the usage statement prints after its first line.
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

    /// What the client says when no runtime directory is set.
    const NO_RUNTIME_DIR_START: &str =
        "XDG_RUNTIME_DIR is unset; cannot locate the daemon socket\n";

    /// How the client starts its message when it cannot connect.
    const CONNECT_FAILED_START: &str = "Could not connect to the pods daemon at ";

    /// How the client starts its message when no reply came.
    const TIMED_OUT_START: &str = "Timed out waiting for a reply to ";

    /// What the socket path appends to the runtime directory.
    const SOCKET_SUFFIX: &str = "/pods.sock";

    /// The legacy reply to a request to open a window.
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

    /// A stub daemon serving one client on the session's socket.
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
    fn serve_one(session: &Session, stub: Stub) -> StubDaemon {
        let socket = session.socket();
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

    fn run_ctl(session: &Session, verb: &str) -> Output {
        session.run_client(Path::new(binary()), &["ctl", verb])
    }

    /// `pods ctl status` with `XDG_RUNTIME_DIR` set to `runtime_dir`, or unset when it is `None`.
    fn status_with_runtime_dir(session: &Session, runtime_dir: Option<&str>) -> Output {
        let mut command = session.command(Path::new(binary()), &["ctl", "status"]);
        match runtime_dir {
            Some(dir) => command.env("XDG_RUNTIME_DIR", dir),
            None => command.env_remove("XDG_RUNTIME_DIR"),
        };
        run_to_end(command.stdout(Stdio::piped()).stderr(Stdio::piped()))
    }

    /// A link named `pods-ctl` to the built binary, the way an install provides the old name.
    fn legacy_link(session: &Session) -> PathBuf {
        let link = session.dir.path().join("pods-ctl");
        std::os::unix::fs::symlink(binary(), &link).unwrap();
        link
    }

    fn text(bytes: &[u8]) -> &str {
        std::str::from_utf8(bytes).unwrap()
    }

    #[test]
    fn usage_when_no_verb() {
        let session = Session::new();
        let out = session.run_client(Path::new(binary()), &["ctl"]);
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(
            text(&out.stderr),
            format!("Usage: pods ctl <command>\n{USAGE_COMMANDS}")
        );
    }

    #[test]
    fn usage_when_started_as_pods_ctl() {
        let session = Session::new();
        let out = session.run_client(&legacy_link(&session), &[]);
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(
            text(&out.stderr),
            format!("Usage: pods-ctl <command>\n{USAGE_COMMANDS}")
        );
    }

    #[test]
    fn unset_runtime_dir_is_an_error() {
        let session = Session::new();
        let out = status_with_runtime_dir(&session, None);
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(text(&out.stderr), NO_RUNTIME_DIR_START);
    }

    #[test]
    fn empty_runtime_dir_is_an_error() {
        let session = Session::new();
        let out = status_with_runtime_dir(&session, Some(""));
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(text(&out.stderr), NO_RUNTIME_DIR_START);
    }

    #[test]
    fn connect_failure_names_the_path() {
        let session = Session::new();
        let out = run_ctl(&session, "status");
        assert_eq!(out.status.code(), Some(69));
        assert_eq!(text(&out.stdout), "");
        let path = format!("{}{SOCKET_SUFFIX}", session.runtime_dir().display());
        assert_eq!(
            text(&out.stderr),
            format!("{CONNECT_FAILED_START}{path} (is it running?)\n")
        );
    }

    #[test]
    fn control_verb_writes_bytes_and_exits_zero() {
        let session = Session::new();
        let stub = serve_one(&session, Stub::Drain);
        let out = run_ctl(&session, "noise:anc");
        assert_eq!(stub.received(), b"noise:anc");
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(text(&out.stderr), "");
    }

    #[test]
    fn unknown_verb_is_sent_and_exits_zero() {
        let session = Session::new();
        let stub = serve_one(&session, Stub::Drain);
        let out = run_ctl(&session, "bogusverb");
        assert_eq!(stub.received(), b"bogusverb");
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(text(&out.stderr), "");
    }

    #[test]
    fn status_reply_goes_to_stdout() {
        let session = Session::new();
        let stub = serve_one(&session, Stub::Reply(b"{\"connected\":true}\n"));
        let out = run_ctl(&session, "status");
        assert_eq!(stub.received(), b"status");
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(out.stdout, b"{\"connected\":true}\n");
        assert_eq!(text(&out.stderr), "");
    }

    #[test]
    fn error_reply_goes_to_stderr_and_exits_one() {
        let session = Session::new();
        let stub = serve_one(&session, Stub::Reply(HEADLESS_REFUSAL));
        let out = run_ctl(&session, "reopen");
        assert_eq!(stub.received(), b"reopen");
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(out.stderr, HEADLESS_REFUSAL);
    }

    #[test]
    fn error_reply_to_status_goes_to_stderr_and_exits_one() {
        let session = Session::new();
        let stub = serve_one(&session, Stub::Reply(HEADLESS_REFUSAL));
        let out = run_ctl(&session, "status");
        assert_eq!(stub.received(), b"status");
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(out.stderr, HEADLESS_REFUSAL);
    }

    #[test]
    fn reopen_with_closed_peer_exits_zero() {
        let session = Session::new();
        let stub = serve_one(&session, Stub::Close);
        let out = run_ctl(&session, "reopen");
        assert_eq!(stub.received(), b"reopen");
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(text(&out.stderr), "");
    }

    #[test]
    fn reopen_left_hanging_times_out() {
        let session = Session::new();
        let stub = serve_one(&session, Stub::Hold);
        let out = run_ctl(&session, "reopen");
        assert_eq!(stub.received(), b"reopen");
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(text(&out.stderr), format!("{TIMED_OUT_START}reopen\n"));
    }

    #[test]
    fn status_closed_without_reply_is_an_error() {
        let session = Session::new();
        let stub = serve_one(&session, Stub::Close);
        let started = Instant::now();
        let out = run_ctl(&session, "status");
        let elapsed = started.elapsed();
        assert_eq!(stub.received(), b"status");
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(text(&out.stderr), format!("{TIMED_OUT_START}status\n"));
        // The close is seen at once, so the reply timeout never runs.
        assert!(elapsed < REPLY_TIMEOUT_FLOOR, "{elapsed:?}");
    }

    #[test]
    fn status_times_out_after_one_second() {
        let session = Session::new();
        let stub = serve_one(&session, Stub::Hold);
        let started = Instant::now();
        let out = run_ctl(&session, "status");
        let elapsed = started.elapsed();
        assert_eq!(stub.received(), b"status");
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(text(&out.stderr), format!("{TIMED_OUT_START}status\n"));
        // The floor proves the wait; the ceiling catches a wait well past the deadline, and `run_client` catches a hang.
        assert!(elapsed >= REPLY_TIMEOUT_FLOOR, "{elapsed:?}");
        assert!(elapsed <= REPLY_TIMEOUT_CEILING, "{elapsed:?}");
    }

    #[test]
    fn symlink_named_pods_ctl_dispatches_to_client() {
        let session = Session::new();
        let link = legacy_link(&session);
        let stub = serve_one(&session, Stub::Drain);
        let out = session.run_client(&link, &["noise:anc"]);
        assert_eq!(stub.received(), b"noise:anc");
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(text(&out.stderr), "");
    }
}
