//! The built capture commands under the socket trap against the fake pods, in a store under a temporary `HOME`, and a dictation soak captured and analysed that way.

use std::fmt::Write as _;
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

use pods_proto::dictation::DictationPacket;
use pods_proto::doap::Handles;
use pods_proto::packets::{
    ATT_PSM, CONTROL_PSM, HANDSHAKE, REQUEST_NOTIFICATIONS, SET_SPECIFIC_FEATURES,
};
use pods_proto::sdulog::{self, Direction};
use pods_sys::l2cap::Channel;
use pods_trap::{Answer, Outcome, Route, Trap};
use rustix::process::{Pid, Signal, kill_process};
use tempfile::TempDir;

use super::att_pods::{AttPods, FIND_INFORMATION, TABLE_END};
use super::pods::{self, DEADLINE, Pods, Transcript};

/// A stamp of the shape `date -u +%Y%m%dT%H%M%SZ` prints.
pub(crate) const STAMP: &str = "20261121T120000Z";
/// The synthetic device of the device file, which no output of the command may name.
pub(crate) const DEVICE: &str = "02:00:00:00:00:01";
/// The store under the temporary home.
pub(crate) const ROOT_NAME: &str = "omapods-captures";
/// The mode the store gives a run directory.
const OWNER_DIR: u32 = 0o700;
/// The mode of a file of the caller's alone: every file the store makes, and every step file the tests write.
pub(crate) const OWNER_FILE: u32 = 0o600;
const PERMISSION_BITS: u32 = 0o777;
/// The status a command exits with when the script did not run to its end.
pub(crate) const FAILED: Option<i32> = Some(1);
/// A command still running this long after the test began to wait has hung, and the test fails.
pub(crate) const HANG_LIMIT: Duration = Duration::from_secs(30);
/// The steady packets of a short capture, a fifth of a second of the unpaced stream.
pub(crate) const A_FEW: u32 = 100;
const POLL: Duration = Duration::from_millis(5);

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
pub(crate) fn probe(home: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command.arg("probe").args(args).env("HOME", home);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// A temporary home whose store, made by the built `probe init`, holds the session and the device file.
pub(crate) fn home_with_device() -> TempDir {
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

pub(crate) fn run_path(home: &TempDir, name: &str) -> PathBuf {
    home.path().join(ROOT_NAME).join(STAMP).join(name)
}

/// The capture command for the run `name`, then the route and script arguments.
pub(crate) fn capture(home: &TempDir, name: &str, script: &[&str]) -> Command {
    let run = ["capture", "--stamp", STAMP, "--run", name];
    probe(home.path(), &[&run[..], script].concat())
}

/// Answers a dial by handing its far end to the receiver it returns.
pub(crate) fn hand_over() -> (Answer, mpsc::Receiver<Channel>) {
    hand_over_after(Duration::ZERO)
}

/// The same answer, given only once the dial has waited `after`.
pub(crate) fn hand_over_after(after: Duration) -> (Answer, mpsc::Receiver<Channel>) {
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

pub(crate) fn route(psm: u16, answer: Answer) -> Route {
    Route {
        device: DEVICE.parse().unwrap(),
        psm,
        answer,
    }
}

pub(crate) fn far_end(handed: &mpsc::Receiver<Channel>) -> Channel {
    handed
        .recv_timeout(HANG_LIMIT)
        .expect("the dial never came")
}

/// What a finished command did: its exit code, what it printed, and every dial with its PSM and outcome.
pub(crate) struct Ran {
    pub(crate) code: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) dials: Vec<(u16, Outcome)>,
}

/// The built command under the trap; dropping it kills and reaps the command, so a failed assertion leaves no process behind.
pub(crate) struct Running {
    child: Child,
    trap: Option<Trap>,
    stdout_closed: bool,
}

impl Running {
    pub(crate) fn start(command: Command, routes: Vec<Route>) -> Self {
        let (child, trap) =
            pods_trap::spawn(command, routes).unwrap_or_else(|error| panic!("{error}"));
        Self {
            child,
            trap: Some(trap),
            stdout_closed: false,
        }
    }

    /// Sends SIGINT once the fake pods raise `flag`, as the owner pressing Ctrl+C mid stream; the test fails once `limit` passes.
    pub(crate) fn interrupt_within(&self, limit: Duration, flag: &AtomicBool) {
        let raised = || flag.load(Ordering::SeqCst).then_some(());
        wait_within(limit, "raised flag", raised);
        kill_process(Pid::from_child(&self.child), Signal::INT).unwrap();
    }

    /// Writes one line to the command's standard input, which the command must have been given as a pipe.
    pub(crate) fn type_line(&mut self) {
        let stdin = self.child.stdin.as_mut();
        let stdin = stdin.expect("the command was started with no pipe on its standard input");
        stdin.write_all(b"\n").unwrap();
    }

    /// Closes the test's end of the command's standard output, as a reader that went away does.
    pub(crate) fn close_stdout(&mut self) {
        drop(self.child.stdout.take());
        self.stdout_closed = true;
    }

    pub(crate) fn finish(mut self) -> Ran {
        let status = wait_within(HANG_LIMIT, "exit", || self.child.try_wait().unwrap());
        let stdout = if self.stdout_closed {
            String::new()
        } else {
            text_of(self.child.stdout.take())
        };
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
pub(crate) fn transcript(played: &mpsc::Receiver<Transcript>) -> Transcript {
    played
        .recv_timeout(DEADLINE)
        .expect("the fake pods did not finish before the deadline")
}

/// The names of the files of a run, sorted, after checking the run and each file are the owner's alone and no file names the device.
pub(crate) fn files_of(run: &Path) -> Vec<String> {
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

/// Waits until the marks file of `run` holds `shown` cue marks, so the command is known to wait at its cue, and returns the stamp of the last of them.
pub(crate) fn wait_for_cue(run: &Path, shown: usize) -> u64 {
    wait_within(HANG_LIMIT, "cue", || {
        let marks = fs::read_to_string(run.join("marks.tsv")).unwrap_or_default();
        let cues: Vec<u64> = whole_lines(&marks)
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let stamp = fields.next()?.parse().ok()?;
                (fields.next() == Some("cue")).then_some(stamp)
            })
            .collect();
        (cues.len() >= shown)
            .then(|| cues.last().copied())
            .flatten()
    })
}

/// Waits until the packet log of `run` holds `count` packets from the pods stamped after `after_ns`, and returns the stamp of the last of them.
pub(crate) fn wait_for_packets_after(run: &Path, after_ns: u64, count: usize) -> u64 {
    wait_within(HANG_LIMIT, "packets from the pods", || {
        let log = fs::read_to_string(run.join("sdu.log")).unwrap_or_default();
        let records = whole_lines(&log).map(|line| sdulog::parse_line(line).unwrap());
        let later: Vec<u64> = records
            .filter(|record| record.direction == Direction::FromPods)
            .map(|record| record.at_ns)
            .filter(|at_ns| *at_ns > after_ns)
            .collect();
        later.get(count.checked_sub(1)?).copied()
    })
}

/// The lines of a file a running command is still writing, without a last line whose end has not been written yet.
// Sample input: `12\tgate\tear\n34\tcue\tre`, which holds one whole line.
fn whole_lines(text: &str) -> impl Iterator<Item = &str> {
    let lines = text.split_inclusive('\n');
    lines.filter_map(|line| line.strip_suffix('\n'))
}

fn text_of(stream: Option<impl Read>) -> String {
    let mut text = String::new();
    stream.unwrap().read_to_string(&mut text).unwrap();
    text
}

pub(crate) fn read(run: &Path, name: &str) -> String {
    fs::read_to_string(run.join(name)).unwrap()
}

/// The packets of a channel log that went one way, in order.
// Sample input: `1203456789 < 0400040058000100`, a stamp in nanoseconds, the direction and the packet in hex.
pub(crate) fn logged(log: &str, direction: Direction) -> Vec<Vec<u8>> {
    let records = log.lines().map(|line| sdulog::parse_line(line).unwrap());
    records
        .filter(|record| record.direction == direction)
        .map(|record| record.sdu)
        .collect()
}

/// The kind of every mark: the second field of `1203456789\tgate\tear`.
pub(crate) fn kinds(marks: &str) -> Vec<&str> {
    marks
        .lines()
        .map(|line| line.split('\t').nth(1).unwrap())
        .collect()
}

/// The stamp of the only mark of one kind: the first field of `1203456789\tgate\tear`, in nanoseconds.
pub(crate) fn stamp_of(marks: &str, kind: &str) -> u64 {
    let of_kind = |line: &&str| line.split('\t').nth(1) == Some(kind);
    let found: Vec<&str> = marks.lines().filter(of_kind).collect();
    assert_eq!(found.len(), 1, "{kind} marks in\n{marks}");
    found[0].split('\t').next().unwrap().parse().unwrap()
}

/// Asserts the walk of the attribute table and then `writes` reached the fake pods, and that the attribute log holds both directions as they went.
pub(crate) fn assert_att_ran(pods: &Transcript, run: &Path, writes: &[&[u8]]) {
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
pub(crate) fn assert_control_logged(pods: &Transcript, run: &Path) {
    let log = read(run, "sdu.log");
    assert_eq!(logged(&log, Direction::ToPods), pods.got());
    assert_eq!(logged(&log, Direction::FromPods), pods.sent);
}

/// A dictation soak captured from fake pods playing `att` until they raise their flag, then analysed by the built command.
pub(crate) struct Analysed {
    pub(crate) home: TempDir,
    pub(crate) run: PathBuf,
    pods: Transcript,
    /// The rows of `analysis-att.tsv`, each `key\tvalue\tunit\tnote`.
    pub(crate) rows: Vec<Vec<String>>,
}

impl Analysed {
    pub(crate) fn of(att: AttPods, limit: Duration) -> Self {
        let home = home_with_device();
        let (control, control_far) = hand_over();
        let (dialled, att_far) = hand_over();
        let script = ["--route", "dictation", "--script", "soak", "--minutes", "2"];
        let command = capture(&home, "dictation", &script);
        let routes = vec![route(CONTROL_PSM, control), route(ATT_PSM, dialled)];
        let running = Running::start(command, routes);
        let flag = Arc::new(AtomicBool::new(false));
        let part = Pods {
            att,
            deadline: limit,
            ..Pods::default()
        };
        let control_far = Some(far_end(&control_far));
        let played = pods::spawn_dialled(control_far, att_far, part, Arc::clone(&flag));
        running.interrupt_within(limit, &flag);
        let ran = running.finish();
        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        let pods = transcript(&played);
        let run = run_path(&home, "dictation");
        let mut analyse = probe(home.path(), &["analyse", run.to_str().unwrap()]);
        let analysed = analyse.output().unwrap();
        let marks = read(&run, "marks.tsv");
        assert!(analysed.status.success(), "{analysed:?}\n{marks}");
        let table = read(&run, "analysis-att.tsv");
        let printed = String::from_utf8(analysed.stdout).unwrap();
        assert!(printed.ends_with(&table), "{printed}");
        let fields = |line: &str| line.split('\t').map(str::to_owned).collect();
        Self {
            home,
            run,
            pods,
            rows: table.lines().map(fields).collect(),
        }
    }

    pub(crate) fn value(&self, key: &str) -> &str {
        let mut found = self.rows.iter().filter(|row| row[0] == key);
        let row = found.next().unwrap_or_else(|| panic!("no row {key}"));
        assert!(found.next().is_none(), "two rows {key}");
        &row[1]
    }

    pub(crate) fn number(&self, key: &str) -> f64 {
        self.value(key).parse().unwrap()
    }

    /// How many frames the fake pods sent, in every audio notification.
    pub(crate) fn frames_sent(&self) -> usize {
        let sent = self.pods.att_sent.iter();
        sent.map(
            |pdu| match DictationPacket::parse(pdu, &Handles::OBSERVED) {
                Ok(Some(DictationPacket::Audio(audio))) => audio.frames().count(),
                _ => 0,
            },
        )
        .sum()
    }
}

/// The three packets of the handshake, in the order the command sends them.
pub(crate) const HANDSHAKE_SENT: [&[u8]; 3] =
    [&HANDSHAKE, &SET_SPECIFIC_FEATURES, &REQUEST_NOTIFICATIONS];

/// Writes `contents` to a new file of `mode` in `dir` and returns its path; the mode is set after the umask has had its say.
pub(crate) fn plan(dir: &Path, name: &str, contents: &[u8], mode: u32) -> PathBuf {
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

pub(crate) fn steps_args(path: &Path) -> [&str; 4] {
    ["--route", "dictation", "--steps", path.to_str().unwrap()]
}

/// Runs the capture with `args` into run `name` against fake pods playing `part`.
pub(crate) fn run_steps(
    home: &TempDir,
    name: &str,
    args: &[&str],
    part: Pods,
) -> (Ran, Transcript) {
    let (control, control_far) = hand_over();
    let (att, att_far) = hand_over();
    let command = capture(home, name, args);
    let routes = vec![route(CONTROL_PSM, control), route(ATT_PSM, att)];
    let running = Running::start(command, routes);
    let flag = Arc::new(AtomicBool::new(false));
    let played = pods::spawn_dialled(Some(far_end(&control_far)), att_far, part, flag);
    let ran = running.finish();
    (ran, transcript(&played))
}

/// The step that sends `sdu` on the control channel, as the line `control 0400040017000000`.
pub(crate) fn control_step(sdu: &[u8]) -> String {
    let mut line = String::from("control ");
    for byte in sdu {
        write!(line, "{byte:02x}").unwrap();
    }
    line.push('\n');
    line
}

pub(crate) fn after_handshake(pods: &Transcript) -> Vec<Vec<u8>> {
    let got = pods.got();
    let (handshake, rest) = got.split_at(HANDSHAKE_SENT.len());
    assert_eq!(handshake, HANDSHAKE_SENT);
    rest.iter().map(|sdu| sdu.to_vec()).collect()
}

/// The stamp and the text of every step mark, each line `1203456789\tstep\tline 2 read 0x0024`.
pub(crate) fn step_marks(marks: &str) -> Vec<(u64, &str)> {
    let fields = marks.lines().map(|line| {
        let [stamp, kind, text] = line.splitn(3, '\t').collect::<Vec<_>>()[..] else {
            panic!("{line}");
        };
        (stamp.parse().unwrap(), kind, text)
    });
    let steps = fields.filter(|(_, kind, _)| *kind == "step");
    steps.map(|(stamp, _, text)| (stamp, text)).collect()
}

/// Runs the command with `args` under the trap, which takes any dial, and returns what it said after asserting it dialled nothing and made no run.
pub(crate) fn refusal_of(home: &TempDir, args: &[&str]) -> String {
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
