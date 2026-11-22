//! The two user units the hardware run may touch, the program runner every external command goes through, and the restore guard.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// The installed daemon's unit, stopped for the run and restored after it.
pub(crate) const INSTALLED: &str = "librepods.service";

/// The transient unit the binary under test runs in.
pub(crate) const UNDER_TEST: &str = "librepods-hw.service";

/// The name `systemd-run --unit` takes for `UNDER_TEST`.
const UNDER_TEST_NAME: &str = "librepods-hw";

/// The installed unit file, read at build time so the run's hardening cannot drift from it.
const SERVICE_FILE: &str = include_str!("../../../../daemon/librepods.service");

/// The first directive of the hardening block that the test unit copies.
const FIRST_HARDENING_DIRECTIVE: &str = "UMask";

/// Directives the test unit leaves out: five name the installed daemon's real directories, and `PrivateTmp` would hide the run directory under `/tmp`.
const REPLACED_DIRECTIVES: [&str; 6] = [
    "PrivateTmp",
    "StateDirectory",
    "StateDirectoryMode",
    "ConfigurationDirectory",
    "ConfigurationDirectoryMode",
    "ReadWritePaths",
];

/// How long one `systemctl` or `systemd-run` call may take; a stop waits for the daemon to exit.
pub(crate) const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the restored service has to report active after its start returned.
const RESTORE_ACTIVE_WAIT: Duration = Duration::from_secs(5);

/// How often a wait on a unit or the journal looks again.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How often the runner looks whether a program has exited; it bounds the error of every timed command.
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// The most standard output kept from one program; journal reads are the largest.
const MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;

/// What one external program did: its exit status and its standard output.
pub(crate) struct Ran {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: String,
}

impl Ran {
    pub(crate) fn succeeded(&self) -> bool {
        self.status.success()
    }
}

/// Waits `duration` on the calling thread; xtask has no clock to inject.
#[expect(
    clippy::disallowed_methods,
    reason = "the hardware run waits on real time, which is what it measures"
)]
pub(crate) fn pause(duration: Duration) {
    thread::sleep(duration);
}

/// Runs `program` with `args` and `env`, no shell, for at most `timeout`, and keeps its standard output.
pub(crate) fn run(
    program: &str,
    args: &[&OsStr],
    env: &[(&str, &OsStr)],
    timeout: Duration,
) -> Result<Ran, String> {
    let output =
        tempfile::tempfile().map_err(|error| format!("{program}: no output file: {error}"))?;
    let child_output = output
        .try_clone()
        .map_err(|error| format!("{program}: no output file: {error}"))?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(child_output)
        // Standard error is never read, so nothing a tool prints about a device can reach the report.
        .stderr(Stdio::null())
        // Its own process group keeps a Ctrl-C on the terminal from killing the child, so only xtask hears it and restores.
        .process_group(0);
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("{program} could not start: {error}"))?;
    let deadline = Instant::now().checked_add(timeout);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if deadline.is_some_and(|deadline| Instant::now() < deadline) => {
                pause(EXIT_POLL_INTERVAL);
            }
            Ok(None) => {
                let killed = child.kill();
                let reaped = child.wait();
                drop((killed, reaped));
                return Err(format!(
                    "{program} {} did not finish within {} s and was killed",
                    first_word(args),
                    timeout.as_secs()
                ));
            }
            Err(error) => return Err(format!("{program}: could not wait: {error}")),
        }
    };
    Ok(Ran {
        status,
        stdout: read_output(output, program)?,
    })
}

/// The first argument that is neither an option nor a `-p` property value, which names what the program was asked to act on.
fn first_word(args: &[&OsStr]) -> String {
    args.iter()
        .find(|arg| {
            let arg = arg.to_string_lossy();
            !arg.starts_with('-') && !arg.contains('=')
        })
        .map_or_else(String::new, |arg| arg.to_string_lossy().into_owned())
}

