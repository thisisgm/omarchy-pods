//! `hw link`: the daemon's hardware checks, run on the test machine with the installed service stopped and always restored.

mod adapter;
mod census;
mod checks;
mod idle;
mod lab;
mod report;
mod system;
mod unit;

use std::env;
use std::ffi::OsString;
use std::fs::DirBuilder;
use std::os::fd::OwnedFd;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use rustix::fs::{Mode, OFlags};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};

use crate::{print_line, report};
use lab::{CONNECT_WAIT, Lab, Presence};
use report::Check;
use unit::{Guard, INSTALLED};

/// Every check passed and every bounded gate was met.
const EXIT_PASSED: u8 = 0;
/// A check failed, a gate was missed, the run was stopped by a signal or a tool failed.
const EXIT_FAILED: u8 = 1;
/// Wrong usage, or a precondition of the machine that makes the run refuse to start.
const EXIT_USAGE: u8 = 2;
/// The `AirPods` never connected, so nothing was measured; the service was restored.
const EXIT_ABSENT: u8 = 77;
/// The installed service could not be restored.
const EXIT_NOT_RESTORED: u8 = 70;

/// The run lock's file in the user's runtime directory; a second run finds it held and starts nothing.
const RUN_LOCK: &str = "librepods-hw.lock";

/// The length of each idle window of C5 unless a test shortens it.
const IDLE_WINDOW_SECONDS: u64 = 600;

/// Set by SIGINT, SIGTERM or SIGHUP; every wait looks at it, and the restore then runs as on any other way out.
static STOP: AtomicBool = AtomicBool::new(false);

/// A stop signal arrived during a check.
#[derive(Debug)]
pub(crate) struct Interrupted;

fn stopped() -> bool {
    STOP.load(Ordering::SeqCst)
}

/// Hardware checks of the daemon, run on the machine with the `AirPods`.
#[derive(Debug, Parser)]
#[command(name = "hw", bin_name = "cargo xtask hw")]
struct Hw {
    #[command(subcommand)]
    command: HwCommand,
}

#[derive(Debug, Subcommand)]
enum HwCommand {
    /// Runs the link checks against the binary, in a transient unit, with the installed service stopped meanwhile.
    Link(LinkArgs),
}

#[derive(Debug, Args)]
struct LinkArgs {
    /// The librepods binary under test.
    #[arg(long)]
    binary: PathBuf,
    /// A person is at the machine: run the command census, which ends with a prompt to take both buds out.
    #[arg(long)]
    attended: bool,
    /// Also power the adapter off and on, which drops every Bluetooth device once.
    #[arg(long, requires = "attended")]
    adapter_cycle: bool,
    /// Seconds in each idle window of the idle check.
    #[arg(long, hide = true, default_value_t = IDLE_WINDOW_SECONDS)]
    idle_window_seconds: u64,
}

/// How the part of the run after the installed service stopped ended.
enum Outcome {
    Checked(Vec<Check>),
    Absent,
    Interrupted,
    Failed(String),
}

