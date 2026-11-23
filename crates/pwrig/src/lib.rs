//! Test support: a private `pipewire` server a test starts in its own directory, with stock recorders and a WAV reader.
#![expect(
    clippy::panic,
    clippy::missing_panics_doc,
    reason = "a test tool: every failure ends the test that called it, with a line saying why"
)]
#![cfg_attr(
    test,
    expect(
        dead_code_pub_in_binary,
        reason = "the unit tests cover the WAV reader; the tests under tests/ drive the server and the recorders"
    )
)]

pub mod pod;
mod recorder;
mod server;
pub mod tone;
mod wav;

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::marker::PhantomData;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};

pub use recorder::Recorder;
pub use server::{DRIVER_NODE, Server, environment, node_names, parse_dump};
pub use wav::write_wav;

/// Every program the rig runs comes in this package, on Debian and on Ubuntu.
const PACKAGE: &str = "pipewire-bin";

/// How often a wait checks its condition again.
const POLL: Duration = Duration::from_millis(10);

const BYTES_PER_MIB: u64 = 1024 * 1024;
/// The most a stock tool may print to one stream; pw-dump of the busiest graph a test makes printed 59 KB.
const TOOL_OUTPUT_LIMIT: u64 = BYTES_PER_MIB;

/// A child of a server, killed and waited for when dropped; the borrow makes the server end last.
#[derive(Debug)]
pub struct Process<'server> {
    child: Child,
    program: String,
    /// Where the child's standard error goes, quoted when it fails.
    log: PathBuf,
    /// Set once a caller took the exit status, so the drop leaves judging it to that caller.
    status_taken: bool,
    server: PhantomData<&'server Server>,
}

impl Process<'_> {
    /// Starts `command` with its standard error written to `log`.
    fn start(command: &mut Command, stdout: Stdio, log: PathBuf) -> Self {
        let errors =
            File::create(&log).unwrap_or_else(|error| panic!("{}: {error}", log.display()));
        let child = spawn(command.stdout(stdout).stderr(errors));
        Self {
            child,
            program: command.get_program().to_string_lossy().into_owned(),
            log,
            status_taken: false,
            server: PhantomData,
        }
    }

    /// The exit status once the child exits by itself, or `None` once `within` passes.
    pub fn wait(&mut self, within: Duration) -> Option<ExitStatus> {
        let status = wait_until(within, || {
            self.child
                .try_wait()
                .unwrap_or_else(|error| panic!("waiting for `{}`: {error}", self.program))
        });
        self.status_taken |= status.is_some();
        status
    }

    /// Sends `signal` and waits for the exit, failing on a child that already exited or once `within` passes.
    fn signal(&mut self, signal: Signal, within: Duration) -> ExitStatus {
        self.status_taken = true;
        let exited = self
            .child
            .try_wait()
            .unwrap_or_else(|error| panic!("waiting for `{}`: {error}", self.program));
        if let Some(status) = exited {
            panic!(
                "`{}` exited with {status} on its own, before {signal:?}; its log: {}",
                self.program,
                self.log()
            );
        }
        signal_and_wait(&mut self.child, signal, within)
    }

    /// What the child wrote to its standard error, or why it cannot be read.
    #[must_use]
    pub fn log(&self) -> String {
        log_text(&self.log)
    }
}

impl Drop for Process<'_> {
    fn drop(&mut self) {
        let status = end(&mut self.child);
        let failed_unseen = status.filter(|status| {
            !status.success()
                && status.signal() != Some(Signal::KILL.as_raw())
                && !self.status_taken
        });
        // A test already failing keeps its own message.
        if let Some(status) = failed_unseen
            && !thread::panicking()
        {
            panic!(
                "`{}` exited with {status} on its own; its log: {}",
                self.program,
                self.log()
            );
        }
    }
}

/// Starts `command`, failing with the package to install when its program is missing.
fn spawn(command: &mut Command) -> Child {
    command.spawn().unwrap_or_else(|error| {
        let program = command.get_program().to_string_lossy();
        assert!(
            error.kind() != ErrorKind::NotFound,
            "`{program}` is not on the PATH; it comes with the package {PACKAGE}"
        );
        panic!("`{program}` did not start: {error}")
    })
}

/// Kills `child` and waits for it, giving the status of the kill or of an exit that came first.
fn end(child: &mut Child) -> Option<ExitStatus> {
    // The kill fails only on a child already reaped, whose status `wait` still gives; a wait error leaves nothing to judge.
    drop(child.kill());
    child.wait().ok()
}

