//! The stub programs every hw test runs the command against, and the bench that holds them in a temporary directory.

use std::cell::{Cell, RefCell};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use pods_core::link::ladder::Attempt;
use pods_core::link::{Phase, STATE_POPULATED_LINE, phase_change_line};
use pods_core::state::State;
use pods_core::status::to_status_line;
use pods_fake::{Pods, Profile};
use pods_proto::model::Model;
use pods_proto::packets::CONTROL_PSM;
use pods_trap::{Answer, Dial, Route, Trap};
use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process};

/// The synthetic device every stub reports; none of it may reach the command's output.
pub(crate) const ADDRESS: &str = "02:00:00:00:00:01";
const UNDER_TEST: &str = "librepods-hw.service";
const INSTALLED: &str = "librepods.service";
pub(crate) const DEVICE_PATH: &str = "dev_02_00_00_00_00_01";
pub(crate) const DEVICE_NAME: &str = "Synthetic Pods 7";

/// The daemon's own log lines as shell variables, so the scripted daemon writes what the real one does.
fn daemon_lines() -> String {
    let attempt = Attempt::Held(1);
    let opening = Phase::Opening { attempt };
    let handshake = Phase::AwaitHandshakeAck {
        attempt,
        until: Duration::ZERO,
    };
    format!(
        "opening_line='{}'\nhandshake_line='{}'\npopulated_line='{STATE_POPULATED_LINE}'\n",
        phase_change_line(Phase::Idle, opening),
        phase_change_line(opening, handshake),
    )
}

/// How long a half typed line sits at the prompt before the stop signal follows it.
const HALF_LINE_LEAD: Duration = Duration::from_millis(500);

