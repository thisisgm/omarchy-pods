//! The built binary's live `probe source` against the fake pods under the socket trap, on a private audio server, with stub `systemctl` and `pactl` alone on its `PATH`.

#[cfg(test)]
#[expect(
    dead_code,
    reason = "the parts of the fake pods only the unit tests of the capture loop read go unused here"
)]
#[path = "../src/probe/capture/tests/att_pods.rs"]
mod att_pods;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the helpers of the capture runs and their stores go unused here"
)]
#[path = "support/capture.rs"]
mod capture;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the parts of the fake pods only the unit tests of the capture loop read go unused here"
)]
#[path = "../src/probe/capture/tests/pods.rs"]
mod pods;

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::Receiver;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use pods_proto::packets::CONTROL_PSM;
    use pods_proto::uplink::{START, STOP};
    use pods_pwrig::{Server, environment, node_names};
    use pods_samples::uplink::state_marker;
    use pods_trap::Outcome;
    use tempfile::TempDir;

    use super::capture::{
        FAILED, HANG_LIMIT, Ran, Running, far_end, hand_over, home_with_device, route, transcript,
    };
    use super::pods::{self, Hook, Pods, SILENT_UNIT, Transcript, audio_sdu};

    const NODE: &str = "omapods_input.probe";
    const INSTALLED: &str = "librepods.service";
    const UNDER_TEST: &str = "librepods-hw.service";
    const EXECUTABLE: u32 = 0o755;
    const NS_PER_MS: u64 = 1_000_000;
    /// What the two stub calls around the hold add to it, a fork and a `date` each, with room for a loaded host.
    const HOLD_SLACK: Duration = Duration::from_secs(2);
    /// A unit the decoder refuses: a frame count is due after its first byte, and none follows.
    const REFUSED_UNIT: [u8; 1] = [0x03];
    /// The audio SDU after which the signal test raises its flag and the pods go quiet, so with no `--seconds` only the signal ends the run.
    const SIGNAL_AFTER: u32 = 50;
    /// What the stub log holds once the fake pods have heard STOP.
    const HEARD_STOP: &str = "pods heard STOP";
    /// The calls that find the installed service running and stop it.
    const STOPPED: [&str; 3] = [
        "systemctl --user is-active librepods.service",
        "systemctl --user stop librepods.service",
        "systemctl --user is-active librepods.service",
    ];
    /// The calls of the restore, the sink found among the others listed.
    const RESTORE: [&str; 3] = [
        "pactl list short sinks",
        "pactl suspend-sink bluez_output.02_00_00_00_00_01.1 1",
        "pactl suspend-sink bluez_output.02_00_00_00_00_01.1 0",
    ];
    /// The calls that put the installed service back.
    const STARTED: [&str; 4] = [
        "systemctl --user stop librepods-hw.service",
        "systemctl --user is-active librepods-hw.service",
        "systemctl --user start librepods.service",
        "systemctl --user is-active librepods.service",
    ];

    /// A `systemctl --user` that logs each call and keeps each unit's state in a file named after it, a stop leaving the state a `.stops-as` file names; builtins only, as `PATH` holds nothing else.
    const SYSTEMCTL: &str = r#"#!/bin/sh
printf 'systemctl %s\n' "$*" >> "$STUB/calls"
case "$2" in
is-active) read -r s < "$STUB/$3"; printf '%s\n' "$s"; [ "$s" = active ] || exit 3 ;;
stop) s=inactive; [ -e "$STUB/$3.stops-as" ] && read -r s < "$STUB/$3.stops-as"; printf '%s\n' "$s" > "$STUB/$3" ;;
start) printf 'active\n' > "$STUB/$3" ;;
*) exit 1 ;;
esac
"#;
    /// A `pactl` that logs each call with the time it ran in nanoseconds and lists a speaker sink, then the device's; a resume fails while a `resume-fails` file is there.
    const PACTL: &str = r#"#!/bin/sh