/// The first `limit` bytes of `path` and whether it holds more, or the read error naming the path.
fn read_head(path: &Path, limit: u64) -> Result<(Vec<u8>, bool), String> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(limit.saturating_add(1)).read_to_end(&mut bytes))
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let cut = u64::try_from(bytes.len()).map_or(true, |count| count > limit);
    bytes.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    Ok((bytes, cut))
}

/// All of `path`, or why not: the read error, or more than `limit` bytes, each naming the path.
fn read_at_most(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    match read_head(path, limit)? {
        (_, true) => Err(format!("{} holds more than {limit} bytes", path.display())),
        (bytes, false) => Ok(bytes),
    }
}

/// A stock tool's output, failing with the tool's name, the path and the reason when it cannot be read whole.
fn tool_output(program: &str, path: &Path) -> String {
    let bytes = read_at_most(path, TOOL_OUTPUT_LIMIT)
        .unwrap_or_else(|reason| panic!("`{program}`: {reason}"));
    String::from_utf8(bytes)
        .unwrap_or_else(|error| panic!("`{program}` printed {}: {error}", path.display()))
}

/// A child's log for a failure message: its text, cut at the limit with a note, or the path and why it cannot be read.
fn log_text(path: &Path) -> String {
    match read_head(path, TOOL_OUTPUT_LIMIT) {
        Ok((bytes, false)) => String::from_utf8_lossy(&bytes).into_owned(),
        Ok((bytes, true)) => format!(
            "{}, cut at {TOOL_OUTPUT_LIMIT} bytes",
            String::from_utf8_lossy(&bytes)
        ),
        Err(reason) => format!("unreadable, {reason}"),
    }
}

/// Sends `signal` and waits for the exit; kills the child and fails once `within` passes.
fn signal_and_wait(child: &mut Child, signal: Signal, within: Duration) -> ExitStatus {
    let pid = Pid::from_child(child);
    kill_process(pid, signal).unwrap_or_else(|error| panic!("{signal:?} to {pid:?}: {error}"));
    let status = wait_until(within, || {
        child
            .try_wait()
            .unwrap_or_else(|error| panic!("waiting for {pid:?}: {error}"))
    });
    status.unwrap_or_else(|| {
        end(child);
        panic!("{pid:?} still running {within:?} after {signal:?}")
    })
}

/// Polls `ready` until it gives a value, or gives `None` once `within` passes.
fn wait_until<T>(within: Duration, mut ready: impl FnMut() -> Option<T>) -> Option<T> {
    let started = Instant::now();
    loop {
        if let Some(value) = ready() {
            return Some(value);
        }
        if started.elapsed() >= within {
            return None;
        }
        pause();
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "std has no timed wait for a child process or a file, and this is a test tool, not the core"
)]
fn pause() {
    thread::sleep(POLL);
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{TOOL_OUTPUT_LIMIT, log_text, read_at_most};

    /// A limit small enough to write past in a test.
    const LIMIT: u64 = 16;

    #[test]
    fn a_file_of_exactly_the_limit_is_read_and_one_byte_more_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output");
        let at_limit = vec![7; usize::try_from(LIMIT).unwrap()];
        fs::write(&path, &at_limit).unwrap();
        assert_eq!(read_at_most(&path, LIMIT).unwrap(), at_limit);
        fs::write(&path, [at_limit.as_slice(), &[7]].concat()).unwrap();
        assert_eq!(
            read_at_most(&path, LIMIT).unwrap_err(),
            format!("{} holds more than {LIMIT} bytes", path.display())
        );
    }

    #[test]
    fn a_file_that_cannot_be_read_names_its_path_and_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing");
        let reason = read_at_most(&path, LIMIT).unwrap_err();
        assert!(
            reason.starts_with(&format!("{}: ", path.display())),
            "{reason}"
        );
        assert!(reason.contains("No such file or directory"), "{reason}");
    }

    #[test]
    fn a_log_past_the_limit_keeps_its_head_and_says_where_it_was_cut() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool.log");
        let head = vec![b'a'; usize::try_from(TOOL_OUTPUT_LIMIT).unwrap()];
        fs::write(&path, [head.as_slice(), b"b"].concat()).unwrap();
        let text = log_text(&path);
        assert_eq!(
            text,
            format!(
                "{}, cut at {TOOL_OUTPUT_LIMIT} bytes",
                String::from_utf8(head).unwrap()
            )
        );
    }
}