/// Runs one `hw` command; `args` starts with the word `hw`.
pub(crate) fn run(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let link = match Hw::try_parse_from(args) {
        Ok(Hw {
            command: HwCommand::Link(link),
        }) => link,
        Err(error) => return usage(&error),
    };
    let Some(home) = env::var_os("HOME").filter(|home| home.to_str().is_some()) else {
        report(
            "xtask hw: HOME must be set and valid UTF-8, because the installed librepods-ctl is found under it",
        );
        return ExitCode::from(EXIT_USAGE);
    };
    let binary = match link.binary.canonicalize() {
        Ok(binary) if binary.is_file() => binary,
        _ => {
            report("xtask hw: --binary must name an existing file");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    for signal in [SIGINT, SIGTERM, SIGHUP] {
        if let Err(error) = signal_hook::flag::register(signal, &STOP) {
            report(&format!(
                "xtask hw: could not handle signal {signal}: {error}"
            ));
            return ExitCode::from(EXIT_FAILED);
        }
    }
    // Held from before the installed service's state is read until after its restore.
    let lock = match run_lock() {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            report("xtask hw: a run is in progress; nothing was stopped");
            return ExitCode::from(EXIT_USAGE);
        }
        Err(error) => {
            report(&format!("xtask hw: {error}; nothing was stopped"));
            return ExitCode::from(EXIT_USAGE);
        }
    };
    match unit::test_unit_exists() {
        Ok(false) => {}
        Ok(true) => {
            report(&format!(
                "xtask hw: a unit named {} exists, so another run may be going on; nothing was stopped",
                unit::UNDER_TEST
            ));
            return ExitCode::from(EXIT_USAGE);
        }
        Err(error) => {
            report(&format!("xtask hw: {error}; nothing was stopped"));
            return ExitCode::from(EXIT_FAILED);
        }
    }
    let dir = match private_dir() {
        Ok(dir) => dir,
        Err(error) => {
            report(&format!("xtask hw: {error}; nothing was stopped"));
            return ExitCode::from(EXIT_FAILED);
        }
    };
    let was_running = match unit::activity(INSTALLED) {
        Ok(word) => unit::is_running(&word),
        Err(error) => {
            report(&format!("xtask hw: {error}; nothing was stopped"));
            return ExitCode::from(EXIT_FAILED);
        }
    };
    if stopped() {
        report("xtask hw: stopped by a signal; nothing was stopped");
        return ExitCode::from(EXIT_FAILED);
    }
    // Declared after `dir`, so on an unwind the guard restores before the run directory goes.
    let guard = Guard::new(was_running);
    let lab = Lab::new(
        &binary,
        dir.path(),
        rustix::process::getuid().as_raw(),
        &home,
        Duration::from_secs(link.idle_window_seconds),
    );
    let outcome = stop_then_check(lab, &link);
    let restored = guard.restore();
    drop(dir);
    let code = finish(outcome, &restored);
    drop(lock);
    code
}

/// Takes the run lock without waiting; `None` when another run holds it.
fn run_lock() -> Result<Option<OwnedFd>, String> {
    let Some(runtime) = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) else {
        return Err("XDG_RUNTIME_DIR must be set, because the run lock lives there".to_owned());
    };
    if !runtime.is_absolute() {
        return Err("XDG_RUNTIME_DIR must be an absolute path".to_owned());
    }
    // Created if absent and never truncated; a symlink in its place is refused.
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let file = rustix::fs::open(runtime.join(RUN_LOCK), flags, Mode::RUSR | Mode::WUSR)
        .map_err(|error| format!("could not open the run lock: {error}"))?;
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(file)),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
        Err(error) => Err(format!("could not take the run lock: {error}")),
    }
}

/// The run's directory, with `run` (0700) and `state` in it; its path goes into a unit property, so it holds no space.
fn private_dir() -> Result<tempfile::TempDir, String> {
    let dir = tempfile::Builder::new()
        .prefix("librepods-hw-")
        .tempdir()
        .map_err(|error| format!("could not create the run directory: {error}"))?;
    let usable = dir
        .path()
        .to_str()
        .is_some_and(|path| !path.contains(char::is_whitespace));
    if !usable {
        return Err("the run directory's path holds a space or is not UTF-8".to_owned());
    }
    for name in ["run", "state"] {
        DirBuilder::new()
            .mode(0o700)
            .create(dir.path().join(name))
            .map_err(|error| format!("could not create the run directory: {error}"))?;
    }
    Ok(dir)
}

