//! The state file the plugin watches: one status line, replaced whole on every change, removed when the daemon stops.

use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, DirBuilder, File};
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pods_core::state::State;
use pods_core::status::to_status_line;
use rustix::fs::{Mode, OFlags, open};
use tokio::sync::watch;
use tokio::task::{JoinError, spawn_blocking};
use tokio::time::sleep;

use super::log;

/// The directory under the state home, as `daemon/main.cpp` names it.
const STATE_DIR_NAME: &str = "librepods";

/// The file in that directory, as `daemon/main.cpp` names it.
const STATE_FILE_NAME: &str = "status.json";

/// The state home under `HOME` when `XDG_STATE_HOME` cannot place it, as Qt's `GenericStateLocation` falls back.
const HOME_STATE_DIR: &str = ".local/state";

/// Appended to the state file's name for the copy written before the rename.
const TEMP_SUFFIX: &str = ".tmp";

/// The state file carries the pods' identity, so only its owner reads it.
const STATE_FILE_MODE: u32 = 0o600;

/// Mode of a state directory this daemon creates.
const STATE_DIR_MODE: u32 = 0o700;

/// The longest line published; a longer one is refused, which only an absurd device name could cause.
const MAX_STATE_LINE_BYTES: usize = 65536;

/// The pause before a failed write is tried again, the C++ state writer's one second timer.
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// The state file path, under `XDG_STATE_HOME` when it is absolute and under `HOME` otherwise, as Qt places it.
// Sample input: `XDG_STATE_HOME=/home/u/.local/state`, or it unset, empty or relative with `HOME=/home/u`.
pub(crate) fn path(xdg_state_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    let state_home = match xdg_state_home
        .map(Path::new)
        .filter(|dir| dir.is_absolute())
    {
        Some(dir) => dir.to_path_buf(),
        None => Path::new(home.filter(|home| !home.is_empty())?).join(HOME_STATE_DIR),
    };
    Some(state_home.join(STATE_DIR_NAME).join(STATE_FILE_NAME))
}

/// Why the state file at `path` could not be published or removed.
#[derive(Debug)]
pub(super) struct StateFileError {
    path: PathBuf,
    failure: Failure,
}

/// The step that failed, with the directory or copy it acted on and the cause.
#[derive(Debug)]
enum Failure {
    LineTooLong(usize),
    NoDirectory,
    CreateDirectory(PathBuf, io::Error),
    Inspect(io::Error),
    Link,
    ClearCopy(PathBuf, io::Error),
    CreateCopy(PathBuf, io::Error),
    WriteCopy(PathBuf, io::Error),
    RenameCopy(PathBuf, io::Error),
    PublishUnfinished(JoinError),
    Remove(io::Error),
    RemoveUnfinished(JoinError),
}