printf 'pactl %s %s\n' "$*" "$(/usr/bin/date +%s%N)" >> "$STUB/calls"
case "$1" in
list) printf '56\talsa_output.speakers\tPipeWire\ts16le 2ch 48000Hz\tRUNNING\n57\tbluez_output.02_00_00_00_00_01.1\tPipeWire\ts16le 2ch 48000Hz\tSUSPENDED\n' ;;
suspend-sink) if [ "$3" = 0 ] && [ -e "$STUB/resume-fails" ]; then exit 1; fi ;;
esac
"#;

    /// The stub tools, their log, and the state of each unit, in a directory of the test's own.
    struct Stubs {
        dir: TempDir,
    }

    impl Stubs {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("bin");
            fs::create_dir(&bin).unwrap();
            for (name, script) in [("systemctl", SYSTEMCTL), ("pactl", PACTL)] {
                let path = bin.join(name);
                fs::write(&path, script).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(EXECUTABLE)).unwrap();
            }
            fs::write(dir.path().join(INSTALLED), "active\n").unwrap();
            fs::write(dir.path().join(UNDER_TEST), "inactive\n").unwrap();
            fs::write(dir.path().join("calls"), "").unwrap();
            Self { dir }
        }

        /// A hook that logs the fake pods hearing STOP among the stubs' calls.
        fn heard_stop(&self) -> Hook {
            let calls = self.dir.path().join("calls");
            Arc::new(move || {
                let mut log = OpenOptions::new().append(true).open(&calls).unwrap();
                log.write_all(format!("{HEARD_STOP}\n").as_bytes()).unwrap();
            })
        }

        /// Every call in order, a `pactl` call with its time.
        // Sample input: `pactl suspend-sink bluez_output.02_00_00_00_00_01.1 1 1795412345123456789`.
        fn stamped(&self) -> Vec<(String, Option<u64>)> {
            let log = fs::read_to_string(self.dir.path().join("calls")).unwrap();
            let call = |line: &str| match line.strip_prefix("pactl ") {
                Some(_) => {
                    let (call, stamp) = line.rsplit_once(' ').unwrap();
                    (call.to_owned(), Some(stamp.parse().unwrap()))
                }
                None => (line.to_owned(), None),
            };
            log.lines().map(call).collect()
        }

        fn calls(&self) -> Vec<String> {
            self.stamped().into_iter().map(|(call, _)| call).collect()
        }

        fn state(&self, unit: &str) -> String {
            fs::read_to_string(self.dir.path().join(unit)).unwrap()
        }
    }

    /// The live command on `server` with the stubs alone on its `PATH`.
    fn command(server: &Server, home: &Path, stubs: &Stubs, args: &[&str]) -> Command {
        let mut command = server.command(env!("CARGO_BIN_EXE_xtask"));
        command
            .args(["probe", "source"])
            .args(args)
            .env("HOME", home)
            .env("PATH", stubs.dir.path().join("bin"))
            .env("STUB", stubs.dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    /// The live command, its far end played by `part`.
    fn start(
        server: &Server,
        home: &Path,
        stubs: &Stubs,
        args: &[&str],
        part: Pods,
        flag: &Arc<AtomicBool>,
    ) -> (Running, Receiver<Transcript>) {
        let command = command(server, home, stubs, args);
        let (answer, handed) = hand_over();
        let running = Running::start(command, vec![route(CONTROL_PSM, answer)]);
        let played = pods::spawn(far_end(&handed), part, Arc::clone(flag));
        (running, played)
    }

    /// A hook that keeps whether the private server has the node at the moment it runs.
    fn node_seen(dir: &Path, seen: &Arc<Mutex<Option<bool>>>) -> Hook {
        let (dir, seen) = (dir.to_path_buf(), Arc::clone(seen));
        Arc::new(move || {
            let mut dump = Command::new("pw-dump");
            for (name, value) in environment(&dir) {
                match value {
                    Some(value) => dump.env(name, value),
                    None => dump.env_remove(name),
                };
            }
            let printed = dump.output().unwrap();
            let names = node_names(&serde_json::from_slice(&printed.stdout).unwrap());
            *seen.lock().unwrap() = Some(names.iter().any(|name| name == NODE));
        })
    }

    /// Pods that answer START with the running marker and STOP with the stopped one, logging STOP among the stubs' calls.
    fn answering(stubs: &Stubs) -> Pods {
        Pods {
            starting: vec![state_marker(true).to_vec()],
            on_stop: Some(stubs.heard_stop()),
            parting: vec![state_marker(false).to_vec()],
            ..Pods::default()
        }
    }

    /// The decode errors a status line counts.
    // Sample input: `distance none, rate none, unsteered 0, underruns 0, concealed 4, decode errors 4, decoder resets 0`.
    fn decode_errors(line: &str) -> u32 {
        let (_, after) = line.split_once("decode errors ").unwrap();
        let (count, _) = after.split_once(',').unwrap();
        count.parse().unwrap()
    }

    /// The hold the command says the restore kept.
    // Sample input: `xtask probe: the device's sink was suspended for 300 ms and resumed\n`.
    fn said_hold(stderr: &str) -> Duration {
        let (_, after) = stderr.split_once("suspended for ").unwrap();
        let (ms, _) = after.split_once(" ms and resumed").unwrap();
        Duration::from_millis(ms.parse().unwrap())
    }

    /// Checks the sink was suspended, then resumed no sooner than the hold the command says it kept and not long after.
    fn assert_held(stubs: &Stubs, ran: &Ran) {
        let stamp = |wanted: &str| {
            let mut stamped = stubs.stamped().into_iter();
            stamped.find_map(|(call, stamp)| (call == wanted).then_some(stamp)?)
        };
        let suspended = stamp(RESTORE[1]).unwrap();
        let resumed = stamp(RESTORE[2]).unwrap();
        let held = Duration::from_nanos(resumed.checked_sub(suspended).unwrap());
        let said = said_hold(&ran.stderr);
        assert!(held >= said, "held {held:?}, said {said:?}");
        let not_after = said.checked_add(HOLD_SLACK).unwrap();
        assert!(held <= not_after, "held {held:?}, said {said:?}");
        assert!(said >= Duration::from_nanos(NS_PER_MS), "{}", ran.stderr);
        assert!(!ran.stderr.contains("bluez_output"), "{}", ran.stderr);
        assert!(!ran.stdout.contains("bluez_output"), "{}", ran.stdout);
    }

    fn expected(parts: &[&[&str]]) -> Vec<String> {
        let calls = parts.iter().flat_map(|part| part.iter());
        calls.map(|call| (*call).to_owned()).collect()
    }

    #[test]
    fn a_live_run_ends_with_stop_then_suspends_and_resumes_the_sink_once() {
        let home = home_with_device();
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let stubs = Stubs::new();
        let at_start = Arc::new(Mutex::new(None));
        let part = Pods {
            unit: Some(REFUSED_UNIT.to_vec()),
            on_start: Some(node_seen(dir.path(), &at_start)),
            ..answering(&stubs)
        };
        let flag = Arc::default();
        let args = ["--seconds", "1"];
        let (running, played) = start(&server, home.path(), &stubs, &args, part, &flag);
        server.wait_node(NODE, HANG_LIMIT);
        let ran = running.finish();
        let pods = transcript(&played);

        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        assert_eq!(*at_start.lock().unwrap(), Some(false));
        let last = ran.stdout.lines().last().unwrap();
        assert!(decode_errors(last) > 0, "{}", ran.stdout);
        assert_eq!((pods.count(&START), pods.count(&STOP)), (1, 1));
        let heard = [HEARD_STOP];
        let wanted = expected(&[&STOPPED, &heard, &RESTORE, &STARTED]);
        assert_eq!(stubs.calls(), wanted);
        assert_held(&stubs, &ran);
        assert_eq!(stubs.state(INSTALLED), "active\n");
        assert_eq!(ran.dials, [(CONTROL_PSM, Outcome::Accepted)]);
        server.wait_no_node(NODE, HANG_LIMIT);
    }

    #[test]
    fn a_signal_mid_uplink_ends_with_stop_then_suspends_and_resumes_the_sink() {
        let home = home_with_device();
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let stubs = Stubs::new();
        // Quiet from the flag on, so the signal lands while the command waits for a packet.
        let part = Pods {
            interrupt_after: Some(SIGNAL_AFTER),
            stream_for: Some(SIGNAL_AFTER),
            ..answering(&stubs)
        };
        let flag = Arc::new(AtomicBool::new(false));
        let (running, played) = start(&server, home.path(), &stubs, &[], part, &flag);
        running.interrupt_within(HANG_LIMIT, &flag);
        let ran = running.finish();
        let pods = transcript(&played);

        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        assert_eq!((pods.count(&START), pods.count(&STOP)), (1, 1));
        let heard = [HEARD_STOP];
        let wanted = expected(&[&STOPPED, &heard, &RESTORE, &STARTED]);
        assert_eq!(stubs.calls(), wanted);
        assert_held(&stubs, &ran);
        assert_eq!(stubs.state(INSTALLED), "active\n");
    }

    #[test]
    fn pods_that_stop_on_their_own_get_no_stop_and_the_sink_is_still_restored() {
        let home = home_with_device();
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let stubs = Stubs::new();
        let stopped = [state_marker(true), state_marker(false)].map(|marker| marker.to_vec());
        let part = Pods {
            burst: Some(vec![
                stopped[0].clone(),
                audio_sdu(0, &SILENT_UNIT),
                stopped[1].clone(),
            ]),
            ..Pods::default()
        };
        let (running, played) = start(&server, home.path(), &stubs, &[], part, &Arc::default());
        let ran = running.finish();
        let pods = transcript(&played);

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        let said = "the pods stopped the uplink on their own";
        assert!(ran.stderr.contains(said), "{}", ran.stderr);
        assert_eq!((pods.count(&START), pods.count(&STOP)), (1, 0));
        assert_eq!(stubs.calls(), expected(&[&STOPPED, &RESTORE, &STARTED]));
        assert_held(&stubs, &ran);
        assert_eq!(stubs.state(INSTALLED), "active\n");
    }

    #[test]
    fn an_unanswered_stop_fails_the_run_and_the_sink_is_still_restored() {
        let home = home_with_device();
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let stubs = Stubs::new();
        let part = Pods {
            parting: Vec::new(),
            ..answering(&stubs)
        };
        let args = ["--seconds", "1"];
        let (running, played) = start(&server, home.path(), &stubs, &args, part, &Arc::default());
        let ran = running.finish();
        let pods = transcript(&played);

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        let said = "the pods did not answer STOP within 1500 ms";
        assert!(ran.stderr.contains(said), "{}", ran.stderr);
        assert_eq!((pods.count(&START), pods.count(&STOP)), (1, 1));
        let heard = [HEARD_STOP];
        let wanted = expected(&[&STOPPED, &heard, &RESTORE, &STARTED]);
        assert_eq!(stubs.calls(), wanted);
        assert_held(&stubs, &ran);
        assert_eq!(stubs.state(INSTALLED), "active\n");
    }

    #[test]
    fn an_unanswered_start_sends_no_stop_and_leaves_the_sink_alone() {
        let home = home_with_device();
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let stubs = Stubs::new();
        let (running, played) = start(
            &server,
            home.path(),
            &stubs,
            &[],
            Pods::default(),
            &Arc::default(),
        );
        let ran = running.finish();
        let pods = transcript(&played);

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        let said = "the pods did not answer START within 1500 ms, so nothing more was sent";
        assert!(ran.stderr.contains(said), "{}", ran.stderr);
        assert_eq!((pods.count(&START), pods.count(&STOP)), (1, 0));
        assert_eq!(stubs.calls(), expected(&[&STOPPED, &STARTED]));
        assert_eq!(stubs.state(INSTALLED), "active\n");
        assert_eq!(ran.stdout, "");
    }

    #[test]
    fn a_stop_that_fails_partway_still_starts_the_service_again() {
        let home = home_with_device();
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let stubs = Stubs::new();
        let stops_as = stubs.dir.path().join(format!("{INSTALLED}.stops-as"));
        fs::write(stops_as, "deactivating\n").unwrap();
        let command = command(&server, home.path(), &stubs, &[]);
        let ran = Running::start(command, Vec::new()).finish();

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        let said = "librepods.service is still deactivating after its stop";
        assert!(ran.stderr.contains(said), "{}", ran.stderr);
        assert_eq!(stubs.calls(), expected(&[&STOPPED, &STARTED]));
        assert_eq!(stubs.state(INSTALLED), "active\n");
        assert_eq!(ran.dials, []);
    }

    #[test]
    fn a_resume_that_fails_says_the_sink_stays_suspended() {
        let home = home_with_device();
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let stubs = Stubs::new();
        fs::write(stubs.dir.path().join("resume-fails"), "").unwrap();
        let args = ["--seconds", "1"];
        let part = answering(&stubs);
        let (running, played) = start(&server, home.path(), &stubs, &args, part, &Arc::default());
        let ran = running.finish();
        let pods = transcript(&played);

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        let said = "exited with exit status: 1, so the device's sink stays suspended";
        assert!(ran.stderr.contains(said), "{}", ran.stderr);
        assert!(!ran.stderr.contains("and resumed"), "{}", ran.stderr);
        assert_eq!((pods.count(&START), pods.count(&STOP)), (1, 1));
        let heard = [HEARD_STOP];
        let wanted = expected(&[&STOPPED, &heard, &RESTORE, &STARTED]);
        assert_eq!(stubs.calls(), wanted);
        assert_eq!(stubs.state(INSTALLED), "active\n");
    }

    #[test]
    fn pods_that_stop_on_their_own_get_no_stop_when_the_last_status_line_cannot_be_printed() {
        let home = home_with_device();
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let stubs = Stubs::new();
        let stopped = [state_marker(true), state_marker(false)].map(|marker| marker.to_vec());
        let part = Pods {
            burst: Some(vec![
                stopped[0].clone(),
                audio_sdu(0, &SILENT_UNIT),
                stopped[1].clone(),
            ]),
            ..Pods::default()
        };
        let command = command(&server, home.path(), &stubs, &[]);
        let (answer, handed) = hand_over();
        let mut running = Running::start(command, vec![route(CONTROL_PSM, answer)]);
        // Closed before the far end plays, so no status line can reach the pipe first.
        running.close_stdout();
        let played = pods::spawn(far_end(&handed), part, Arc::default());
        let ran = running.finish();
        let pods = transcript(&played);

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        for said in [
            "could not print the status line",
            "the pods stopped the uplink on their own",
        ] {
            assert!(ran.stderr.contains(said), "{}", ran.stderr);
        }
        assert_eq!((pods.count(&START), pods.count(&STOP)), (1, 0));
        assert_eq!(stubs.calls(), expected(&[&STOPPED, &RESTORE, &STARTED]));
        assert_eq!(stubs.state(INSTALLED), "active\n");
    }
}
