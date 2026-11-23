//! One temporary directory per case: the home, a copy of the plugin, the stubs, the allow list of real programs and the call log.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process_group};
use tempfile::TempDir;

/// The synthetic version the served binary reports.
pub(crate) const VERSION: &str = "9.8.7";
/// The release asset of that version, as the lock line names it.
pub(crate) const ASSET: &str = "librepods-9.8.7-x86_64";
/// The user id a case runs as unless it sets another.
const USER_ID: &str = "1000\n";
/// The machine type a case runs on unless it sets another.
const MACHINE: &str = "x86_64\n";
/// The D-Bus object `busctl` reads, which names no file.
pub(crate) const ADAPTER_OBJECT: &str = "/org/bluez/hci0";
/// What `busctl` prints for the manager's monitor types when BlueZ serves it.
pub(crate) const SERVED_TYPES: &str = "as 1 \"or_patterns\"\n";
/// Where a case links the built daemon for the payload's `state-dir`.
pub(crate) const BUILT_DAEMON_LINK: &str = "stub/librepods-built";
/// The real programs the installer and the stubs run; any other name is not found.
const REAL: [&str; 14] = [
    "bash",
    "cat",
    "dirname",
    "install",
    "ln",
    "mkdir",
    "mktemp",
    "mv",
    "readlink",
    "rm",
    "rmdir",
    "sha256sum",
    "sleep",
    "timeout",
];
/// How long a case waits for one run of the installer, well past its own longest wait.
const RUN_DEADLINE: Duration = Duration::from_secs(60);
/// How often a case looks again at the run it waits for.
const POLL: Duration = Duration::from_millis(10);
const PERMISSION_BITS: u32 = 0o7777;
/// Any of the three execute bits.
const ANY_EXECUTE_BITS: u32 = 0o111;
/// The mode of every stub the bench writes.
const STUB_MODE: u32 = 0o755;
/// The bench root holds a space, so a path the installer leaves unquoted splits and the case goes red.
const ROOT_PREFIX: &str = "setup bench ";