fn read_output(mut file: File, program: &str) -> Result<String, String> {
    let size = file
        .metadata()
        .map_err(|error| format!("{program}: could not read its output: {error}"))?
        .len();
    if size > MAX_OUTPUT_BYTES {
        return Err(format!(
            "{program} wrote {size} bytes, more than the {MAX_OUTPUT_BYTES} a run keeps"
        ));
    }
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.take(MAX_OUTPUT_BYTES).read_to_end(&mut bytes))
        .map_err(|error| format!("{program}: could not read its output: {error}"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Runs `systemctl --user` with `args`.
pub(crate) fn systemctl(args: &[&str]) -> Result<Ran, String> {
    let mut all: Vec<&OsStr> = vec![OsStr::new("--user")];
    all.extend(args.iter().map(OsStr::new));
    run("systemctl", &all, &[], SYSTEMCTL_TIMEOUT)
}

/// Runs `systemctl --user` with `args` and requires exit status 0.
pub(crate) fn systemctl_ok(args: &[&str]) -> Result<(), String> {
    let ran = systemctl(args)?;
    if ran.succeeded() {
        Ok(())
    } else {
        Err(format!(
            "systemctl {} exited with {}",
            args.join(" "),
            ran.status
        ))
    }
}

/// The word `systemctl is-active` prints for `unit`, such as `active` or `inactive`; sample input `inactive\n`, with exit status 3, or 4 for the collected test unit once gone.
pub(crate) fn activity(unit: &str) -> Result<String, String> {
    let ran = systemctl(&["is-active", unit])?;
    let word = ran.stdout.trim().to_owned();
    // Exit 4 means systemd holds no such unit, which only the collected test unit may be.
    let known = match ran.status.code() {
        Some(0 | 3) => true,
        Some(4) => unit == UNDER_TEST,
        _ => false,
    };
    if known && !word.is_empty() && !word.contains(char::is_whitespace) {
        Ok(word)
    } else {
        Err(format!(
            "systemctl is-active {unit} gave no state ({})",
            ran.status
        ))
    }
}

/// Stops `unit` and requires it not running after the stop.
pub(crate) fn stop(unit: &str) -> Result<(), String> {
    systemctl_ok(&["stop", unit])?;
    let word = activity(unit)?;
    if is_running(&word) {
        return Err(format!("{unit} is still {word} after its stop"));
    }
    Ok(())
}

/// True for the states in which the unit's process is running or about to.
pub(crate) fn is_running(word: &str) -> bool {
    matches!(word, "active" | "activating" | "reloading" | "deactivating")
}

/// True when systemd knows a unit named `UNDER_TEST`, in any state; sample input `not-found\n` for no such unit, `loaded\n` for one.
pub(crate) fn test_unit_exists() -> Result<bool, String> {
    let ran = systemctl(&["show", "--property=LoadState", "--value", UNDER_TEST])?;
    if !ran.succeeded() {
        return Err(format!(
            "systemctl show {UNDER_TEST} exited with {}",
            ran.status
        ));
    }
    Ok(ran.stdout.trim() != "not-found")
}

/// The main process of the unit under test; sample input `4242\n`; `0\n` means no process.
pub(crate) fn main_pid() -> Result<u32, String> {
    let ran = systemctl(&["show", "--property=MainPID", "--value", UNDER_TEST])?;
    if !ran.succeeded() {
        return Err(format!(
            "systemctl show {UNDER_TEST} exited with {}",
            ran.status
        ));
    }
    match ran.stdout.trim().parse::<u32>() {
        Ok(pid) if pid != 0 => Ok(pid),
        _ => Err(format!("{UNDER_TEST} has no main process")),
    }
}

/// The `-p` properties of the installed unit's hardening block, from `UMask` to the end of `[Service]`, less the replaced ones; sample input `[Service]`, `ExecStart=...`, `UMask=0077`, `PrivateTmp=yes`, `[Install]`, which gives `UMask=0077`.
pub(crate) fn hardening_properties(unit_file: &str) -> Vec<String> {
    let mut in_service = false;
    let mut in_block = false;
    let mut properties = Vec::new();
    for line in unit_file.lines().map(str::trim) {
        if line.starts_with('[') {
            in_service = line == "[Service]";
            in_block = false;
            continue;
        }
        if !in_service || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        in_block = in_block || key == FIRST_HARDENING_DIRECTIVE;
        if in_block && !REPLACED_DIRECTIVES.contains(&key) {
            properties.push(line.to_owned());
        }
    }
    properties
}

/// The arguments of `systemd-run` that start `binary` in the test unit, writable only under `dir`.
pub(crate) fn start_arguments(binary: &Path, dir: &Path, uid: u32) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["--user", "--collect", "--quiet"]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.push(format!("--unit={UNDER_TEST_NAME}").into());
    for property in hardening_properties(SERVICE_FILE) {
        args.push("-p".into());
        args.push(property.into());
    }
    let mut read_write = OsString::from("ReadWritePaths=");
    read_write.push(dir);
    args.extend(["-p".into(), read_write]);
    // Ten restarts of a daemon that is up in a fraction of a second would trip the default start rate limit.
    args.extend(["-p".into(), "StartLimitIntervalSec=0".into()]);
    for (name, path) in [("XDG_RUNTIME_DIR", "run"), ("XDG_STATE_HOME", "state")] {
        let mut setting = OsString::from(format!("--setenv={name}="));
        setting.push(dir.join(path));
        args.push(setting);
    }
    // The moved runtime directory would move the session bus lookup with it, so the real bus is named.
    args.push(format!("--setenv=DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{uid}/bus").into());
    args.push("--".into());
    args.push(binary.into());
    args.push("--headless".into());
    args
}

/// Starts the binary under test in the transient unit.
pub(crate) fn start_under_test(binary: &Path, dir: &Path, uid: u32) -> Result<(), String> {
    let args = start_arguments(binary, dir, uid);
    let refs: Vec<&OsStr> = args.iter().map(OsString::as_os_str).collect();
    let ran = run("systemd-run", &refs, &[], SYSTEMCTL_TIMEOUT)?;
    if ran.succeeded() {
        Ok(())
    } else {
        Err(format!(
            "systemd-run could not start {UNDER_TEST} ({})",
            ran.status
        ))
    }
}

/// Puts the installed service back the way the run found it, from the moment it was stopped; dropping it on any way out, an unwind included, restores.
#[derive(Debug)]
pub(crate) struct Guard {
    was_running: bool,
    pending: bool,
}

impl Guard {
    pub(crate) fn new(was_running: bool) -> Self {
        Self {
            was_running,
            pending: true,
        }
    }

    /// Restores now and gives the result to the caller instead of printing it on drop.
    pub(crate) fn restore(mut self) -> Result<(), String> {
        self.pending = false;
        put_back(self.was_running)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.pending {
            crate::report(&restored_line(&put_back(self.was_running)));
        }
    }
}

/// The line that tells the person how the restore went, which on a failure is the last line of the run.
pub(crate) fn restored_line(restored: &Result<(), String>) -> String {
    match restored {
        Ok(()) => format!("xtask hw: {INSTALLED} is as the run found it"),
        Err(error) => format!(
            "xtask hw: RESTORE FAILED: {error}. {INSTALLED} is not as the run found it; run systemctl --user start {INSTALLED}"
        ),
    }
}

/// Stops the test unit, then starts the installed service if it was running; the error says what is left wrong.
fn put_back(was_running: bool) -> Result<(), String> {
    // The test unit may never have started, so a failed stop is judged by the state after it.
    let stopped = systemctl(&["stop", UNDER_TEST]);
    let after = activity(UNDER_TEST)?;
    if is_running(&after) {
        let reason = stopped.err().unwrap_or_default();
        return Err(format!(
            "{UNDER_TEST} is still {after} after its stop {reason}"
        ));
    }
    if !was_running {
        return Ok(());
    }
    systemctl_ok(&["start", INSTALLED])?;
    let deadline = Instant::now().checked_add(RESTORE_ACTIVE_WAIT);
    loop {
        let word = activity(INSTALLED)?;
        if word == "active" {
            return Ok(());
        }
        if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
            return Err(format!("{INSTALLED} is {word} after its start"));
        }
        pause(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Command;

    use std::ffi::{OsStr, OsString};
    use std::time::Duration;

    use std::io::Write;

    use proptest::prelude::*;

    use super::{
        Guard, MAX_OUTPUT_BYTES, SERVICE_FILE, first_word, hardening_properties, read_output, run,
        start_arguments,
    };

    /// The longest arbitrary output the property feeds the reader; the cap itself is the `head` test's.
    const ARBITRARY_OUTPUT_BYTES: usize = 70_000;

    /// What `read_output` gives for a captured output file holding `bytes`.
    fn read_back(bytes: &[u8]) -> Result<String, String> {
        let mut file = tempfile::tempfile().map_err(|error| error.to_string())?;
        file.write_all(bytes).map_err(|error| error.to_string())?;
        read_output(file, "probe")
    }

    /// Set only in the copy of this test binary that panics inside the guarded scope.
    const PANIC_CHILD: &str = "XTASK_HW_GUARD_PANIC_CHILD";

    /// A `systemctl --user` that logs each call and keeps each unit's state in a file named after it.
    const SYSTEMCTL_STUB: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$STUB/calls"
case "$2" in
is-active) s=$(cat "$STUB/$3"); echo "$s"; [ "$s" = active ] || exit 3 ;;
stop) echo inactive > "$STUB/$3" ;;
start) echo active > "$STUB/$3" ;;
*) exit 1 ;;
esac
"#;

    #[test]
    fn a_panic_inside_the_guarded_scope_restores_the_installed_service() {
        let stub = tempfile::tempdir().unwrap();
        let systemctl = stub.path().join("systemctl");
        fs::write(&systemctl, SYSTEMCTL_STUB).unwrap();
        fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(stub.path().join("librepods.service"), "inactive").unwrap();
        fs::write(stub.path().join("librepods-hw.service"), "active").unwrap();
        let path = format!("{}:{}", stub.path().display(), env::var("PATH").unwrap());
        let child = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "--nocapture",
                "hw::unit::tests::panic_inside_the_guarded_scope",
            ])
            .env(PANIC_CHILD, "1")
            .env("PATH", path)
            .env("STUB", stub.path())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&child.stderr);
        assert!(!child.status.success(), "the child did not panic: {stderr}");
        assert!(
            stderr.contains("librepods.service is as the run found it"),
            "{stderr}"
        );
        let calls = fs::read_to_string(stub.path().join("calls")).unwrap();
        assert_eq!(
            calls.lines().collect::<Vec<_>>(),
            [
                "--user stop librepods-hw.service",
                "--user is-active librepods-hw.service",
                "--user start librepods.service",
                "--user is-active librepods.service"
            ]
        );
    }

    /// Runs only as the child of the test above, which reads what the guard did as the panic unwound.
    #[test]
    fn panic_inside_the_guarded_scope() {
        if env::var_os(PANIC_CHILD).is_none() {
            return;
        }
        let _guard = Guard::new(true);
        panic!("inside the guarded scope");
    }

    #[test]
    fn the_hardening_block_starts_at_umask_and_skips_the_replaced_directives() {
        let unit = "[Unit]\nUMask=0000\n[Service]\nExecStart=x\n# note\nUMask=0077\nPrivateTmp=yes\n\
                    StateDirectory=a\nProtectHome=read-only\nCapabilityBoundingSet=\n[Install]\nWantedBy=y\n";
        assert_eq!(
            hardening_properties(unit),
            [
                "UMask=0077",
                "ProtectHome=read-only",
                "CapabilityBoundingSet="
            ]
        );
    }

    #[test]
    fn the_shipped_unit_file_gives_its_hardening_and_none_of_its_paths() {
        let properties = hardening_properties(SERVICE_FILE);
        assert!(properties.iter().any(|line| line == "UMask=0077"));
        assert!(
            properties
                .iter()
                .any(|line| line.starts_with("RestrictAddressFamilies="))
        );
        assert!(properties.iter().all(|line| !line.contains('%')));
        assert!(properties.iter().all(|line| !line.starts_with("ExecStart")));
    }

    #[test]
    fn the_start_arguments_end_with_the_binary_in_headless_mode() {
        let args = start_arguments(Path::new("/x/librepods"), Path::new("/t/d"), 1000);
        let text: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(text[text.len() - 3..], ["--", "/x/librepods", "--headless"]);
        assert!(text.contains(&"--setenv=XDG_RUNTIME_DIR=/t/d/run".to_owned()));
        assert!(text.contains(
            &"--setenv=DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus".to_owned()
        ));
        assert!(text.contains(&"ReadWritePaths=/t/d".to_owned()));
    }

    #[test]
    fn a_timeout_names_what_the_program_acted_on_not_a_property_value() {
        let args = [
            OsStr::new("-i"),
            OsStr::new("A=1"),
            OsStr::new("sleep"),
            OsStr::new("5"),
        ];
        let error = run("env", &args, &[], Duration::from_millis(200))
            .err()
            .unwrap();
        assert!(error.starts_with("env sleep did not finish"), "{error}");
        let start = start_arguments(Path::new("/x/librepods"), Path::new("/t/d"), 1000);
        let refs: Vec<&OsStr> = start.iter().map(OsString::as_os_str).collect();
        assert_eq!(first_word(&refs), "/x/librepods");
    }

    #[test]
    fn output_up_to_the_cap_is_kept_whole_and_past_it_is_an_error() {
        let head = |bytes: u64| {
            let count = format!("--bytes={bytes}");
            let args = [OsStr::new(&count), OsStr::new("/dev/zero")];
            run("head", &args, &[], Duration::from_secs(30))
        };
        let kept = head(MAX_OUTPUT_BYTES).unwrap().stdout;
        assert_eq!(u64::try_from(kept.len()).unwrap(), MAX_OUTPUT_BYTES);
        let error = head(MAX_OUTPUT_BYTES + 1).err().unwrap();
        assert!(error.contains("more than the"), "{error}");
    }

    #[test]
    fn output_that_is_not_utf8_is_kept_with_replacements() {
        assert_eq!(read_back(b"ok\xff\xfe\n").unwrap(), "ok\u{fffd}\u{fffd}\n");
        assert_eq!(read_back(b"").unwrap(), "");
    }

    proptest! {
        #[test]
        fn any_output_bytes_read_back_as_their_lossy_text(
            bytes in proptest::collection::vec(any::<u8>(), 0..=ARBITRARY_OUTPUT_BYTES)
        ) {
            prop_assert_eq!(read_back(&bytes), Ok(String::from_utf8_lossy(&bytes).into_owned()));
        }
    }
}