/// Stops the installed service, starts the binary under test and runs the checks.
fn stop_then_check(mut lab: Lab<'_>, link: &LinkArgs) -> Outcome {
    if let Err(error) = unit::systemctl_ok(&["stop", INSTALLED]) {
        return Outcome::Failed(error);
    }
    match unit::activity(INSTALLED) {
        Ok(word) if !unit::is_running(&word) => {}
        Ok(word) => return Outcome::Failed(format!("{INSTALLED} is still {word} after its stop")),
        Err(error) => return Outcome::Failed(error),
    }
    if stopped() {
        return Outcome::Interrupted;
    }
    if let Err(error) = unit::start_under_test(lab.binary, lab.dir, lab.uid) {
        return Outcome::Failed(error);
    }
    match lab.wait_present() {
        Ok(Presence::Connected) => {}
        Ok(Presence::Absent) => return Outcome::Absent,
        Ok(Presence::Silent(cause)) => {
            return Outcome::Failed(format!(
                "the daemon under test did not answer status: {cause}"
            ));
        }
        Err(Interrupted) => return Outcome::Interrupted,
    }
    if let Err(error) = lab.find_device() {
        return Outcome::Failed(error);
    }
    match system::headset_profile() {
        Ok(headset) => print_line(&format!(
            "audio card in a headset profile: {}",
            if headset { "yes" } else { "no" }
        )),
        Err(error) => print_line(&format!(
            "audio card in a headset profile: unknown ({error})"
        )),
    }
    run_checks(&lab, link)
}

type CheckFn = fn(&Lab<'_>) -> Result<Check, Interrupted>;

fn run_checks(lab: &Lab<'_>, link: &LinkArgs) -> Outcome {
    // The attended checks come first, so the person is free once they are done.
    let mut plan: Vec<CheckFn> = vec![checks::adoption, |lab| Ok(checks::same_contract(lab))];
    if link.attended {
        plan.push(census::commands);
    }
    if link.adapter_cycle {
        plan.push(adapter::check);
    }
    plan.extend::<[CheckFn; 3]>([checks::stop, checks::reconnect, idle::idle]);
    let mut done = Vec::new();
    for check in plan {
        let Ok(check) = check(lab) else {
            // A stop signal can land after `do disconnect` before `connected` reads false; the run still leaves the AirPods connected.
            if (lab.disconnect_sent() || lab.connected() == Some(false))
                && let Err(error) = lab.act("connect")
            {
                report(&format!(
                    "xtask hw: the AirPods were left disconnected: {error}"
                ));
            }
            return Outcome::Interrupted;
        };
        for line in report::lines(std::slice::from_ref(&check)) {
            print_line(&line);
        }
        done.push(check);
    }
    Outcome::Checked(done)
}

/// Prints how the run ended and picks the exit status; a failed restore is printed last.
fn finish(outcome: Outcome, restored: &Result<(), String>) -> ExitCode {
    let status = match outcome {
        Outcome::Checked(checks) if report::all_passed(&checks) => {
            print_line("result: every check passed");
            EXIT_PASSED
        }
        Outcome::Checked(_) => {
            print_line("result: FAILED");
            EXIT_FAILED
        }
        Outcome::Absent => {
            report(&format!(
                "xtask hw: the AirPods did not connect within {} s; nothing was measured",
                CONNECT_WAIT.as_secs()
            ));
            EXIT_ABSENT
        }
        Outcome::Interrupted => {
            report("xtask hw: stopped by a signal");
            EXIT_FAILED
        }
        Outcome::Failed(error) => {
            report(&format!("xtask hw: {error}"));
            EXIT_FAILED
        }
    };
    report(&unit::restored_line(restored));
    if restored.is_ok() {
        ExitCode::from(status)
    } else {
        ExitCode::from(EXIT_NOT_RESTORED)
    }
}

/// Help goes to stdout and succeeds; a parse error goes to stderr with the usage status.
fn usage(error: &clap::Error) -> ExitCode {
    let text = error.render().to_string();
    if error.use_stderr() {
        report(text.trim_end());
        ExitCode::from(EXIT_USAGE)
    } else {
        print_line(text.trim_end());
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::Hw;
    use clap::Parser;

    #[test]
    fn the_adapter_cycle_needs_a_person_at_the_machine() {
        assert!(Hw::try_parse_from(["hw", "link", "--binary", "x", "--adapter-cycle"]).is_err());
        assert!(
            Hw::try_parse_from([
                "hw",
                "link",
                "--binary",
                "x",
                "--attended",
                "--adapter-cycle"
            ])
            .is_ok()
        );
    }
}