/// Shared by every stub: the call log, one line per call with each argument ended by a NUL, and a refusal of any path outside the bench.
const LIB: &str = r#"
log() {
    printf '%s\0' "$@" >> "$bench/calls.log"
    printf '\n' >> "$bench/calls.log"
}
inside() {
    case "$1" in
    *"/.." | *"/../"*) ;;
    "$bench"/*) return 0 ;;
    esac
    echo "stub: refusing a path outside the bench: $1" >&2
    exit 99
}
"#;

const CURL: &str = r#"
log curl "$@"
out=
while [ "$#" -gt 0 ]; do
    [ "$1" = --output ] && out=$2
    shift
done
inside "$out"
if [ -e "$bench/stub/curl-fails" ]; then
    echo 'curl: (22) The requested URL returned error: 404' >&2
    exit 22
fi
cat "$bench/stub/served" > "$out"
"#;

/// Fails the verb named in `stub/systemctl-fails`, the way a session with no user bus does.
const SYSTEMCTL: &str = r#"
log systemctl "$@"
if [ -e "$bench/stub/systemctl-fails" ] && [ "$2" = "$(cat "$bench/stub/systemctl-fails")" ]; then
    echo 'Failed to connect to bus: No medium found' >&2
    exit 1
fi
"#;

const SUDO: &str = r#"
log sudo "$@"
exit 1
"#;

/// Prints `stub/busctl-output` and exits with `stub/busctl-exit`, the served manager's reply unless the case set another, and names the interface on standard error when it fails.
const BUSCTL: &str = r#"
log busctl "$@"
cat "$bench/stub/busctl-output"
status=$(cat "$bench/stub/busctl-exit")
[ "$status" = 0 ] || echo 'Failed to get property SupportedMonitorTypes on interface org.bluez.AdvertisementMonitorManager1: Unknown interface' >&2
exit "$status"
"#;

const ID: &str = r#"
log id "$@"
cat "$bench/stub/uid"
"#;

const UNAME: &str = r#"
log uname "$@"
cat "$bench/stub/machine"
"#;

const CARGO: &str = r#"
log cargo "$@" in "$PWD"
inside "$PWD/"
if [ -e "$bench/stub/cargo-fails" ]; then
    echo 'error: could not compile `librepods`' >&2
    exit 101
fi
mkdir -p target/release
cat "$bench/stub/payload" > target/release/librepods
"#;

/// The binary the release serves and cargo builds: it answers `--version`, `status` unless silenced or slowed, and `state-dir` through the built daemon.
/// A slowed `status` spends the client's 2 s worst case (connect, write and reply timeouts) before it fails.
const PAYLOAD: &str = r#"
log librepods "$@"
case "$1" in
--version) echo "librepods VERSION" ;;
state-dir)
    [ -e "$bench/stub/state-dir-fails" ] && { echo 'librepods: cannot place the state file' >&2; exit 1; }
    [ -e "$bench/stub/state-dir-hangs" ] && { trap '' TERM; exec sleep 30; }
    exec "$bench/BUILT_LINK" state-dir ;;
status)
    [ -e "$bench/stub/silent" ] && exit 1
    [ -e "$bench/stub/slow" ] && { sleep 2; exit 1; }
    echo '{"connected":false,"schema_version":1}' ;;
esac
"#;

/// Stands in for `rm` or `rmdir`: logs the call, refuses an absolute path outside the bench, then runs the real program.
const RECORDED_REMOVAL: &str = r#"
log NAME "$@"
for argument in "$@"; do
    case "$argument" in
    /*) inside "$argument" ;;
    esac
done
exec 'REAL' "$@"
"#;

/// A file, a directory or a symbolic link as a listing records it.
#[derive(Debug, PartialEq)]
pub(crate) enum Node {
    Dir(u32),
    File(u32, Vec<u8>),
    Link(PathBuf),
}

pub(crate) type Tree = BTreeMap<PathBuf, Node>;

/// What one run of the installer did.
pub(crate) struct Ran {
    pub(crate) code: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) took: Duration,
}

pub(crate) struct Bench {
    root: TempDir,
}

impl Bench {
    pub(crate) fn new() -> Self {
        let bench = Self {
            root: tempfile::Builder::new()
                .prefix(ROOT_PREFIX)
                .tempdir()
                .unwrap(),
        };
        for dir in ["home", "plugin", "bin", "runtime", "stub"] {
            fs::create_dir(bench.path(dir)).unwrap();
        }
        copy_tree(&repo().join("packaging"), &bench.path("plugin/packaging"));
        fs::copy(
            repo().join("manifest.json"),
            bench.path("plugin/manifest.json"),
        )
        .unwrap();
        for name in REAL {
            symlink(real_program(name), bench.path(&format!("bin/{name}"))).unwrap();
        }
        let stubs = [
            ("curl", CURL),
            ("systemctl", SYSTEMCTL),
            ("sudo", SUDO),
            ("busctl", BUSCTL),
            ("id", ID),
            ("uname", UNAME),
        ];
        for (name, body) in stubs {
            bench.write_stub(&format!("bin/{name}"), body);
        }
        bench.write_stub(
            "stub/payload",
            &PAYLOAD
                .replace("VERSION", VERSION)
                .replace("BUILT_LINK", BUILT_DAEMON_LINK),
        );
        fs::copy(bench.path("stub/payload"), bench.path("stub/served")).unwrap();
        bench.set("uid", USER_ID);
        bench.set("machine", MACHINE);
        bench.set("busctl-output", SERVED_TYPES);
        bench.set("busctl-exit", "0\n");
        fs::write(bench.path("calls.log"), "").unwrap();
        let lock = format!(
            "# The release this plugin installs.\n{}\n",
            bench.lock_line()
        );
        bench.write_lock(&lock);
        bench
    }

    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    pub(crate) fn root(&self) -> &Path {
        self.root.path()
    }

    /// Writes a control file the stubs read under `stub/`.
    pub(crate) fn set(&self, name: &str, body: &str) {
        fs::write(self.path(&format!("stub/{name}")), body).unwrap();
    }

    /// Lets the installed binary answer `state-dir` through the daemon this workspace built.
    pub(crate) fn link_built_daemon(&self) {
        symlink(built_daemon(), self.path(BUILT_DAEMON_LINK)).unwrap();
    }

    /// Replaces the plugin copy's `packaging/librepods.sha256`, which is never the repository's own.
    pub(crate) fn write_lock(&self, text: &str) {
        fs::write(self.path("plugin/packaging/librepods.sha256"), text).unwrap();
    }

    /// Puts a `cargo` on `PATH` that builds the payload into the plugin's `target/release`.
    pub(crate) fn add_cargo(&self) {
        self.write_stub("bin/cargo", CARGO);
    }

    pub(crate) fn payload(&self) -> Vec<u8> {
        fs::read(self.path("stub/payload")).unwrap()
    }

    /// The checksum line of the payload, as the release's lock file holds it.
    pub(crate) fn lock_line(&self) -> String {
        let summed = Command::new(real_program("sha256sum"))
            .arg(self.path("stub/payload"))
            .output()
            .unwrap();
        assert!(summed.status.success(), "sha256sum failed: {summed:?}");
        let text = String::from_utf8(summed.stdout).unwrap();
        // Sample input: `9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08  /tmp/.tmpAb12/stub/payload`
        let hash = text.split(' ').next().unwrap();
        format!("{hash}  {ASSET}")
    }

    /// Runs `bash plugin/packaging/setup` with only `HOME`, `PATH` and `XDG_RUNTIME_DIR` set, all inside the bench.
    pub(crate) fn run(&self, args: &[&str]) -> Ran {
        self.launch(args, self.path("home").as_os_str(), None, Stdio::piped())
    }

    /// The same run with `XDG_STATE_HOME` set as well.
    pub(crate) fn run_with_state_home(&self, args: &[&str], state_home: &str) -> Ran {
        let home = self.path("home");
        self.launch(
            args,
            home.as_os_str(),
            Some(OsStr::new(state_home)),
            Stdio::piped(),
        )
    }

    /// The same run with another `HOME`; the run's working directory is the bench root.
    pub(crate) fn run_as(&self, args: &[&str], home: &str) -> Ran {
        self.launch(args, OsStr::new(home), None, Stdio::piped())
    }

    /// The same run with its standard output appended to the call log, so a printed line and the calls around it keep their order.
    pub(crate) fn run_into_log(&self, args: &[&str]) -> Ran {
        let log = OpenOptions::new()
            .append(true)
            .open(self.path("calls.log"))
            .unwrap();
        self.launch(args, self.path("home").as_os_str(), None, Stdio::from(log))
    }

    fn launch(
        &self,
        args: &[&str],
        home: &OsStr,
        state_home: Option<&OsStr>,
        stdout: Stdio,
    ) -> Ran {
        let mut command = Command::new(self.path("bin/bash"));
        command
            .arg(self.path("plugin/packaging/setup"))
            .args(args)
            .env_clear()
            .env("HOME", home)
            .env("PATH", self.path("bin"))
            .env("XDG_RUNTIME_DIR", self.path("runtime"))
            .current_dir(self.root())
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(Stdio::piped())
            .process_group(0);
        if let Some(state_home) = state_home {
            command.env("XDG_STATE_HOME", state_home);
        }
        let started = Instant::now();
        let mut running = Running {
            child: command.spawn().unwrap(),
            reaped: false,
        };
        let stdout = running.child.stdout.take().map(read_all);
        let stderr = read_all(running.child.stderr.take().unwrap());
        let deadline = started.checked_add(RUN_DEADLINE).unwrap();
        let status = loop {
            if let Some(status) = running.child.try_wait().unwrap() {
                running.reaped = true;
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "the installer did not end within {} s",
                RUN_DEADLINE.as_secs()
            );
            thread::sleep(POLL);
        };
        let took = started.elapsed();
        let leftover = running.end_group();
        let ran = Ran {
            code: status.code(),
            stdout: stdout.map_or_default(|reader| reader.join().unwrap()),
            stderr: stderr.join().unwrap(),
            took,
        };
        assert!(
            !leftover,
            "the installer left a process of its group running"
        );
        ran
    }

    /// The arguments of each call the stubs logged.
    pub(crate) fn call_arguments(&self) -> Vec<Vec<String>> {
        let text = fs::read_to_string(self.path("calls.log")).unwrap();
        // Sample input: `curl\0--output\0/tmp/setup bench Ab12/runtime/omapods-setup.Cd34/librepods-9.8.7-x86_64\0\n`
        text.lines()
            .map(|line| {
                let ended = line.strip_suffix('\0').unwrap();
                ended.split('\0').map(str::to_owned).collect()
            })
            .collect()
    }

    /// Each call the stubs logged, its arguments joined by single spaces.
    pub(crate) fn calls(&self) -> Vec<String> {
        self.call_arguments()
            .iter()
            .map(|arguments| arguments.join(" "))
            .collect()
    }

    /// The arguments of each call that ran `program`.
    pub(crate) fn arguments_of(&self, program: &str) -> Vec<Vec<String>> {
        self.call_arguments()
            .into_iter()
            .filter(|arguments| arguments.first().is_some_and(|first| first == program))
            .collect()
    }

    /// Each call that ran `program`, its arguments joined by single spaces.
    pub(crate) fn calls_of(&self, program: &str) -> Vec<String> {
        self.arguments_of(program)
            .iter()
            .map(|arguments| arguments.join(" "))
            .collect()
    }

    /// Replaces the real `rm` and `rmdir` with stubs that log each call before they run the real program.
    pub(crate) fn record_removals(&self) {
        for name in ["rm", "rmdir"] {
            let stub = format!("bin/{name}");
            fs::remove_file(self.path(&stub)).unwrap();
            let real = real_program(name);
            let body = RECORDED_REMOVAL
                .replace("NAME", name)
                .replace("REAL", &real.display().to_string());
            self.write_stub(&stub, &body);
        }
    }

    pub(crate) fn clear_calls(&self) {
        fs::write(self.path("calls.log"), "").unwrap();
    }

    pub(crate) fn home_tree(&self) -> Tree {
        let mut tree = Tree::new();
        walk(&self.path("home"), &self.path("home"), &[], &mut tree);
        tree
    }

    /// Everything in the bench but `home`, the call log and the relative paths in `skip`.
    pub(crate) fn outside_home(&self, skip: &[&str]) -> Tree {
        let mut skipped = vec!["home", "calls.log"];
        skipped.extend_from_slice(skip);
        let mut tree = Tree::new();
        walk(self.root(), self.root(), &skipped, &mut tree);
        tree
    }

    pub(crate) fn runtime_entries(&self) -> Vec<PathBuf> {
        fs::read_dir(self.path("runtime"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect()
    }

    fn write_stub(&self, name: &str, body: &str) {
        let path = self.path(name);
        let text = format!(
            "#!/bin/bash\nbench='{}'\n{LIB}{body}\n",
            self.root().display()
        );
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(STUB_MODE)).unwrap();
    }
}

/// A run of the installer; dropped before it was reaped, it ends the run's whole process group.
struct Running {
    child: Child,
    reaped: bool,
}

impl Running {
    /// Kills whatever is left in the run's process group, and says whether anything was.
    fn end_group(&self) -> bool {
        let group = i32::try_from(self.child.id()).ok().and_then(Pid::from_raw);
        group.is_some_and(|group| kill_process_group(group, Signal::KILL).is_ok())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        self.end_group();
        drop(self.child.wait());
    }
}

/// The value of `readonly <name>=<value>` in the repository's installer.
pub(crate) fn script_constant(name: &str) -> String {
    let script = fs::read_to_string(repo().join("packaging/setup")).unwrap();
    let wanted = format!("readonly {name}=");
    // Sample input: `readonly service_wait_s=10`
    let value = script
        .lines()
        .find_map(|line| line.strip_prefix(&wanted))
        .unwrap_or_else(|| panic!("packaging/setup has no `{wanted}`"));
    value.trim_matches('"').to_owned()
}

pub(crate) fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The daemon the same workspace build left beside this test's directory, as `cargo xtask ci` builds both.
fn built_daemon() -> PathBuf {
    let test = std::env::current_exe().unwrap();
    // Sample input: `<target>/debug/build/xtask/0123abcd/out/setup-0123abcd`, under `<target>/debug/librepods`'s directory.
    test.ancestors()
        .map(|dir| dir.join("librepods"))
        .find(|daemon| daemon.is_file())
        .unwrap_or_else(|| {
            panic!(
                "no librepods above {}; build the workspace first, as cargo xtask ci does",
                test.display()
            )
        })
}

/// The first executable `name` on the test's own `PATH`.
fn real_program(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").unwrap();
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| {
            fs::metadata(candidate).is_ok_and(|meta| {
                meta.is_file() && meta.permissions().mode() & ANY_EXECUTE_BITS != 0
            })
        })
        .unwrap_or_else(|| panic!("no {name} on PATH"))
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn walk(base: &Path, dir: &Path, skip: &[&str], tree: &mut Tree) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let relative = path.strip_prefix(base).unwrap().to_path_buf();
        if skip.iter().any(|name| relative == Path::new(name)) {
            continue;
        }
        let meta = fs::symlink_metadata(&path).unwrap();
        let mode = meta.permissions().mode() & PERMISSION_BITS;
        let node = if meta.file_type().is_symlink() {
            Node::Link(fs::read_link(&path).unwrap())
        } else if meta.is_dir() {
            walk(base, &path, skip, tree);
            Node::Dir(mode)
        } else {
            Node::File(mode, fs::read(&path).unwrap())
        };
        tree.insert(relative, node);
    }
}

fn read_all(mut pipe: impl Read + Send + 'static) -> JoinHandle<String> {
    thread::spawn(move || {
        let mut text = String::new();
        pipe.read_to_string(&mut text).unwrap();
        text
    })
}