impl fmt::Display for StateFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.path.display();
        let publish = "cannot publish the state file";
        match &self.failure {
            Failure::LineTooLong(bytes) => write!(
                formatter,
                "{publish} {path}: refusing the line: {bytes} bytes is over {MAX_STATE_LINE_BYTES}"
            ),
            Failure::NoDirectory => write!(
                formatter,
                "{publish} {path}: finding its directory: the path has none"
            ),
            Failure::CreateDirectory(dir, cause) => write!(
                formatter,
                "{publish} {path}: creating {}: {cause}",
                dir.display()
            ),
            Failure::Inspect(cause) => {
                write!(formatter, "{publish} {path}: inspecting it: {cause}")
            }
            Failure::Link => write!(
                formatter,
                "{publish} {path}: refusing to replace it: it is a symbolic link, and its target stays untouched"
            ),
            Failure::ClearCopy(copy, cause) => write!(
                formatter,
                "{publish} {path}: clearing {}: {cause}",
                copy.display()
            ),
            Failure::CreateCopy(copy, cause) => write!(
                formatter,
                "{publish} {path}: creating {}: {cause}",
                copy.display()
            ),
            Failure::WriteCopy(copy, cause) => write!(
                formatter,
                "{publish} {path}: writing {}: {cause}",
                copy.display()
            ),
            Failure::RenameCopy(copy, cause) => write!(
                formatter,
                "{publish} {path}: renaming {} over it: {cause}",
                copy.display()
            ),
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

impl PartialEq for Failure {
    /// The same step with the same cause, which is what makes the logged sentence the same.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::LineTooLong(one), Self::LineTooLong(other)) => one == other,
            (Self::NoDirectory, Self::NoDirectory) | (Self::Link, Self::Link) => true,
            (Self::CreateDirectory(one, cause), Self::CreateDirectory(other, other_cause))
            | (Self::ClearCopy(one, cause), Self::ClearCopy(other, other_cause))
            | (Self::CreateCopy(one, cause), Self::CreateCopy(other, other_cause))
            | (Self::WriteCopy(one, cause), Self::WriteCopy(other, other_cause))
            | (Self::RenameCopy(one, cause), Self::RenameCopy(other, other_cause)) => {
                one == other && same_cause(cause, other_cause)
            }
            (Self::Inspect(cause), Self::Inspect(other_cause))
            | (Self::Remove(cause), Self::Remove(other_cause)) => same_cause(cause, other_cause),
            // Each write is its own task, so the failure of another write never matches.
            (Self::PublishUnfinished(cause), Self::PublishUnfinished(other_cause))
            | (Self::RemoveUnfinished(cause), Self::RemoveUnfinished(other_cause)) => {
                cause.id() == other_cause.id()
            }
            (
                Self::LineTooLong(_)
                | Self::NoDirectory
                | Self::CreateDirectory(..)
                | Self::Inspect(_)
                | Self::Link
                | Self::ClearCopy(..)
                | Self::CreateCopy(..)
                | Self::WriteCopy(..)
                | Self::RenameCopy(..)
                | Self::PublishUnfinished(_)
                | Self::Remove(_)
                | Self::RemoveUnfinished(_),
                _,
            ) => false,
        }
    }
}

/// Two causes print alike when they carry the same OS error number, or the same message when neither has one.
fn same_cause(one: &io::Error, other: &io::Error) -> bool {
    match (one.raw_os_error(), other.raw_os_error()) {
        (Some(one), Some(other)) => one == other,
        (None, None) => one.to_string() == other.to_string(),
        (Some(_), None) | (None, Some(_)) => false,
    }
}

/// The state file, shared by the writer and the shutdown so no write lands after the removal.
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

    /// Writes `line` on the calling thread, which `publish` keeps off the runtime.
    fn publish_now(&self, line: &str) -> Result<(), StateFileError> {
        let removed = self.removed.lock().unwrap_or_else(PoisonError::into_inner);
        if *removed {
            return Ok(());
        }
        write_atomically(&self.path, line)
    }

    /// Writes `line` on the blocking pool, off the runtime thread.
    pub(super) async fn publish(&self, line: String) -> Result<(), StateFileError> {
        let file = self.clone();
        spawn_blocking(move || file.publish_now(&line))
            .await
            .map_err(|cause| self.error(Failure::PublishUnfinished(cause)))?
    }

    /// Removes the file and stops every later write, waiting for one in progress.
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

    fn error(&self, failure: Failure) -> StateFileError {
        StateFileError {
            path: self.path.clone(),
            failure,
        }
    }
}

/// The last write error, so a write that keeps failing is logged when it starts, when its error changes and when it recovers.
#[derive(Debug, Default)]
pub(super) struct WriteFailures {
    last: Option<Failure>,
}

impl WriteFailures {
    /// Takes the result of one write of `file`, and logs it if it starts, changes or ends a failure.
    pub(super) fn note(&mut self, file: &StateFile, result: Result<(), StateFileError>) {
        if let Some(message) = self.message(&file.path, result) {
            log(&message);
        }
    }

