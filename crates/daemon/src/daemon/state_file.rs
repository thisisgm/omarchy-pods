//! The state file the plugin watches: replaced whole on every change, removed at stop.

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pods_core::state::State;
use pods_core::status::to_status_line;
use tokio::sync::watch;
use tokio::task::{JoinError, spawn_blocking};
use tokio::time::sleep;

use super::owner_file::{self, Failures, WriteError, same_cause};

/// As `daemon/main.cpp` names it.
const STATE_DIR_NAME: &str = "librepods";

/// As `daemon/main.cpp` names it.
const STATE_FILE_NAME: &str = "status.json";

/// Qt's `GenericStateLocation` fallback when `XDG_STATE_HOME` cannot place it.
const HOME_STATE_DIR: &str = ".local/state";

/// The C++ state writer's one second timer.
const RETRY_DELAY: Duration = Duration::from_secs(1);

// Sample: `XDG_STATE_HOME=/home/u/.local/state`, or `HOME=/home/u` if unset, empty or relative.
pub(crate) fn path(xdg_state_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    Some(
        xdg_dir(xdg_state_home, home, HOME_STATE_DIR)?
            .join(STATE_DIR_NAME)
            .join(STATE_FILE_NAME),
    )
}

/// An absolute XDG variable's directory, else `home_relative` under a non empty `HOME`.
// Sample: `/home/u/.config` from `XDG_CONFIG_HOME=/home/u/.config`, or from `HOME=/home/u` and `.config`.
pub(super) fn xdg_dir(
    xdg: Option<&OsStr>,
    home: Option<&OsStr>,
    home_relative: &str,
) -> Option<PathBuf> {
    match xdg.map(Path::new).filter(|dir| dir.is_absolute()) {
        Some(dir) => Some(dir.to_path_buf()),
        None => Some(Path::new(home.filter(|home| !home.is_empty())?).join(home_relative)),
    }
}

#[derive(Debug)]
pub(super) struct StateFileError {
    path: PathBuf,
    failure: Failure,
}

#[derive(Debug)]
enum Failure {
    Write(WriteError),
    PublishUnfinished(JoinError),
    Remove(io::Error),
    RemoveUnfinished(JoinError),
}

impl fmt::Display for StateFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.path.display();
        let publish = "cannot publish the state file";
        match &self.failure {
            Failure::Write(step) => write!(formatter, "{publish} {path}: {step}"),
            Failure::PublishUnfinished(cause) => write!(
                formatter,
                "{publish} {path}: the write did not finish: {cause}"
            ),
            Failure::Remove(cause) => {
                write!(formatter, "cannot remove the state file {path}: {cause}")
            }
            Failure::RemoveUnfinished(cause) => write!(
                formatter,
                "cannot remove the state file {path}: the removal did not finish: {cause}"
            ),
        }
    }
}

impl PartialEq for StateFileError {
    /// One writer has one path, so the failure alone decides.
    fn eq(&self, other: &Self) -> bool {
        self.failure == other.failure
    }
}

impl PartialEq for Failure {
    /// The same step with the same cause, which is what makes the logged sentence the same.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Write(one), Self::Write(other)) => one == other,
            (Self::Remove(cause), Self::Remove(other_cause)) => same_cause(cause, other_cause),
            // Each write is its own task, so the failure of another write never matches.
            (Self::PublishUnfinished(cause), Self::PublishUnfinished(other_cause))
            | (Self::RemoveUnfinished(cause), Self::RemoveUnfinished(other_cause)) => {
                cause.id() == other_cause.id()
            }
            (
                Self::Write(_)
                | Self::PublishUnfinished(_)
                | Self::Remove(_)
                | Self::RemoveUnfinished(_),
                _,
            ) => false,
        }
    }
}

/// Shared by the writer and the shutdown so no write lands after the removal.
#[derive(Debug, Clone)]
pub(super) struct StateFile {
    path: PathBuf,
    removed: Arc<Mutex<bool>>,
}

impl StateFile {
    pub(super) fn new(path: PathBuf) -> Self {
        Self {
            path,
            removed: Arc::new(Mutex::new(false)),
        }
    }

