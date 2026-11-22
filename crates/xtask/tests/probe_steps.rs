//! The built binary's `probe capture --steps` against the fake pods, every L2CAP connect of the command taken by the socket trap.

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
    reason = "the analysed run and the cut helpers serve the other capture tests"
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
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use pods_proto::att;
    use pods_proto::dictation::{self, SESSION_ACTIVE, SESSION_IDLE};
    use pods_proto::packets::{
        ATT_PSM, CONTROL_PSM, HANDSHAKE, REQUEST_NOTIFICATIONS, SET_SPECIFIC_FEATURES,
    };
    use pods_proto::sdulog::Direction;
    use pods_trap::Outcome;
    use tempfile::TempDir;

    use super::att_pods::{
        READ_NOT_PERMITTED, READ_RESPONSE, READ_VALUE, WRITE_NOT_PERMITTED, WRITE_RESPONSE,
    };
    use super::capture::{
        FAILED, Ran, Running, assert_att_ran, assert_control_logged, capture, far_end, files_of,
        hand_over, home_with_device, kinds, logged, read, route, run_path, stamp_of, transcript,
    };
    use super::pods::{self, Pods, Transcript};

    /// The three packets of the handshake, in the order the command sends them.
    const HANDSHAKE_SENT: [&[u8]; 3] = [&HANDSHAKE, &SET_SPECIFIC_FEATURES, &REQUEST_NOTIFICATIONS];
    /// One step of each kind, with a comment, a blank line and a wait between each two.
    const EVERY_KIND: &str = "# one step of each kind\nwrite-request 0x0019 0100\nwait 100\nread 0x0024\n\nwait 100\nwrite-command 0x0028 31\nwait 100\ncontrol 0400040017000000\nwait 300\n";
    /// What each step of `EVERY_KIND` is marked with, and its offset from the gate in milliseconds.
    const EVERY_KIND_MARKS: [(&str, u64); 4] = [
        ("line 2 write-request 0x0019", 0),
        ("line 4 read 0x0024", 100),
        ("line 7 write-command 0x0028", 200),
        ("line 9 control", 300),
    ];
    /// When `EVERY_KIND` ends, after its last wait, in milliseconds from the gate.
    const EVERY_KIND_END_MS: u64 = 600;
    /// The attribute channel packets of `EVERY_KIND`, as they leave.
    const EVERY_KIND_ATT: [&[u8]; 3] = [
        &[att::WRITE_REQUEST, 0x19, 0x00, 0x01, 0x00],
        &[att::READ_REQUEST, 0x24, 0x00],
        &[att::WRITE_COMMAND, 0x28, 0x00, 0x31],
    ];
    /// The control channel packet of `EVERY_KIND`, an opcode no guard covers.
    const EVERY_KIND_CONTROL: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x17, 0x00, 0x00, 0x00];
    /// The two handles `EVERY_KIND` writes with a response and reads.
    const ASKED: [u16; 2] = [0x0019, 0x0024];
    /// The hex of the step values, none of which the terminal may show.
    const STEP_HEX: [&str; 3] = ["0400040017", "5aa5", "0x0019"];
    /// How late a step may fire behind its offset on a loaded host; the loop wakes at least every 10 ms.
    const LATE_LIMIT: Duration = Duration::from_millis(200);
    const NS_PER_MS: u64 = 1_000_000;
    /// The steps the guards cover, with neither stop: the marker, then the start write once the marker has led it.
    const STARTS_ONLY: &str =
        "control 040004004b000300030103\nwait 480\nwrite-command 0x0007 0100\nwait 200\n";
    /// The longest value a step sends.
    const MAX_VALUE_BYTES: usize = 512;
    /// The most lines a step file holds.
    const MAX_LINES: usize = 256;
    /// The longest wait, and how many of them make up the longest run.
    const MAX_WAIT_MS: u32 = 60_000;
    const MAX_WAITS: usize = 90;
    /// The step file's mode: the caller's alone, as the store reads every file from outside it.
    const OWNER_FILE: u32 = 0o600;
    /// A mode that lets the group read.
    const GROUP_READABLE: u32 = 0o640;
    /// A mode that lets others read.
    const OTHERS_READABLE: u32 = 0o604;
    /// When the start write of `STARTS_ONLY` leaves, in milliseconds from the gate.
    const STARTS_ONLY_START_MS: u64 = 480;

    /// Writes `contents` to a new file of `mode` in `dir` and returns its path; the mode is set after the umask has had its say.
    fn plan(dir: &Path, name: &str, contents: &[u8], mode: u32) -> PathBuf {
        let path = dir.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(contents).unwrap();
        file.set_permissions(fs::Permissions::from_mode(mode))
            .unwrap();
        path
    }

    fn steps_args(path: &Path) -> [&str; 4] {
        ["--route", "dictation", "--steps", path.to_str().unwrap()]
    }

    /// Runs the step file at `path` into run `name` against fake pods playing `part`.
    fn run_steps(home: &TempDir, name: &str, path: &Path, part: Pods) -> (Ran, Transcript) {
        let (control, control_far) = hand_over();
        let (att, att_far) = hand_over();
        let command = capture(home, name, &steps_args(path));
        let routes = vec![route(CONTROL_PSM, control), route(ATT_PSM, att)];
        let running = Running::start(command, routes);
        let flag = Arc::new(AtomicBool::new(false));
        let played = pods::spawn_dialled(Some(far_end(&control_far)), att_far, part, flag);
        let ran = running.finish();
        (ran, transcript(&played))
    }

    /// The stamp and the text of every step mark, each line `1203456789\tstep\tline 2 read 0x0024`.
    fn step_marks(marks: &str) -> Vec<(u64, &str)> {
        let fields = marks.lines().map(|line| {
            let [stamp, kind, text] = line.splitn(3, '\t').collect::<Vec<_>>()[..] else {
                panic!("{line}");
            };
            (stamp.parse().unwrap(), kind, text)
        });
        let steps = fields.filter(|(_, kind, _)| *kind == "step");
        steps.map(|(stamp, _, text)| (stamp, text)).collect()
    }

    /// Asserts the mark stamped `at_ns` is not before `due_ns` and not more than `LATE_LIMIT` after it.
    fn assert_on_time(at_ns: u64, due_ns: u64, what: &str, marks: &str) {
        let Some(late_ns) = at_ns.checked_sub(due_ns) else {
            panic!("{what} is early\n{marks}");
        };
        let late = Duration::from_nanos(late_ns);
        assert!(late <= LATE_LIMIT, "{what} is {late:?} late\n{marks}");
    }

    /// Asserts the run ran to its end and said so in one line that shows no packet.
    fn assert_completed(ran: &Ran) {
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        assert_eq!(ran.stdout, "");
        assert!(
            ran.stderr.starts_with("xtask probe: capture complete: ")
                && ran.stderr.lines().count() == 1,
            "{}",
            ran.stderr
        );
        for hex in STEP_HEX {
            assert!(!ran.stderr.contains(hex), "{}", ran.stderr);
        }
        let accepted = [
            (CONTROL_PSM, Outcome::Accepted),
            (ATT_PSM, Outcome::Accepted),
        ];
        assert_eq!(ran.dials, accepted);
    }

    #[test]
    fn a_step_file_sends_each_step_in_order_at_its_offset_and_records_the_answers() {
        let home = home_with_device();
        let path = plan(home.path(), "plan", EVERY_KIND.as_bytes(), OWNER_FILE);
        let (ran, pods) = run_steps(&home, "steps", &path, Pods::default());

        assert_completed(&ran);
        assert_eq!(pods.att_got, EVERY_KIND_ATT);
        let answers = [
            vec![WRITE_RESPONSE],
            [&[READ_RESPONSE][..], &READ_VALUE].concat(),
        ];
        assert_eq!(pods.att_sent, answers);
        assert_eq!(
            pods.got(),
            [&HANDSHAKE_SENT[..], &[&EVERY_KIND_CONTROL]].concat()
        );
        let run = run_path(&home, "steps");
        assert_eq!(
            files_of(&run),
            ["att.log", "marks.tsv", "meta.txt", "sdu.log", "steps.txt"]
        );
        let att_log = read(&run, "att.log");
        assert_eq!(logged(&att_log, Direction::ToPods), pods.att_got);
        assert_eq!(logged(&att_log, Direction::FromPods), pods.att_sent);
        assert_control_logged(&pods, &run);
        let marks = read(&run, "marks.tsv");
        let gate_ns = stamp_of(&marks, "gate");
        let steps = step_marks(&marks);
        let texts: Vec<&str> = steps.iter().map(|(_, text)| *text).collect();
        assert_eq!(texts, EVERY_KIND_MARKS.map(|(text, _)| text), "{marks}");
        for ((at_ns, text), (_, offset_ms)) in steps.iter().zip(EVERY_KIND_MARKS) {
            assert_on_time(*at_ns, gate_ns + offset_ms * NS_PER_MS, text, &marks);
        }
        let end_due_ns = gate_ns + EVERY_KIND_END_MS * NS_PER_MS;
        assert_on_time(stamp_of(&marks, "end"), end_due_ns, "end", &marks);
        assert!(
            marks.ends_with("\tend\tthe script ran to its end\n"),
            "{marks}"
        );
        let meta = read(&run, "meta.txt");
        assert!(meta.starts_with("script=steps\n"), "{meta}");
        assert!(
            meta.ends_with("\nroute=dictation\nsteps=steps.txt\nunchecked=false\n"),
            "{meta}"
        );
        assert_eq!(read(&run, "steps.txt"), EVERY_KIND);
    }

    #[test]
    fn an_error_response_to_a_write_and_a_read_is_recorded_and_the_steps_go_on() {
        let home = home_with_device();
        let path = plan(home.path(), "plan", EVERY_KIND.as_bytes(), OWNER_FILE);
        let mut part = Pods::default();
        part.att.refused = ASKED.to_vec();
        let (ran, pods) = run_steps(&home, "refused", &path, part);

        assert_completed(&ran);
        assert_eq!(pods.att_got, EVERY_KIND_ATT);
        let refusals = [
            att::error_response(att::WRITE_REQUEST, ASKED[0], WRITE_NOT_PERMITTED).to_vec(),
            att::error_response(att::READ_REQUEST, ASKED[1], READ_NOT_PERMITTED).to_vec(),
        ];
        assert_eq!(pods.att_sent, refusals);
        let run = run_path(&home, "refused");
        let att_log = read(&run, "att.log");
        assert_eq!(logged(&att_log, Direction::FromPods), refusals);
        let marks = read(&run, "marks.tsv");
        assert_eq!(step_marks(&marks).len(), EVERY_KIND_MARKS.len(), "{marks}");
    }

    #[test]
    fn a_start_in_a_step_file_is_checked_and_stopped_by_the_run() {
        let home = home_with_device();
        let path = plan(home.path(), "plan", STARTS_ONLY.as_bytes(), OWNER_FILE);
        let (ran, pods) = run_steps(&home, "guarded", &path, Pods::default());

        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        let counted = ", dictation start sent 1, dictation stop sent 1, requests refused 0\n";
        assert!(ran.stderr.ends_with(counted), "{}", ran.stderr);
        let control = [&HANDSHAKE_SENT[..], &[&SESSION_ACTIVE, &SESSION_IDLE]].concat();
        assert_eq!(pods.got(), control);
        let run = run_path(&home, "guarded");
        let files = [
            "att.log",
            "handles.tsv",
            "marks.tsv",
            "meta.txt",
            "sdu.log",
            "steps.txt",
        ];
        assert_eq!(files_of(&run), files);
        assert_att_ran(&pods, &run, &[&dictation::START, &dictation::STOP]);
        let marks = read(&run, "marks.tsv");
        let order = ["handles", "gate", "active", "dictation-start", "started"];
        let seen: Vec<&str> = kinds(&marks)
            .into_iter()
            .filter(|kind| order.contains(kind))
            .collect();
        assert_eq!(seen, order, "{marks}");
        let after_end: Vec<&str> = kinds(&marks)
            .into_iter()
            .skip_while(|kind| *kind != "dictation-stop")
            .collect();
        assert_eq!(after_end[..2], ["dictation-stop", "idle"], "{marks}");
        assert!(step_marks(&marks).is_empty(), "{marks}");
        let start_due_ns = stamp_of(&marks, "gate") + STARTS_ONLY_START_MS * NS_PER_MS;
        let start_ns = stamp_of(&marks, "dictation-start");
        assert_on_time(start_ns, start_due_ns, "dictation-start", &marks);
        let meta = read(&run, "meta.txt");
        assert!(meta.starts_with("script=steps\n"), "{meta}");
    }

    /// A step file the command must refuse whole, the line it names and what it says of that line.
    struct Refused {
        contents: Vec<u8>,
        line: usize,
        fault: String,
    }

    fn refused(contents: impl Into<Vec<u8>>, line: usize, fault: &str) -> Refused {
        let (contents, fault) = (contents.into(), fault.to_owned());
        Refused {
            contents,
            line,
            fault,
        }
    }

    /// One file for each way a line is refused.
    fn refused_files() -> Vec<Refused> {
        let too_long = format!(
            "write-command 0x0019 {}\n",
            "00".repeat(MAX_VALUE_BYTES + 1)
        );
        let longest_value = format!("write-command 0x0019 {}\n", "00".repeat(MAX_VALUE_BYTES));
        let longest_run = format!("wait {MAX_WAIT_MS}\n").repeat(MAX_WAITS + 1);
        let too_many = "#\n".repeat(MAX_LINES + 1);
        vec![
            refused(
                "# plan\nsend 0x0019 0100\n",
                2,
                "the line starts with no step; the steps are write-request, write-command, read, control and wait, and a comment starts with #",
            ),
            refused(
                "write-request 0x0019 01g0\n",
                1,
                "byte 1 of the hex, counting from 0, is not two lowercase hex digits",
            ),
            refused(
                "wait 10\ncontrol 040\n",
                2,
                "byte 1 of the hex, counting from 0, is not two lowercase hex digits",
            ),
            refused(
                "write-command 0x0019 0A\n",
                1,
                "byte 0 of the hex, counting from 0, is not two lowercase hex digits",
            ),
            refused(
                "control +1000400\n",
                1,
                "byte 0 of the hex, counting from 0, is not two lowercase hex digits",
            ),
            refused(
                format!("{longest_value}{too_long}"),
                2,
                "the value is 513 bytes, and a step sends at most 512",
            ),
            refused(
                "read 0x0000\n",
                1,
                "handle 0x0000 is reserved; handles run from 0x0001 to 0xffff",
            ),
            refused(
                "read 0x19\n",
                1,
                "the handle is not 0x and 4 lowercase hex digits",
            ),
            refused(
                "read 0x001E\n",
                1,
                "the handle is not 0x and 4 lowercase hex digits",
            ),
            refused(
                "read 0x+019\n",
                1,
                "the handle is not 0x and 4 lowercase hex digits",
            ),
            refused(
                "read 0x0019 00\n",
                1,
                "read takes a handle and nothing more",
            ),
            refused(
                format!("wait {MAX_WAIT_MS}\nwait 60001\n"),
                2,
                "the wait is longer than 60000 ms",
            ),
            refused(
                "wait +5\n",
                1,
                "the wait is not a whole number of milliseconds",
            ),
            refused(
                longest_run,
                MAX_WAITS + 1,
                "the waits up to this line pass 5400000 ms in all",
            ),
            refused(too_many, MAX_LINES + 1, "the file has more than 256 lines"),
            refused(b"wait 10\n\xff\n".to_vec(), 2, "the line is not UTF-8 text"),
            refused(
                "write-request 0x0007 0100\n",
                1,
                "a write to handle 0x0007 or 0x000a may only be the stream's own start or stop write, which the run stops itself",
            ),
            refused(
                "control 0400040009000102000000\n",
                1,
                "a control command, opcode 0x0009, changes a device setting the run cannot restore, so no step may send one",
            ),
            refused(
                "control 0400040058000000\n",
                1,
                "a packet with the opcode of the uplink start or stop or of a session marker may only be one of those exact packets, which the run undoes itself",
            ),
        ]
    }

    /// Runs the command with `args` under the trap, which takes any dial, and returns what it said after asserting it dialled nothing and made no run.
    fn refusal_of(home: &TempDir, args: &[&str]) -> String {
        let (control, _) = hand_over();
        let (att, _) = hand_over();
        let routes = vec![route(CONTROL_PSM, control), route(ATT_PSM, att)];
        let running = Running::start(capture(home, "refused", args), routes);
        let ran = running.finish();
        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        assert_eq!(ran.stdout, "");
        assert!(ran.dials.is_empty(), "{:?}", ran.dials);
        assert!(!run_path(home, "refused").exists());
        ran.stderr
    }

    #[test]
    fn a_bad_line_is_named_and_nothing_is_sent() {
        let home = home_with_device();
        let files = tempfile::tempdir().unwrap();
        for (index, case) in refused_files().into_iter().enumerate() {
            let path = plan(
                files.path(),
                &format!("plan-{index}"),
                &case.contents,
                OWNER_FILE,
            );
            let said = refusal_of(&home, &steps_args(&path));
            let expected = format!(
                "xtask probe: {}: line {}: {}\n",
                path.display(),
                case.line,
                case.fault
            );
            assert_eq!(said, expected);
        }
    }

    #[test]
    fn a_step_file_off_the_dictation_route_is_refused_and_nothing_is_sent() {
        let home = home_with_device();
        let path = plan(home.path(), "plan", EVERY_KIND.as_bytes(), OWNER_FILE);
        for route_name in ["uplink", "both"] {
            let args = ["--route", route_name, "--steps", path.to_str().unwrap()];
            let expected = format!(
                "xtask probe: a step file runs on the dictation route, not on {route_name}: pass --route dictation\n"
            );
            assert_eq!(refusal_of(&home, &args), expected);
        }
    }

    #[test]
    fn a_step_file_that_is_no_regular_file_of_the_caller_alone_is_refused() {
        let home = home_with_device();
        let files = tempfile::tempdir().unwrap();
        let target = plan(files.path(), "target", EVERY_KIND.as_bytes(), OWNER_FILE);
        let link = files.path().join("link");
        symlink(&target, &link).unwrap();
        let shared = plan(
            files.path(),
            "shared",
            EVERY_KIND.as_bytes(),
            GROUP_READABLE,
        );
        let open_to_others = plan(
            files.path(),
            "others",
            EVERY_KIND.as_bytes(),
            OTHERS_READABLE,
        );
        let directory = files.path().join("directory");
        fs::create_dir(&directory).unwrap();
        let cases = [
            (link, "is a symbolic link; the store never follows one"),
            (directory, "is not a regular file"),
            (
                shared,
                "has mode 0640; a capture is read only when group and others have no access",
            ),
            (
                open_to_others,
                "has mode 0604; a capture is read only when group and others have no access",
            ),
        ];
        for (path, rule) in cases {
            let said = refusal_of(&home, &steps_args(&path));
            let expected = format!("xtask probe: {} {rule}\n", path.display());
            assert_eq!(said, expected);
        }
    }
}