/// Shared by every stub: the call log, the device named on stderr, and the scripted daemon.
pub(crate) const LIB: &str = r#"
log() { printf '%s\n' "$*" >> "$STUB/calls"; }
# Seconds a hang flag holds a logged call open, so the test's signal lands inside it.
hold_seconds=2
# Seconds slow-disconnect holds `do disconnect` open, so the test's signal lands inside it.
slow_disconnect_seconds=1
# Seconds after the watch's first lines that daemon-busy wakes the daemon, inside the 1 s idle window.
busy_wake_seconds=0.3
# Seconds a watch stand-in lives, past every idle window and the run's 5 s wait for its first line.
watch_seconds=30
printf 'device 02:00:00:00:00:01 Synthetic Pods 7\n' >&2
read_file() { cat "$STUB/$1"; }
inside() { case "$1" in "$STUB"/*) return 0 ;; esac; return 1; }
emit() {
    line_no=$(( $(wc -l < "$STUB/journal") + 1 ))
    id=; [ -n "$3" ] && id=",\"MESSAGE_ID\":\"$3\""
    printf '{"__CURSOR":"c%s","__MONOTONIC_TIMESTAMP":"%s","MESSAGE":"%s"%s}\n' "$line_no" "$1" "$2" "$id" >> "$STUB/journal"
}
tick() { c=$(( $(read_file clock) + 1000000 )); echo "$c" > "$STUB/clock"; echo "$c"; }
link_up() {
    [ -n "$no_opening" ] || emit "$1" "$opening_line"
    emit "$1" "link: peer 02:00:00:00:00:01 named Synthetic Pods 7"
    [ -e "$STUB/slow-connect" ] && emit "$(( ($1 + $2) / 2 ))" "$handshake_line"
    emit "$2" "$populated_line"
    echo true > "$STUB/connected"; echo true > "$STUB/bluez-connected"
}
daemon_start() {
    echo active > "$STUB/unit-librepods-hw.service"
    n=$(read_file starts); echo $(( n + 1 )) > "$STUB/starts"
    t=$(tick); emit "$t" "Started librepods-hw.service." 39f53479d3a045ac8e11786248231fbf
    run=$(read_file run-dir); state=$(read_file state-dir)
    inside "$run" && { [ -e "$run/librepods.sock" ] || : > "$run/librepods.sock"; }
    inside "$state" && mkdir -p "$state/librepods" && echo '{}' > "$state/librepods/status.json"
    [ -e "$STUB/pods-absent" ] && return 0
    d=$(( 100000 + 10000 * n ))
    link_up "$(( t + d - 50000 ))" "$(( t + d ))"
}
daemon_stop() {
    [ -e "$STUB/hw-stop-ignored" ] && return 0
    echo inactive > "$STUB/unit-librepods-hw.service"
    # Under pods-leave-after-idle the pods drop at the first stop after the idle check, so the burst check finds none connected.
    [ -e "$STUB/pods-leave-after-idle" ] && [ -e "$STUB/idle-started" ] && echo false > "$STUB/bluez-connected"
    [ -e "$STUB/stop-leaves-files" ] && return 0
    for p in "$(read_file run-dir)/librepods.sock" "$(read_file state-dir)/librepods/status.json"; do
        inside "$p" && rm -f "$p"
    done
}
"#;

pub(crate) const SYSTEMCTL: &str = r#"
log "systemctl $*"
[ "$1" = --user ] || exit 2
shift
case "$1" in
show)
    [ "$3 $4" = "--value librepods-hw.service" ] || exit 1
    case "$2" in
    --property=LoadState)
        [ -e "$STUB/hang-show" ] && sleep "$hold_seconds"
        if [ -e "$STUB/hw-exists" ]; then echo loaded; else echo not-found; fi ;;
    --property=MainPID) [ -e "$STUB/main-pid-fails" ] && exit 1
        : > "$STUB/idle-started"
        [ -e "$STUB/link-down-at-idle" ] && echo false > "$STUB/connected"
        read_file main-pid ;;
    *) exit 1 ;;
    esac ;;
is-active)
    s=$(cat "$STUB/unit-$2" 2>/dev/null || echo inactive)
    if [ "$2 $s" = "librepods-hw.service inactive" ] && [ -e "$STUB/hw-activity-fails" ] && [ ! -e "$STUB/hw-activity-failed" ]; then
        # A word with a failing status, so only the exit status can reject it.
        : > "$STUB/hw-activity-failed"; echo inactive; exit 1
    fi
    echo "$s"
    [ "$s" = active ] && exit 0
    # systemd 257 exits 4 for a unit it does not hold: the collected test unit once inactive, or a missing service.
    { [ "$2" = librepods-hw.service ] || [ -e "$STUB/installed-missing" ]; } && exit 4
    exit 3 ;;
stop)
    [ "$2" = librepods.service ] && [ -e "$STUB/hang-stop" ] && sleep "$hold_seconds"
    if [ "$2" = librepods-hw.service ]; then daemon_stop; else echo inactive > "$STUB/unit-$2"; fi ;;
start)
    [ -e "$STUB/restore-fails" ] && exit 1
    [ -e "$STUB/start-ignored" ] && exit 0
    echo active > "$STUB/unit-$2" ;;
restart)
    [ "$2" = librepods-hw.service ] || exit 1
    emit "$(read_file clock)" "Stopped librepods-hw.service."
    daemon_start ;;
*) exit 1 ;;
esac
"#;

pub(crate) const SYSTEMD_RUN: &str = r#"
log "systemd-run $*"
: > "$STUB/systemd-run.args"
for a; do
    printf '%s\n' "$a" >> "$STUB/systemd-run.args"
    case $a in
    --setenv=XDG_RUNTIME_DIR=*) echo "${a#--setenv=XDG_RUNTIME_DIR=}" > "$STUB/run-dir" ;;
    --setenv=XDG_STATE_HOME=*) echo "${a#--setenv=XDG_STATE_HOME=}" > "$STUB/state-dir" ;;
    esac
done
daemon_start
"#;

pub(crate) const JOURNALCTL: &str = r#"
log "journalctl $*"
last=; after=
for a; do
    case $a in
    --lines=1) last=1 ;;
    --after-cursor=c*) after=${a#--after-cursor=c} ;;
    esac
done
if [ -n "$last" ]; then tail -n 1 "$STUB/journal"
elif [ -n "$after" ]; then [ -e "$STUB/journal-fails" ] && exit 1; tail -n +"$(( after + 1 ))" "$STUB/journal"
else cat "$STUB/journal"; fi
"#;

pub(crate) const BUSCTL: &str = r#"
log "busctl $*"
# Under watch-unstartable the first adapter read of C5 takes the binary's exec bit away for the rest of the run, so the watch cannot start.
if [ -e "$STUB/watch-unstartable" ] && [ -e "$STUB/idle-started" ]; then chmod -x "$STUB/bin/librepods"; fi
printf '{"type":"a{oa{sa{sv}}}","data":[{"/org/bluez/hci0":{"org.bluez.Adapter1":{"Address":{"type":"s","data":"02:00:00:00:00:09"},"Powered":{"type":"b","data":%s},"Discovering":{"type":"b","data":%s}}},"/org/bluez/hci0/dev_02_00_00_00_00_01":{"org.bluez.Device1":{"Address":{"type":"s","data":"02:00:00:00:00:01"},"Name":{"type":"s","data":"Synthetic Pods 7"},"Adapter":{"type":"o","data":"/org/bluez/hci0"},"Connected":{"type":"b","data":%s},"UUIDs":{"type":"as","data":["74ec2172-0bad-4d01-8f77-997b2be0722a"]}}}}]}\n' "$(read_file powered)" "$([ -e "$STUB/discovering" ] && echo true || echo false)" "$(read_file bluez-connected)"
"#;

pub(crate) const BLUETOOTHCTL: &str = r#"
log "bluetoothctl $*"
case "$1 $2" in
"power off") echo false > "$STUB/powered"; echo false > "$STUB/bluez-connected"; echo false > "$STUB/connected"
    [ -e "$STUB/interrupt-at-power-off" ] && kill -INT "$PPID" ;;
"power on")
    if [ -e "$STUB/power-on-fails" ]; then rm -f "$STUB/power-on-fails"; exit 1; fi
    echo true > "$STUB/powered" ;;
*) exit 1 ;;
esac
echo "Changing power $2 succeeded"
"#;

pub(crate) const PACTL: &str = r#"
log "pactl $*"
[ -e "$STUB/hang-pactl" ] && exec sleep 15
[ "$1 $2" = "list cards" ] || exit 1
printf 'Card #52\n\tName: bluez_card.02_00_00_00_00_01\n\tdevice.description = "Synthetic Pods 7"\n\tActive Profile: %s\n' "$(read_file profile)"
"#;

/// The binary under test, and the installed `librepods-ctl`; both answer only on the run's own runtime directory.
pub(crate) const LIBREPODS: &str = r#"
log "$(basename "$0") $*"
[ "$XDG_RUNTIME_DIR" = "$(read_file run-dir)" ] || exit 1
[ "$(read_file unit-librepods-hw.service)" = active ] || exit 1
line() { read_file "status-$(read_file connected).json"; }
case "$1" in
status)
    [ -e "$STUB/hang-status" ] && { echo $$ > "$STUB/status-pid"; exec sleep 10; }
    [ -e "$STUB/status-once" ] && [ -e "$STUB/status-answered" ] && exit 1
    : > "$STUB/status-answered"
    [ "$(basename "$0")" = librepods-ctl ] && [ -e "$STUB/link-drops" ] && echo false > "$STUB/bluez-connected"
    if [ "$(basename "$0")" = librepods-ctl ] && [ -e "$STUB/ctl-differs" ]; then echo '{"other":1}'; else line; fi ;;
watch) echo $$ > "$STUB/watch-pid"
    [ -e "$STUB/watch-silent" ] && exec sleep "$watch_seconds"
    [ -e "$STUB/watch-wakes-daemon" ] && { kill -STOP "$(read_file main-pid)"; kill -CONT "$(read_file main-pid)"; }
    line; line
    [ -e "$STUB/watch-ends" ] && exit 0
    if [ -e "$STUB/daemon-busy" ]; then sleep "$busy_wake_seconds"; kill -STOP "$(read_file main-pid)"; kill -CONT "$(read_file main-pid)"; fi
    exec sleep "$watch_seconds" ;;
do)
    case "$2" in
    disconnect) [ -e "$STUB/disconnect-unseen" ] || echo false > "$STUB/connected"; if [ -e "$STUB/slow-disconnect" ]; then sleep "$slow_disconnect_seconds"; fi ;;
    connect) [ -e "$STUB/connect-fails" ] && exit 1
        [ -e "$STUB/final-connect-fails" ] && [ -e "$STUB/idle-started" ] && exit 1
        [ -e "$STUB/final-connect-unseen" ] && [ -e "$STUB/idle-started" ] && exit 0
        no_opening=; [ -e "$STUB/one-opening-missing" ] && [ ! -e "$STUB/opening-dropped" ] && : > "$STUB/opening-dropped" && no_opening=1
        t=$(tick); open=$(( t + 10000 )); done=$(( t + 260000 + $( [ -e "$STUB/slow-connect" ] && echo 400000 || echo 0) ))
        if [ -e "$STUB/populated-first" ]; then link_up "$done" "$open"; else link_up "$open" "$done"; fi ;;
    *) exit 1 ;;
    esac ;;
*) exit 1 ;;
esac
"#;

/// The longest the stub socket serves, well past any run that reaches it.
const SERVER_DEADLINE: Duration = Duration::from_secs(120);
/// The start of the census prompt the command writes on its standard error.
const PROMPT: &str = "C6: take both buds out";
const CONFIRMED_REPLY: &str = r#"{"id":1,"ok":true,"result":"confirmed"}"#;
const SENT_REPLY: &str = r#"{"id":1,"ok":true,"result":"sent"}"#;
const NOT_WORN_REPLY: &str =
    r#"{"id":1,"ok":false,"error":{"code":"not_worn","message":"The AirPods are not in an ear."}}"#;
const NOT_SUPPORTED_REPLY: &str =
    r#"{"id":1,"ok":false,"error":{"code":"not_supported","message":"m"}}"#;

/// How long a test waits for one run to end, well past the longest run here.
pub(crate) const RUN_DEADLINE: Duration = Duration::from_secs(60);
/// How long a run may take to restore and exit after a stop signal.
const SIGNAL_DEADLINE: Duration = Duration::from_secs(10);
/// How long the stand-in for the daemon's main process lives: the longest a test waits on one run, its stop signal included.
const STAND_IN: Duration = RUN_DEADLINE.saturating_add(SIGNAL_DEADLINE);
/// How long a test waits for a stub to write a file it names.
const FILE_DEADLINE: Duration = Duration::from_secs(30);
/// How often a test looks again at a file, a call log or a process.
const POLL: Duration = Duration::from_millis(10);
/// How long a bench waits for the processes it ends to be gone.
const SWEEP_DEADLINE: Duration = Duration::from_secs(5);

/// A temporary directory holding the stubs, their state, the home and the temporary directory of one run, and the fake pods every run dials through the trap.
pub(crate) struct Bench {
    root: tempfile::TempDir,
    main: Child,
    pods: Pods,
    refusal: Cell<Option<Errno>>,
    /// Dropped after the runs `drop` ends, so no run outlives its trap.
    traps: RefCell<Vec<Trap>>,
}

impl Bench {
    pub(crate) fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let main = Command::new("sleep")
            .arg(STAND_IN.as_secs().to_string())
            .current_dir(root.path())
            .spawn()
            .unwrap();
        let pods = Pods::new(Profile {
            name: DEVICE_NAME.to_owned(),
            ..Profile::default()
        });
        let bench = Self {
            root,
            main,
            pods,
            refusal: Cell::new(None),
            traps: RefCell::new(Vec::new()),
        };
        for dir in ["bin", "home/.local/bin", "tmp", "runtime"] {
            fs::create_dir_all(bench.path(dir)).unwrap();
        }
        let stubs = [
            ("bin/systemctl", SYSTEMCTL),
            ("bin/systemd-run", SYSTEMD_RUN),
            ("bin/journalctl", JOURNALCTL),
            ("bin/busctl", BUSCTL),
            ("bin/bluetoothctl", BLUETOOTHCTL),
            ("bin/pactl", PACTL),
            ("bin/librepods", LIBREPODS),
            ("home/.local/bin/librepods-ctl", LIBREPODS),
        ];
        for (name, body) in stubs {
            let path = bench.path(name);
            fs::write(&path, format!("#!/bin/sh\n. \"$STUB/lib.sh\"\n{body}")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        // A model with every feature the census needs.
        bench.set_model(Model::AirPodsPro2USBC);
        let lib = format!("{}{LIB}", daemon_lines());
        let files = [
            ("lib.sh", lib.as_str()),
            ("calls", ""),
            ("journal", ""),
            ("clock", "5000000000"),
            ("starts", "0"),
            ("connected", "false"),
            ("bluez-connected", "true"),
            ("powered", "true"),
            ("profile", "a2dp-sink"),
            ("unit-librepods.service", "active"),
            ("unit-librepods-hw.service", "inactive"),
        ];
        for (name, body) in files {
            bench.set(name, body);
        }
        bench.set("main-pid", &bench.main.id().to_string());
        bench
    }

    /// Writes the two status lines the stub daemon prints, for `model`.
    pub(crate) fn set_model(&self, model: Model) {
        let mut state = State {
            device_name: DEVICE_NAME.to_owned(),
            model,
            ..State::default()
        };
        self.set("status-false.json", &to_status_line(&state));
        state.connected = true;
        self.set("status-true.json", &to_status_line(&state));
    }

    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    pub(crate) fn set(&self, name: &str, body: &str) {
        fs::write(self.path(name), body).unwrap();
    }

    pub(crate) fn read(&self, name: &str) -> String {
        fs::read_to_string(self.path(name)).unwrap_or_default()
    }

    pub(crate) fn command(&self, idle_seconds: &str, extra: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
        command
            .args(["hw", "link", "--binary"])
            .arg(self.path("bin/librepods"))
            .args(["--idle-window-seconds", idle_seconds])
            .args(extra)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.path("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("HOME", self.path("home"))
            .env("TMPDIR", self.path("tmp"))
            .env("XDG_RUNTIME_DIR", self.path("runtime"))
            .env("STUB", self.root.path())
            .current_dir(self.root.path())
            .stdin(Stdio::null());
        command
    }

    pub(crate) fn run(&self, extra: &[&str]) -> Output {
        let mut command = self.command("1", extra);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        finish(self.spawn(command), RUN_DEADLINE)
    }

    /// Starts `command` under the trap, the pods' control channel routed to the fake pods unless `refuse_with` was called.
    pub(crate) fn spawn(&self, command: Command) -> Child {
        let answer = match self.refusal.get() {
            Some(errno) => Answer::Refuse {
                after: Duration::ZERO,
                errno,
            },
            None => self.serve(),
        };
        let route = Route {
            device: ADDRESS.parse().unwrap(),
            psm: CONTROL_PSM,
            answer,
        };
        let (child, trap) =
            pods_trap::spawn(command, vec![route]).unwrap_or_else(|error| panic!("{error}"));
        self.traps.borrow_mut().push(trap);
        child
    }

    /// Hands each dial to the fake pods, first writing to `units-at-dial` the state of the test unit and the installed service.
    fn serve(&self) -> Answer {
        let pods = self.pods.clone();
        let root = self.root.path().to_owned();
        let serve = move |channel| {
            let units = [UNDER_TEST, INSTALLED].map(|unit| {
                let state =
                    fs::read_to_string(root.join(format!("unit-{unit}"))).unwrap_or_default();
                format!("{unit} {}", state.trim())
            });
            // A panic here would end the trap's thread, so a failed write only leaves the file missing.
            drop(fs::write(root.join("units-at-dial"), units.join("\n")));
            pods.serve(channel);
        };
        Answer::Accept {
            after: Duration::ZERO,
            serve: Arc::new(serve),
        }
    }

    /// Makes the runs started after it find the pods refusing the control channel with `errno`.
    pub(crate) fn refuse_with(&self, errno: Errno) {
        self.refusal.set(Some(errno));
    }

    pub(crate) fn pods(&self) -> &Pods {
        &self.pods
    }

    /// Every dial the trap answered, over every run of the bench.
    pub(crate) fn dials(&self) -> Vec<Dial> {
        self.traps.borrow().iter().flat_map(Trap::dials).collect()
    }

    /// Every Bluetooth socket call the trap saw, over every run of the bench.
    pub(crate) fn sockets(&self) -> usize {
        self.traps.borrow().iter().map(Trap::sockets).sum()
    }

    /// The processes still working in the bench once a run has ended, its stand-in for the daemon's main process aside.
    pub(crate) fn left_running(&self) -> Vec<i32> {
        let main = i32::try_from(self.main.id()).unwrap();
        let mut left = processes_in(self.root.path());
        left.retain(|pid| *pid != main);
        left
    }

    pub(crate) fn calls(&self) -> Vec<String> {
        self.read("calls").lines().map(str::to_owned).collect()
    }

    /// True when the installed service ends in `state` and the test unit is not running.
    pub(crate) fn installed_is(&self, state: &str) -> bool {
        self.read("unit-librepods.service").trim() == state
            && self.read("unit-librepods-hw.service").trim() == "inactive"
    }

    /// Serves protocol 1 `set` on the run's socket until `done` is set or a deadline passes, so it never outlives the run.
    fn serve_sets(&self, buds_out: Arc<AtomicBool>, done: Arc<AtomicBool>) -> JoinHandle<Served> {
        let root = self.root.path().to_owned();
        thread::spawn(move || {
            let run_dir = wait_for_file(&root.join("run-dir"));
            let socket = Path::new(run_dir.trim()).join("librepods.sock");
            drop(fs::remove_file(&socket));
            let listener = UnixListener::bind(&socket).unwrap();
            listener.set_nonblocking(true).unwrap();
            let weak = root.join("weak-census").exists();
            let refuse = root.join("refuse-one-bud-off").exists();
            // Under census-silent every connection is accepted and held open, and nothing is ever read or answered.
            let silent = root.join("census-silent").exists();
            let mut held = Vec::new();
            let deadline = Instant::now() + SERVER_DEADLINE;
            let mut served = Served::default();
            let status = fs::read_to_string(root.join("status-true.json")).unwrap();
            let status: serde_json::Value = serde_json::from_str(&status).unwrap();
            let mut level = status["adaptive_noise_level"].to_string();
            while !done.load(Ordering::SeqCst) && Instant::now() < deadline {
                let Ok((stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                if silent {
                    held.push(stream);
                    continue;
                }
                let out = buds_out.load(Ordering::SeqCst);
                if let Some(request) = answer(&stream, out, weak, refuse, &mut level) {
                    if out {
                        served.after_prompt.push(request);
                    } else {
                        served.before_prompt.push(request);
                    }
                }
            }
            served
        })
    }

    /// A run serving `set`, attended with the adapter cycle unless `attended-only` or `unattended`, where the buds come out and Enter is pressed when the prompt appears, or SIGINT sent under `interrupt-at-prompt`, after a line left unfinished under `half-line-at-prompt`.
    pub(crate) fn run_attended(&self) -> (Output, Served) {
        let half_line = self.root.path().join("half-line-at-prompt").exists();
        let interrupt = half_line || self.root.path().join("interrupt-at-prompt").exists();
        let link_drops = self.root.path().join("link-down-after-prompt").exists();
        let connected = self.path("connected");
        let flags: &[&str] = if self.root.path().join("unattended").exists() {
            &[]
        } else if self.root.path().join("attended-only").exists() {
            &["--attended"]
        } else {
            &["--attended", "--adapter-cycle"]
        };
        let buds_out = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let server = self.serve_sets(Arc::clone(&buds_out), Arc::clone(&done));
        let mut command = self.command("1", flags);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = self.spawn(command);
        let run = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let stderr = BufReader::new(child.stderr.take().unwrap());
        let person = thread::spawn(move || {
            let mut seen = String::new();
            for line in stderr.lines().map_while(Result::ok) {
                if line.contains(PROMPT) {
                    buds_out.store(true, Ordering::SeqCst);
                    if link_drops {
                        fs::write(&connected, "false").unwrap();
                    }
                    if half_line {
                        drop(stdin.write_all(b"y"));
                        // Long enough for the run to have taken the byte before the signal lands.
                        thread::sleep(HALF_LINE_LEAD);
                    }
                    if interrupt {
                        kill_process(run, Signal::INT).ok();
                    } else {
                        drop(stdin.write_all(b"\n"));
                    }
                }
                seen.push_str(&line);
                seen.push('\n');
            }
            seen.into_bytes()
        });
        let mut output = finish(child, RUN_DEADLINE);
        output.stderr = person.join().unwrap();
        said(&output.stderr);
        done.store(true, Ordering::SeqCst);
        (output, server.join().unwrap())
    }
}

/// The `set` requests the stub socket read, split at the prompt to take the buds out.
#[derive(Default)]
pub(crate) struct Served {
    pub(crate) before_prompt: Vec<String>,
    pub(crate) after_prompt: Vec<String>,
}

impl Drop for Bench {
    /// Ends every process working in the bench, the run and its stubs included, and waits for them to be gone.
    fn drop(&mut self) {
        drop(self.main.kill());
        drop(self.main.wait());
        let deadline = Instant::now() + SWEEP_DEADLINE;
        loop {
            let left = processes_in(self.root.path());
            if left.is_empty() || Instant::now() >= deadline {
                break;
            }
            for pid in left {
                if let Some(pid) = Pid::from_raw(pid) {
                    kill_process(pid, Signal::KILL).ok();
                }
            }
            thread::sleep(POLL);
        }
        // Joins the fake pods' threads, and a thread that failed fails the test unless it is already failing.
        let finished = self.pods.clone().finish();
        if !thread::panicking() {
            finished.unwrap_or_else(|failure| panic!("the fake pods failed: {failure}"));
        }
    }
}

/// The processes whose working directory is `root` or under it; a removed one reads `/tmp/.tmpAb12/tmp (deleted)`, and a zombie has none.
pub(crate) fn processes_in(root: &Path) -> Vec<i32> {
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok())
        else {
            continue;
        };
        let Ok(cwd) = fs::read_link(entry.path().join("cwd")) else {
            continue;
        };
        let cwd = cwd.to_string_lossy();
        let cwd = cwd.strip_suffix(" (deleted)").unwrap_or(&cwd);
        if Path::new(cwd).starts_with(root) {
            found.push(pid);
        }
    }
    found
}

/// Answers one `set`: a new `adaptive_level` is sent and an unchanged one confirmed, the rest confirmed, and with the buds out `not_worn`; a weak link answers `noise anc` with sent, and with the buds out confirms it and nothing else, and `refuse` refuses `one_bud_anc false`.
fn answer(
    stream: &UnixStream,
    buds_out: bool,
    weak: bool,
    refuse: bool,
    level: &mut String,
) -> Option<String> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    let request: serde_json::Value = serde_json::from_str(&line).ok()?;
    let (key, value) = (request["key"].as_str()?, request["value"].as_str()?);
    let reply = match (buds_out, weak) {
        _ if refuse && key == "one_bud_anc" && value == "false" => NOT_SUPPORTED_REPLY,
        (true, true) if key == "noise" && value == "anc" => CONFIRMED_REPLY,
        (true, _) => NOT_WORN_REPLY,
        (false, _) if key == "adaptive_level" => {
            if std::mem::replace(level, value.to_owned()) == value {
                CONFIRMED_REPLY
            } else {
                SENT_REPLY
            }
        }
        (false, true) if key == "noise" && value == "anc" => SENT_REPLY,
        (false, _) => CONFIRMED_REPLY,
    };
    {
        let mut writer = stream;
        writer.write_all(format!("{reply}\n").as_bytes()).ok()?;
    }
    Some(format!("{key} {value}"))
}

pub(crate) fn wait_for_file(path: &Path) -> String {
    let deadline = Instant::now() + FILE_DEADLINE;
    while Instant::now() < deadline {
        if let Ok(text) = fs::read_to_string(path)
            && !text.is_empty()
        {
            return text;
        }
        thread::sleep(POLL);
    }
    panic!("{} never appeared", path.display());
}

/// Waits for `child` until `limit`, its pipes read on threads; a run past it fails the test, and the bench's drop ends it.
pub(crate) fn finish(mut child: Child, limit: Duration) -> Output {
    let stdout = child.stdout.take().map(read_all);
    let stderr = child.stderr.take().map(read_all);
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the run did not end within {} s",
            limit.as_secs()
        );
        thread::sleep(POLL);
    };
    let joined = |reader: Option<JoinHandle<Vec<u8>>>| {
        reader.map_or_else(Vec::new, |reader| reader.join().unwrap())
    };
    let stderr = joined(stderr);
    said(&stderr);
    Output {
        status,
        stdout: joined(stdout),
        stderr,
    }
}

fn read_all(mut pipe: impl Read + Send + 'static) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).unwrap();
        bytes
    })
}

/// Echoes what a run wrote to stderr: the test runner shows it only for a test that fails, which is when it is wanted.
fn said(stderr: &[u8]) {
    std::io::stderr().write_all(stderr).unwrap();
}

pub(crate) fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

pub(crate) fn last_line(bytes: &[u8]) -> String {
    text(bytes).lines().last().unwrap_or_default().to_owned()
}

pub(crate) fn index_of(calls: &[String], call: &str) -> Option<usize> {
    calls.iter().rposition(|line| line == call)
}

/// The installed service was stopped, then started again after the test unit's last stop.
pub(crate) fn assert_restarted_after_the_run(bench: &Bench) {
    let calls = bench.calls();
    let stopped = index_of(&calls, "systemctl --user stop librepods.service")
        .expect("the installed service was stopped");
    let unit_stopped = index_of(&calls, "systemctl --user stop librepods-hw.service")
        .expect("the test unit was stopped");
    let started = index_of(&calls, "systemctl --user start librepods.service")
        .expect("the installed service was started");
    assert!(stopped < unit_stopped && unit_stopped < started);
    assert!(bench.installed_is("active"));
}

/// Returns once a stub has logged `call`.
pub(crate) fn wait_for_call(bench: &Bench, call: &str) {
    let deadline = Instant::now() + RUN_DEADLINE;
    while !bench.calls().iter().any(|made| made == call) {
        assert!(Instant::now() < deadline, "the run never made {call}");
        thread::sleep(POLL);
    }
}

/// Sends `signal` once the run has made `call`, and requires the restore and status 1 soon after.
pub(crate) fn interrupted_by(bench: &Bench, signal: &str, call: &str) {
    let output = signalled(bench, signal, call);
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("stopped by a signal"));
    assert_restarted_after_the_run(bench);
}

/// Starts a run, sends `signal` once the run has made `call`, and gives what the run printed.
pub(crate) fn signalled(bench: &Bench, signal: &str, call: &str) -> Output {
    signalled_once(bench, "60", signal, || wait_for_call(bench, call))
}

/// Starts a run, sends `signal` once `wait` returns, and gives what the run printed.
pub(crate) fn signalled_once(
    bench: &Bench,
    idle_seconds: &str,
    signal: &str,
    wait: impl FnOnce(),
) -> Output {
    let mut command = bench.command(idle_seconds, &[]);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = bench.spawn(command);
    wait();
    let sent = Command::new("kill")
        .args([signal, &child.id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success());
    finish(child, SIGNAL_DEADLINE)
}

/// A bench whose daemon never connects, so the run waits on `connected` until a signal ends it.
pub(crate) fn waiting_bench() -> Bench {
    let bench = Bench::new();
    bench.set("pods-absent", "");
    bench
}