    fn publish_now(&self, line: &str) -> Result<(), StateFileError> {
        let removed = self.removed.lock().unwrap_or_else(PoisonError::into_inner);
        if *removed {
            return Ok(());
        }
        owner_file::write(&self.path, line.as_bytes())
            .map_err(|step| self.error(Failure::Write(step)))
    }

    pub(super) async fn publish(&self, line: String) -> Result<(), StateFileError> {
        let file = self.clone();
        spawn_blocking(move || file.publish_now(&line))
            .await
            .map_err(|cause| self.error(Failure::PublishUnfinished(cause)))?
    }

    /// Stops every later write, waiting for one in progress.
    pub(super) async fn remove(&self) -> Result<(), StateFileError> {
        let file = self.clone();
        spawn_blocking(move || file.remove_now())
            .await
            .map_err(|cause| self.error(Failure::RemoveUnfinished(cause)))?
    }

    fn remove_now(&self) -> Result<(), StateFileError> {
        let mut removed = self.removed.lock().unwrap_or_else(PoisonError::into_inner);
        *removed = true;
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(self.error(Failure::Remove(error))),
        }
    }

    pub(super) fn failures(&self) -> Failures<StateFileError> {
        Failures::new(format!(
            "state file {} is published again",
            self.path.display()
        ))
    }

    fn error(&self, failure: Failure) -> StateFileError {
        StateFileError {
            path: self.path.clone(),
            failure,
        }
    }
}

/// Retries a failed write, the first one included, every `RETRY_DELAY`.
pub(super) async fn follow(
    mut updates: watch::Receiver<State>,
    file: StateFile,
    mut failures: Failures<StateFileError>,
) {
    loop {
        if failures.failing() {
            sleep(RETRY_DELAY).await;
            updates.mark_changed();
        }
        if updates.changed().await.is_err() {
            return;
        }
        let line = to_status_line(&updates.borrow_and_update());
        failures.note(file.publish(line).await);
    }
}

#[cfg(test)]
mod tests {

    use cpp_source::MAIN;
    use rustix::io::Errno;
    use tokio::runtime::Builder;

    use super::*;

    #[test]
    fn names_are_the_cpp_state_file_path() {
        let cpp = MAIN.all_between("QStringLiteral(\"/", "\")");
        let rust = format!("{STATE_DIR_NAME}/{STATE_FILE_NAME}");
        assert!(cpp.contains(&rust.as_str()), "{cpp:?}");
    }

    // Where Qt 6.8.2 `GenericStateLocation` lands, measured against libQt6Core on this host.

    #[test]
    fn absolute_state_home_is_used() {
        let found = path(Some(OsStr::new("/abs/state")), Some(OsStr::new("/h/u")));
        assert_eq!(
            found.unwrap(),
            Path::new("/abs/state/librepods/status.json")
        );
    }

    #[test]
    fn unset_empty_or_relative_state_home_falls_back_to_home() {
        for state_home in [None, Some(""), Some("rel/state")] {
            let found = path(state_home.map(OsStr::new), Some(OsStr::new("/h/u")));
            let expected = Path::new("/h/u/.local/state/librepods/status.json");
            assert_eq!(found.unwrap(), expected, "{state_home:?}");
        }
    }

    #[test]
    fn no_state_home_and_no_home_has_no_path() {
        assert_eq!(path(None, None), None);
        assert_eq!(path(Some(OsStr::new("rel")), Some(OsStr::new(""))), None);
    }

