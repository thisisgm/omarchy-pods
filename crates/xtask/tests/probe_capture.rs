//! The built binary's `probe capture` and `probe stop` commands against the fake pods, every L2CAP connect of the command taken by the socket trap, and `probe analyse` on the dictation runs they leave.

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
    reason = "the parts of the fake pods only the unit tests of the capture loop read go unused here"
)]
#[path = "../src/probe/capture/tests/pods.rs"]
mod pods;

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod capture {
    mod analysed;

    use std::fs::{self, OpenOptions};
    use std::io::{Read, Write};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use pods_proto::att::{self, CHARACTERISTIC_DECLARATION};
    use pods_proto::dictation::{self, DictationPacket, SESSION_ACTIVE, SESSION_IDLE};
    use pods_proto::packets::{
        ATT_PSM, CONTROL_PSM, HANDSHAKE, REQUEST_NOTIFICATIONS, SET_SPECIFIC_FEATURES,
    };
    use pods_proto::sdulog::{self, Direction};
    use pods_proto::uplink::{START, STOP};
    use pods_sys::l2cap::Channel;
    use pods_trap::{Answer, Outcome, Route, Trap};
    use rustix::io::Errno;
    use rustix::process::{Pid, Signal, kill_process};
    use tempfile::TempDir;

    use super::att_pods::{
        BURST_PACKETS, DECLARED, FIND_INFORMATION, MADE_UP_TYPE, TABLE_END, declared_at,
    };
    use super::pods::{self, DEADLINE, Pods, Transcript};

    /// A stamp of the shape `date -u +%Y%m%dT%H%M%SZ` prints.
    const STAMP: &str = "20261121T120000Z";
    /// The synthetic device of the device file, which no output of the command may name.
    const DEVICE: &str = "02:00:00:00:00:01";
    /// The store under the temporary home.
    const ROOT_NAME: &str = "omapods-captures";
    /// The mode the store gives a run directory.
    const OWNER_DIR: u32 = 0o700;
    /// The mode the store gives every file.
    const OWNER_FILE: u32 = 0o600;
    /// The permission bits of a mode, without the file type.
    const PERMISSION_BITS: u32 = 0o777;
    /// The status a command exits with when the script did not run to its end.
    const FAILED: Option<i32> = Some(1);
    /// A command still running this long after the test began to wait has hung, and the test fails.
    const HANG_LIMIT: Duration = Duration::from_secs(30);
    /// How often a wait checks its condition again.
    const POLL: Duration = Duration::from_millis(5);
    /// The cue every soak prints when its stream starts.
    const SOAK_CUE: &str = "cue Stay as you are. Nothing to do.\n";
    /// How long the scripts let the active marker lead the dictation start write.
    const MARKER_LEAD: Duration = Duration::from_millis(480);
    /// How long the trap holds a dial of the attribute channel in the redial test: longer than that lead.
    const SLOW_DIAL: Duration = Duration::from_millis(600);
    /// The three packets of the handshake, in the order the command sends them.
    const HANDSHAKE_SENT: [&[u8]; 3] = [&HANDSHAKE, &SET_SPECIFIC_FEATURES, &REQUEST_NOTIFICATIONS];
    /// The first handle of the range the request for the declarations covers.
    const FIRST_HANDLE: u16 = 0x0001;
    /// The last handle of that range, the highest an attribute can have.
    const LAST_HANDLE: u16 = 0xffff;

    #[expect(
        clippy::disallowed_methods,
        reason = "std has no timed wait for a child process or a flag, and this is the test, not the core"
    )]
    fn pause() {
        thread::sleep(POLL);
    }

    /// Waits for `ready` to give a value, failing the test once `limit` passes.
    fn wait_within<T>(limit: Duration, what: &str, mut ready: impl FnMut() -> Option<T>) -> T {
        let started = Instant::now();
        loop {
            if let Some(value) = ready() {
                return value;
            }
            assert!(started.elapsed() < limit, "no {what} after {limit:?}");
            pause();
        }
    }

    /// One probe command of the built binary, with `HOME` set to `home`, never the real one.
    fn probe(home: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
        command.arg("probe").args(args).env("HOME", home);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    /// A temporary home whose store, made by the built `probe init`, holds the session and the device file.
    fn home_with_device() -> TempDir {
        let home = tempfile::tempdir().unwrap();
        let init = probe(home.path(), &["init", "--stamp", STAMP])
            .output()
            .unwrap();
        assert!(init.status.success(), "{init:?}");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(OWNER_FILE)
            .open(home.path().join(ROOT_NAME).join("device"))
            .unwrap()
            .write_all(format!("{DEVICE}\n").as_bytes())
            .unwrap();
        home
    }

    fn run_path(home: &TempDir, name: &str) -> PathBuf {
        home.path().join(ROOT_NAME).join(STAMP).join(name)
    }

    /// The capture command for the run `name`, then the route and script arguments.
    fn capture(home: &TempDir, name: &str, script: &[&str]) -> Command {
        let run = ["capture", "--stamp", STAMP, "--run", name];
        probe(home.path(), &[&run[..], script].concat())
    }

    /// Answers a dial by handing its far end to the receiver it returns.
    fn hand_over() -> (Answer, mpsc::Receiver<Channel>) {
        hand_over_after(Duration::ZERO)
    }

    /// The same answer, given only once the dial has waited `after`.
    fn hand_over_after(after: Duration) -> (Answer, mpsc::Receiver<Channel>) {
        let (handed, far) = mpsc::channel();
        let serve = move |channel| {
            // The receiver is gone only when the test already failed.
            drop(handed.send(channel));
        };
        let answer = Answer::Accept {
            after,
            serve: Arc::new(serve),
        };
        (answer, far)
    }

    fn refuse(errno: Errno) -> Answer {
        Answer::Refuse {
            after: Duration::ZERO,
            errno,
        }
    }

    fn route(psm: u16, answer: Answer) -> Route {
        Route {
            device: DEVICE.parse().unwrap(),
            psm,
            answer,
        }
    }

    /// The far end of a dial the trap answered.
    fn far_end(handed: &mpsc::Receiver<Channel>) -> Channel {
        handed
            .recv_timeout(HANG_LIMIT)
            .expect("the dial never came")
    }

    /// What a finished command did: its exit code, what it printed, and every dial with its PSM and outcome.
    struct Ran {
        code: Option<i32>,
        stdout: String,
        stderr: String,
        dials: Vec<(u16, Outcome)>,
    }

    /// The built command under the trap; dropping it kills and reaps the command, so a failed assertion leaves no process behind.
    struct Running {
        child: Child,
        trap: Option<Trap>,
    }

    impl Running {
        fn start(command: Command, routes: Vec<Route>) -> Self {
            let (child, trap) =
                pods_trap::spawn(command, routes).unwrap_or_else(|error| panic!("{error}"));
            Self {
                child,
                trap: Some(trap),
            }
        }

        /// Sends SIGINT once the fake pods raise `flag`, as the owner pressing Ctrl+C mid stream; the test fails once `limit` passes.
        fn interrupt_within(&self, limit: Duration, flag: &AtomicBool) {
            let raised = || flag.load(Ordering::SeqCst).then_some(());
            wait_within(limit, "raised flag", raised);
            kill_process(Pid::from_child(&self.child), Signal::INT).unwrap();
        }

        fn finish(mut self) -> Ran {
            let status = wait_within(HANG_LIMIT, "exit", || self.child.try_wait().unwrap());
            let stdout = text_of(self.child.stdout.take());
            let stderr = text_of(self.child.stderr.take());
            let trap = self.trap.take().unwrap();
            let dials = trap.finish().unwrap_or_else(|error| panic!("{error}"));
            let dials = dials.into_iter().map(|dial| (dial.psm, dial.outcome));
            assert!(!stdout.contains(DEVICE) && !stderr.contains(DEVICE));
            Ran {
                code: status.code(),
                stdout,
                stderr,
                dials: dials.collect(),
            }
        }
    }

    impl Drop for Running {
        fn drop(&mut self) {
            // A command that already exited refuses the kill, and the wait then reaps nothing.
            drop(self.child.kill());
            drop(self.child.wait());
        }
    }

    /// The transcript of fake pods that ended with the command.
    fn transcript(played: &mpsc::Receiver<Transcript>) -> Transcript {
        played
            .recv_timeout(DEADLINE)
            .expect("the fake pods did not finish before the deadline")
    }

    /// The names of the files of a run, sorted, after checking the run and each file are the owner's alone and no file names the device.
    fn files_of(run: &Path) -> Vec<String> {
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & PERMISSION_BITS;
        assert_eq!(mode(run), OWNER_DIR, "{}", run.display());
        let mut names: Vec<String> = fs::read_dir(run)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                assert_eq!(mode(&path), OWNER_FILE, "{}", path.display());
                let text = String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned();
                assert!(!text.contains(DEVICE), "{}", path.display());
                path.file_name().unwrap().to_str().unwrap().to_owned()
            })
            .collect();
        names.sort();
        names
    }

    /// Everything a piped stream of a finished command holds.
    fn text_of(stream: Option<impl Read>) -> String {
        let mut text = String::new();
        stream.unwrap().read_to_string(&mut text).unwrap();
        text
    }

    fn read(run: &Path, name: &str) -> String {
        fs::read_to_string(run.join(name)).unwrap()
    }

    /// The packets of a channel log that went one way, in order.
    // Sample input: `1203456789 < 0400040058000100`, a stamp in nanoseconds, the direction and the packet in hex.
    fn logged(log: &str, direction: Direction) -> Vec<Vec<u8>> {
        let records = log.lines().map(|line| sdulog::parse_line(line).unwrap());
        records
            .filter(|record| record.direction == direction)
            .map(|record| record.sdu)
            .collect()
    }

    /// The kind of every mark: the second field of `1203456789\tgate\tear`.
    fn kinds(marks: &str) -> Vec<&str> {
        marks
            .lines()
            .map(|line| line.split('\t').nth(1).unwrap())
            .collect()
    }

    /// The stamp of the only mark of one kind: the first field of `1203456789\tgate\tear`, in nanoseconds.
    fn stamp_of(marks: &str, kind: &str) -> u64 {
        let of_kind = |line: &&str| line.split('\t').nth(1) == Some(kind);
        let found: Vec<&str> = marks.lines().filter(of_kind).collect();
        assert_eq!(found.len(), 1, "{kind} marks in\n{marks}");
        found[0].split('\t').next().unwrap().parse().unwrap()
    }

    /// Asserts the walk of the attribute table and then `writes` reached the fake pods, and that the attribute log holds both directions as they went.
    fn assert_att_ran(pods: &Transcript, run: &Path, writes: &[&[u8]]) {
        let split = pods.att_got.len().saturating_sub(writes.len());
        let (walk, sent) = pods.att_got.split_at(split);
        assert_eq!(sent, writes, "{:02x?}", pods.att_got);
        assert!(!walk.is_empty(), "{:02x?}", pods.att_got);
        assert!(
            walk.iter().all(|pdu| pdu[0] == FIND_INFORMATION),
            "{walk:02x?}"
        );
        let att_log = read(run, "att.log");
        assert_eq!(logged(&att_log, Direction::ToPods), pods.att_got);
        assert_eq!(logged(&att_log, Direction::FromPods), pods.att_sent);
        let listed = read(run, "handles.tsv");
        let attributes = listed.lines().filter(|row| row.starts_with("attribute\t"));
        assert_eq!(attributes.count(), usize::from(TABLE_END), "{listed}");
    }

    /// Asserts the control log of `run` holds both directions as they went.
    fn assert_control_logged(pods: &Transcript, run: &Path) {
        let log = read(run, "sdu.log");
        assert_eq!(logged(&log, Direction::ToPods), pods.got());
        assert_eq!(logged(&log, Direction::FromPods), pods.sent);
    }

    #[test]
    fn a_signal_mid_uplink_stops_the_uplink_and_fails_the_run() {
        let home = home_with_device();
        let (answer, handed) = hand_over();
        let command = capture(&home, "uplink", &["--script", "soak", "--minutes", "1"]);
        let running = Running::start(command, vec![route(CONTROL_PSM, answer)]);
        let flag = Arc::new(AtomicBool::new(false));
        let part = Pods {
            interrupt_after: Some(3),
            ..Pods::default()
        };
        let played = pods::spawn(far_end(&handed), part, Arc::clone(&flag));
        running.interrupt_within(HANG_LIMIT, &flag);
        let ran = running.finish();
        let pods = transcript(&played);

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        assert_eq!(ran.stdout, SOAK_CUE);
        let said =
            "xtask probe: capture ended early: interrupted by a signal; STOP was sent (packets in ";
        assert!(ran.stderr.starts_with(said), "{}", ran.stderr);
        assert!(
            ran.stderr.ends_with(", START sent 1, STOP sent 1)\n"),
            "{}",
            ran.stderr
        );
        assert_eq!(ran.dials, [(CONTROL_PSM, Outcome::Accepted)]);
        assert_eq!(pods.got(), [&HANDSHAKE_SENT[..], &[&START, &STOP]].concat());
        let run = run_path(&home, "uplink");
        assert_eq!(files_of(&run), ["marks.tsv", "meta.txt", "sdu.log"]);
        assert_control_logged(&pods, &run);
        let marks = read(&run, "marks.tsv");
        assert!(
            marks.ends_with("\tend\tinterrupted by a signal\n"),
            "{marks}"
        );
        assert!(read(&run, "meta.txt").starts_with("script=soak\n"));
    }

    #[test]
    fn a_dictation_capture_maps_the_table_records_the_stream_and_stops_it_on_a_signal() {
        let home = home_with_device();
        let (control, control_far) = hand_over();
        let (att, att_far) = hand_over();
        let script = ["--route", "dictation", "--script", "soak", "--minutes", "1"];
        let command = capture(&home, "dictation", &script);
        let routes = vec![route(CONTROL_PSM, control), route(ATT_PSM, att)];
        let running = Running::start(command, routes);
        let flag = Arc::new(AtomicBool::new(false));
        let mut part = Pods::default();
        part.att.interrupt_after = Some(3);
        let played = pods::spawn_dialled(
            Some(far_end(&control_far)),
            att_far,
            part,
            Arc::clone(&flag),
        );
        running.interrupt_within(HANG_LIMIT, &flag);
        let ran = running.finish();
        let pods = transcript(&played);

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        assert_eq!(ran.stdout, SOAK_CUE);
        let said = "xtask probe: capture ended early: interrupted by a signal; the dictation stop was sent (packets in ";
        assert!(ran.stderr.starts_with(said), "{}", ran.stderr);
        let counted = ", dictation start sent 1, dictation stop sent 1, requests refused 0)\n";
        assert!(ran.stderr.ends_with(counted), "{}", ran.stderr);
        let accepted = [
            (CONTROL_PSM, Outcome::Accepted),
            (ATT_PSM, Outcome::Accepted),
        ];
        assert_eq!(ran.dials, accepted);
        let control = [&HANDSHAKE_SENT[..], &[&SESSION_ACTIVE, &SESSION_IDLE]].concat();
        assert_eq!(pods.got(), control);
        let run = run_path(&home, "dictation");
        let files = ["att.log", "handles.tsv", "marks.tsv", "meta.txt", "sdu.log"];
        assert_eq!(files_of(&run), files);
        assert_att_ran(&pods, &run, &[&dictation::START, &dictation::STOP]);
        assert_control_logged(&pods, &run);
        let att_log = read(&run, "att.log");
        let notified = logged(&att_log, Direction::FromPods);
        let audio = notified.iter().filter(|pdu| {
            matches!(
                DictationPacket::parse(pdu),
                Ok(Some(DictationPacket::Audio(_)))
            )
        });
        // More audio than the burst a session opens with, so the stream ran on past it.
        assert!(audio.count() > BURST_PACKETS, "{att_log}");
        let marks = read(&run, "marks.tsv");
        for kind in [
            "handles",
            "active",
            "dictation-start",
            "started",
            "dictation-stop",
            "idle",
        ] {
            assert!(kinds(&marks).contains(&kind), "{kind} in\n{marks}");
        }
        assert!(marks.contains("\thandles\tpresent\n"), "{marks}");
        assert!(
            marks.ends_with("\tend\tinterrupted by a signal\n"),
            "{marks}"
        );
        let meta = read(&run, "meta.txt");
        assert!(
            meta.ends_with("\nroute=dictation\nno_marker=false\nunchecked=false\n"),
            "{meta}"
        );
    }

    #[test]
    fn a_slow_redial_of_the_attribute_channel_leaves_the_marker_its_lead() {
        let home = home_with_device();
        let (control, control_far) = hand_over();
        let (att, att_far) = hand_over_after(SLOW_DIAL);
        let script = ["--route", "dictation", "--script", "soak", "--minutes", "1"];
        let command = capture(&home, "redial", &[&script[..], &["--unchecked"]].concat());
        let routes = vec![route(CONTROL_PSM, control), route(ATT_PSM, att)];
        let running = Running::start(command, routes);
        let flag = Arc::new(AtomicBool::new(false));
        let mut part = Pods::default();
        // The pods answer no request of the walk, so the command closes the channel and dials a fresh one for the start write.
        part.att.answers = Some(0);
        part.att.interrupt_after = Some(3);
        let played = pods::spawn_dialled(
            Some(far_end(&control_far)),
            att_far,
            part,
            Arc::clone(&flag),
        );
        running.interrupt_within(HANG_LIMIT, &flag);
        let ran = running.finish();
        let pods = transcript(&played);

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        let dialled_twice = [
            (CONTROL_PSM, Outcome::Accepted),
            (ATT_PSM, Outcome::Accepted),
            (ATT_PSM, Outcome::Accepted),
        ];
        assert_eq!(ran.dials, dialled_twice);
        assert!(pods.att_got.contains(&dictation::START.to_vec()));
        let marks = read(&run_path(&home, "redial"), "marks.tsv");
        assert!(marks.contains("\thandles\tunchecked\n"), "{marks}");
        let redialled_ns = stamp_of(&marks, "handles");
        assert!(stamp_of(&marks, "gate") >= redialled_ns, "{marks}");
        let lead = stamp_of(&marks, "dictation-start").saturating_sub(redialled_ns);
        assert!(u128::from(lead) >= MARKER_LEAD.as_nanos(), "{marks}");
    }

    #[test]
    fn a_capture_of_both_routes_stops_both_streams_on_a_signal() {
        let home = home_with_device();
        let (control, control_far) = hand_over();
        let (att, att_far) = hand_over();
        let command = capture(&home, "both", &["--route", "both", "--script", "both"]);
        let routes = vec![route(CONTROL_PSM, control), route(ATT_PSM, att)];
        let running = Running::start(command, routes);
        let flag = Arc::new(AtomicBool::new(false));
        // The flag rises three packets into the uplink, which the script starts five seconds after the dictation stream.
        let part = Pods {
            interrupt_after: Some(3),
            ..Pods::default()
        };
        let played = pods::spawn_dialled(
            Some(far_end(&control_far)),
            att_far,
            part,
            Arc::clone(&flag),
        );
        running.interrupt_within(HANG_LIMIT, &flag);
        let ran = running.finish();
        let pods = transcript(&played);

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        assert_eq!(ran.stdout, "cue Count aloud to twenty\n");
        let said = "xtask probe: capture ended early: interrupted by a signal; STOP was sent; the dictation stop was sent (packets in ";
        assert!(ran.stderr.starts_with(said), "{}", ran.stderr);
        let accepted = [
            (CONTROL_PSM, Outcome::Accepted),
            (ATT_PSM, Outcome::Accepted),
        ];
        assert_eq!(ran.dials, accepted);
        let control = [
            &HANDSHAKE_SENT[..],
            &[&SESSION_ACTIVE, &START, &STOP, &SESSION_IDLE],
        ]
        .concat();
        assert_eq!(pods.got(), control);
        let run = run_path(&home, "both");
        let files = ["att.log", "handles.tsv", "marks.tsv", "meta.txt", "sdu.log"];
        assert_eq!(files_of(&run), files);
        assert_att_ran(&pods, &run, &[&dictation::START, &dictation::STOP]);
        assert_control_logged(&pods, &run);
        let meta = read(&run, "meta.txt");
        assert!(
            meta.starts_with("script=both\n") && meta.contains("\nroute=both\n"),
            "{meta}"
        );
    }

    #[test]
    fn an_att_map_capture_dials_the_attribute_channel_alone_and_writes_the_table() {
        let home = home_with_device();
        let (att, att_far) = hand_over();
        let command = capture(
            &home,
            "map",
            &["--route", "dictation", "--script", "att-map"],
        );
        let running = Running::start(command, vec![route(ATT_PSM, att)]);
        let never = Arc::new(AtomicBool::new(false));
        let played = pods::spawn_on(None, Some(far_end(&att_far)), Pods::default(), never);
        let ran = running.finish();
        let pods = transcript(&played);

        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        assert_eq!(ran.stdout, "");
        let pdus_in = pods.att_sent.len();
        assert_eq!(
            ran.stderr,
            format!(
                "xtask probe: capture complete: packets in 0, with audio 0, not logged 0, START sent 0, STOP sent 0, attribute packets in {pdus_in}, with audio 0, dictation start sent 0, dictation stop sent 0, requests refused 0\n"
            )
        );
        assert_eq!(ran.dials, [(ATT_PSM, Outcome::Accepted)]);
        assert_eq!(pods.got(), Vec::<&[u8]>::new());
        let run = run_path(&home, "map");
        let files = ["att.log", "handles.tsv", "marks.tsv", "meta.txt", "sdu.log"];
        assert_eq!(files_of(&run), files);
        let declarations =
            att::read_by_type_request(FIRST_HANDLE, LAST_HANDLE, CHARACTERISTIC_DECLARATION);
        assert_att_ran(&pods, &run, &[&declarations]);
        let listed = read(&run, "handles.tsv");
        let rows: Vec<&str> = listed
            .lines()
            .filter(|row| row.starts_with("declaration\t"))
            .collect();
        let kind = u16::from_le_bytes(MADE_UP_TYPE);
        let declared: Vec<String> = DECLARED
            .into_iter()
            .map(|(value, properties)| {
                let at = declared_at(value);
                format!("declaration\t{at:#06x}\t{properties:#04x}\t{value:#06x}\t{kind:#06x}")
            })
            .collect();
        assert_eq!(rows, declared, "{listed}");
        assert_eq!(read(&run, "sdu.log"), "");
        let marks = read(&run, "marks.tsv");
        assert!(marks.contains("\thandles\tpresent\n"), "{marks}");
    }

    #[test]
    fn stop_sends_the_stop_of_the_route_it_is_given() {
        let home = home_with_device();
        let (control, control_far) = hand_over();
        let command = probe(home.path(), &["stop"]);
        let running = Running::start(command, vec![route(CONTROL_PSM, control)]);
        let played = pods::spawn(far_end(&control_far), Pods::default(), Arc::default());
        let ran = running.finish();
        let pods = transcript(&played);
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        assert_eq!(
            ran.stderr,
            "xtask probe: STOP was sent and the channel was kept open for 1000 ms\n"
        );
        assert_eq!(ran.stdout, "");
        assert_eq!(ran.dials, [(CONTROL_PSM, Outcome::Accepted)]);
        assert_eq!(pods.got(), [&HANDSHAKE_SENT[..], &[&STOP]].concat());

        let (control, control_far) = hand_over();
        let (att, att_far) = hand_over();
        let command = probe(home.path(), &["stop", "--route", "dictation"]);
        let routes = vec![route(CONTROL_PSM, control), route(ATT_PSM, att)];
        let running = Running::start(command, routes);
        let played = pods::spawn_dialled(
            Some(far_end(&control_far)),
            att_far,
            Pods::default(),
            Arc::default(),
        );
        let ran = running.finish();
        let pods = transcript(&played);
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        assert_eq!(
            ran.stderr,
            "xtask probe: the dictation stop and the idle session marker were sent and the channels were kept open for 1000 ms\n"
        );
        assert_eq!(ran.stdout, "");
        let accepted = [
            (CONTROL_PSM, Outcome::Accepted),
            (ATT_PSM, Outcome::Accepted),
        ];
        assert_eq!(ran.dials, accepted);
        assert_eq!(pods.got(), [&HANDSHAKE_SENT[..], &[&SESSION_IDLE]].concat());
        assert_eq!(pods.att_got, [dictation::STOP]);
        // A stop keeps nothing: the session of init is still empty.
        let session = home.path().join(ROOT_NAME).join(STAMP);
        assert_eq!(fs::read_dir(session).unwrap().count(), 0);
    }

    #[test]
    fn a_refused_connect_is_named_and_starts_no_stream() {
        let home = home_with_device();
        let command = capture(&home, "page", &["--script", "soak", "--minutes", "1"]);
        let running = Running::start(command, vec![route(CONTROL_PSM, refuse(Errno::HOSTDOWN))]);
        let ran = running.finish();
        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        assert_eq!(
            ran.stderr,
            "xtask probe: could not connect the L2CAP channel: Host is down (os error 112)\n"
        );
        assert_eq!(ran.stdout, "");
        assert_eq!(
            ran.dials,
            [(CONTROL_PSM, Outcome::Refused(Errno::HOSTDOWN))]
        );
        let run = run_path(&home, "page");
        assert_eq!(files_of(&run), ["marks.tsv", "meta.txt", "sdu.log"]);
        assert_eq!(read(&run, "sdu.log"), "");
        let marks = read(&run, "marks.tsv");
        assert_eq!(kinds(&marks), ["end"], "{marks}");

        let (control, control_far) = hand_over();
        let script = ["--route", "dictation", "--script", "soak", "--minutes", "1"];
        let command = capture(&home, "refused", &script);
        let routes = vec![
            route(CONTROL_PSM, control),
            route(ATT_PSM, refuse(Errno::CONNREFUSED)),
        ];
        let running = Running::start(command, routes);
        let played = pods::spawn(far_end(&control_far), Pods::default(), Arc::default());
        let ran = running.finish();
        let pods = transcript(&played);
        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        let said = "xtask probe: capture ended early: the attribute channel on PSM 0x001f did not open: could not connect the L2CAP channel: Connection refused (os error 111) (packets in ";
        assert!(ran.stderr.starts_with(said), "{}", ran.stderr);
        assert_eq!(ran.stderr.lines().count(), 1, "{}", ran.stderr);
        assert_eq!(ran.stdout, "");
        let dials = [
            (CONTROL_PSM, Outcome::Accepted),
            (ATT_PSM, Outcome::Refused(Errno::CONNREFUSED)),
        ];
        assert_eq!(ran.dials, dials);
        assert_eq!(pods.got(), HANDSHAKE_SENT);
        let run = run_path(&home, "refused");
        let files = ["att.log", "handles.tsv", "marks.tsv", "meta.txt", "sdu.log"];
        assert_eq!(files_of(&run), files);
        assert_eq!(read(&run, "att.log"), "");
        assert_eq!(read(&run, "handles.tsv"), "");
        let marks = read(&run, "marks.tsv");
        assert!(!kinds(&marks).contains(&"active"), "{marks}");
        assert!(
            marks.ends_with("Connection refused (os error 111)\n"),
            "{marks}"
        );
    }
}