    fn message(&mut self, path: &Path, result: Result<(), StateFileError>) -> Option<String> {
        match result {
            Ok(()) => self
                .last
                .take()
                .map(|_| format!("state file {} is published again", path.display())),
            Err(error) if self.last.as_ref() == Some(&error.failure) => None,
            Err(error) => {
                let message = error.to_string();
                self.last = Some(error.failure);
                Some(message)
            }
        }
    }

    fn failing(&self) -> bool {
        self.last.is_some()
    }
}

/// Publishes every change `updates` announces until aborted, and retries a failed write, the first one included, every `RETRY_DELAY`.
pub(super) async fn follow(
    mut updates: watch::Receiver<State>,
    file: StateFile,
    mut failures: WriteFailures,
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
        failures.note(&file, file.publish(line).await);
    }
}

/// Replaces `path` with `line` through a 0600 copy created fresh beside it, synced, then renamed over it; a link at either is never followed, a linked directory above them is.
fn write_atomically(path: &Path, line: &str) -> Result<(), StateFileError> {
    let fail = |failure| StateFileError {
        path: path.to_path_buf(),
        failure,
    };
    if line.len() > MAX_STATE_LINE_BYTES {
        return Err(fail(Failure::LineTooLong(line.len())));
    }
    let dir = path.parent().ok_or_else(|| fail(Failure::NoDirectory))?;
    DirBuilder::new()
        .recursive(true)
        .mode(STATE_DIR_MODE)
        .create(dir)
        .map_err(|cause| fail(Failure::CreateDirectory(dir.to_path_buf(), cause)))?;
    match fs::symlink_metadata(path) {
        Ok(found) if found.file_type().is_symlink() => return Err(fail(Failure::Link)),
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(fail(Failure::Inspect(error))),
    }
    let mut temp_name = path.as_os_str().to_os_string();
    temp_name.push(TEMP_SUFFIX);
    let temp = PathBuf::from(temp_name);
    // A copy left by a crash would make the exclusive create below fail on every later write.
    match fs::remove_file(&temp) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(fail(Failure::ClearCopy(temp, error))),
    }
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut copy = match open(&temp, flags, Mode::from_raw_mode(STATE_FILE_MODE)) {
        Ok(copy) => File::from(copy),
        Err(errno) => return Err(fail(Failure::CreateCopy(temp, errno.into()))),
    };
    if let Err(error) = copy
        .write_all(line.as_bytes())
        .and_then(|()| copy.sync_all())
    {
        return Err(fail(Failure::WriteCopy(temp, error)));
    }
    fs::rename(&temp, path).map_err(|error| fail(Failure::RenameCopy(temp, error)))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

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

    #[test]
    fn write_creates_the_directory_and_a_0600_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        write_atomically(&path, "{\"a\":1}\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"a\":1}\n");
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, STATE_FILE_MODE);
        let dir_mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, STATE_DIR_MODE);
    }

    #[test]
    fn write_replaces_the_file_with_a_new_inode() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        write_atomically(&path, "one\n").unwrap();
        let first = fs::metadata(&path).unwrap().ino();
        write_atomically(&path, "two\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two\n");
        assert_ne!(fs::metadata(&path).unwrap().ino(), first);
    }

    #[test]
    fn write_refuses_a_symlink_and_leaves_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let target = dir.path().join("target");
        fs::write(&target, "original").unwrap();
        symlink(&target, &path).unwrap();
        let error = write_atomically(&path, "line\n").unwrap_err();
        let sentence = format!(
            "cannot publish the state file {}: refusing to replace it: it is a symbolic link, and its target stays untouched",
            path.display()
        );
        assert_eq!(error.to_string(), sentence);
        assert_eq!(fs::read_to_string(&target).unwrap(), "original");
        assert!(fs::symlink_metadata(&path).unwrap().is_symlink());
    }

    #[test]
    fn write_replaces_a_leftover_copy_without_following_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let target = dir.path().join("target");
        fs::write(&target, "original").unwrap();
        let mut temp = path.as_os_str().to_os_string();
        temp.push(TEMP_SUFFIX);
        symlink(&target, PathBuf::from(&temp)).unwrap();
        write_atomically(&path, "line\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "line\n");
        assert_eq!(fs::read_to_string(&target).unwrap(), "original");
        assert!(!PathBuf::from(temp).exists());
    }

    #[test]
    fn write_goes_through_a_linked_state_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        fs::create_dir(dir.path().join("state")).unwrap();
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        symlink(&real, path.parent().unwrap()).unwrap();
        write_atomically(&path, "line\n").unwrap();
        assert_eq!(
            fs::read_to_string(real.join(STATE_FILE_NAME)).unwrap(),
            "line\n"
        );
    }

    #[test]
    fn write_refuses_a_line_over_the_bound() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        let error = write_atomically(&path, &"x".repeat(MAX_STATE_LINE_BYTES + 1)).unwrap_err();
        let sentence = format!(
            "cannot publish the state file {}: refusing the line: 65537 bytes is over 65536",
            path.display()
        );
        assert_eq!(error.to_string(), sentence);
        assert!(!path.exists());
        write_atomically(&path, &"x".repeat(MAX_STATE_LINE_BYTES)).unwrap();
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
        let path = Path::new(PATH);
        let mut failures = WriteFailures::default();
        let inspect = |errno: Errno| Err(error(Failure::Inspect(errno.into())));
        let denied = "cannot publish the state file /s/librepods/status.json: inspecting it: Permission denied (os error 13)";
        let not_permitted = "cannot publish the state file /s/librepods/status.json: inspecting it: Operation not permitted (os error 1)";
        assert_eq!(failures.message(path, Ok(())), None);
        assert_eq!(
            failures.message(path, inspect(Errno::ACCESS)).as_deref(),
            Some(denied)
        );
        assert_eq!(failures.message(path, inspect(Errno::ACCESS)), None);
        assert!(failures.failing());
        let changed = failures.message(path, inspect(Errno::PERM));
        assert_eq!(changed.as_deref(), Some(not_permitted));
        let custom = |text: &str| Err(error(Failure::Inspect(io::Error::other(text))));
        assert!(failures.message(path, custom("one")).is_some());
        assert_eq!(failures.message(path, custom("one")), None);
        assert!(failures.message(path, custom("two")).is_some());
        for _ in 0..2 {
            let unfinished = Err(error(Failure::PublishUnfinished(cancelled())));
            assert!(failures.message(path, unfinished).is_some());
        }
        let recovered = "state file /s/librepods/status.json is published again";
        assert_eq!(failures.message(path, Ok(())), Some(recovered.to_owned()));
        assert!(!failures.failing());
        assert_eq!(failures.message(path, Ok(())), None);
    }

    #[test]
    fn each_failure_reads_as_the_sentence_the_journal_has_always_shown() {
        let dir = PathBuf::from("/s/librepods");
        let copy = PathBuf::from("/s/librepods/status.json.tmp");
        let denied = || io::Error::from(Errno::ACCESS);
        let publish = "cannot publish the state file /s/librepods/status.json";
        let cases = [
            (
                Failure::LineTooLong(65537),
                "refusing the line: 65537 bytes is over 65536",
            ),
            (
                Failure::NoDirectory,
                "finding its directory: the path has none",
            ),
            (
                Failure::CreateDirectory(dir, denied()),
                "creating /s/librepods: Permission denied (os error 13)",
            ),
            (
                Failure::Inspect(denied()),
                "inspecting it: Permission denied (os error 13)",
            ),
            (
                Failure::Link,
                "refusing to replace it: it is a symbolic link, and its target stays untouched",
            ),
            (
                Failure::ClearCopy(copy.clone(), denied()),
                "clearing /s/librepods/status.json.tmp: Permission denied (os error 13)",
            ),
            (
                Failure::CreateCopy(copy.clone(), denied()),
                "creating /s/librepods/status.json.tmp: Permission denied (os error 13)",
            ),
            (
                Failure::WriteCopy(copy.clone(), denied()),
                "writing /s/librepods/status.json.tmp: Permission denied (os error 13)",
            ),
            (
                Failure::RenameCopy(copy, denied()),
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