    fn state_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path()
            .join("state")
            .join(STATE_DIR_NAME)
            .join(STATE_FILE_NAME)
    }

    const PATH: &str = "/s/librepods/status.json";

    fn error(failure: Failure) -> StateFileError {
        StateFileError {
            path: PathBuf::from(PATH),
            failure,
        }
    }

    fn cancelled() -> JoinError {
        let runtime = Builder::new_current_thread().build().unwrap();
        let task = runtime.spawn(std::future::pending::<()>());
        task.abort();
        runtime.block_on(task).unwrap_err()
    }

    #[test]
    fn a_failure_is_logged_when_it_starts_changes_and_ends() {
        let mut failures = StateFile::new(PathBuf::from(PATH)).failures();
        let inspect = |errno: Errno| Err(error(Failure::Write(WriteError::Inspect(errno.into()))));
        let denied = "cannot publish the state file /s/librepods/status.json: inspecting it: Permission denied (os error 13)";
        let not_permitted = "cannot publish the state file /s/librepods/status.json: inspecting it: Operation not permitted (os error 1)";
        assert_eq!(failures.message(Ok(())), None);
        assert_eq!(
            failures.message(inspect(Errno::ACCESS)).as_deref(),
            Some(denied)
        );
        assert_eq!(failures.message(inspect(Errno::ACCESS)), None);
        assert!(failures.failing());
        let changed = failures.message(inspect(Errno::PERM));
        assert_eq!(changed.as_deref(), Some(not_permitted));
        let custom = |text: &str| {
            Err(error(Failure::Write(WriteError::Inspect(
                io::Error::other(text),
            ))))
        };
        assert!(failures.message(custom("one")).is_some());
        assert_eq!(failures.message(custom("one")), None);
        assert!(failures.message(custom("two")).is_some());
        for _ in 0..2 {
            let unfinished = Err(error(Failure::PublishUnfinished(cancelled())));
            assert!(failures.message(unfinished).is_some());
        }
        let recovered = "state file /s/librepods/status.json is published again";
        assert_eq!(failures.message(Ok(())), Some(recovered.to_owned()));
        assert!(!failures.failing());
        assert_eq!(failures.message(Ok(())), None);
    }

    #[test]
    fn each_failure_reads_as_the_sentence_the_journal_has_always_shown() {
        let dir = PathBuf::from("/s/librepods");
        let copy = PathBuf::from("/s/librepods/status.json.tmp");
        let denied = || io::Error::from(Errno::ACCESS);
        let publish = "cannot publish the state file /s/librepods/status.json";
        let cases = [
            (
                Failure::Write(WriteError::TooLong(65537)),
                "refusing the line: 65537 bytes is over 65536",
            ),
            (
                Failure::Write(WriteError::NoDirectory),
                "finding its directory: the path has none",
            ),
            (
                Failure::Write(WriteError::CreateDirectory(dir, denied())),
                "creating /s/librepods: Permission denied (os error 13)",
            ),
            (
                Failure::Write(WriteError::Inspect(denied())),
                "inspecting it: Permission denied (os error 13)",
            ),
            (
                Failure::Write(WriteError::Link),
                "refusing to replace it: it is a symbolic link, and its target stays untouched",
            ),
            (
                Failure::Write(WriteError::ClearCopy(copy.clone(), denied())),
                "clearing /s/librepods/status.json.tmp: Permission denied (os error 13)",
            ),
            (
                Failure::Write(WriteError::CreateCopy(copy.clone(), denied())),
                "creating /s/librepods/status.json.tmp: Permission denied (os error 13)",
            ),
            (
                Failure::Write(WriteError::WriteCopy(copy.clone(), denied())),
                "writing /s/librepods/status.json.tmp: Permission denied (os error 13)",
            ),
            (
                Failure::Write(WriteError::RenameCopy(copy, denied())),
                "renaming /s/librepods/status.json.tmp over it: Permission denied (os error 13)",
            ),
        ];
        for (failure, step) in cases {
            assert_eq!(error(failure).to_string(), format!("{publish}: {step}"));
        }
        let remove = "cannot remove the state file /s/librepods/status.json";
        let removal = error(Failure::Remove(denied())).to_string();
        assert_eq!(
            removal,
            format!("{remove}: Permission denied (os error 13)")
        );
        let cause = cancelled();
        let unfinished = format!("{publish}: the write did not finish: {cause}");
        assert_eq!(
            error(Failure::PublishUnfinished(cause)).to_string(),
            unfinished
        );
        let cause = cancelled();
        let unfinished = format!("{remove}: the removal did not finish: {cause}");
        assert_eq!(
            error(Failure::RemoveUnfinished(cause)).to_string(),
            unfinished
        );
    }

    #[test]
    fn no_write_lands_after_the_removal() {
        let dir = tempfile::tempdir().unwrap();
        let file = StateFile::new(state_path(&dir));
        file.publish_now("one\n").unwrap();
        file.remove_now().unwrap();
        file.publish_now("two\n").unwrap();
        assert!(!state_path(&dir).exists());
    }
}
