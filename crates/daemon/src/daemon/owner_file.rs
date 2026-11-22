//! A small file only its owner reads: replaced whole through a fresh copy, read back within a bound.

use std::fmt;
use std::fs::{self, DirBuilder, File, Permissions};
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags, fsync, open};
use rustix::io::Errno;

use super::log;

/// A larger file is neither written nor read.
pub(super) const FILE_MAX: u64 = 65536;

const TEMP_SUFFIX: &str = ".tmp";

const FILE_MODE: u32 = 0o600;

const MODE_BITS: u32 = 0o7777;

const DIR_MODE: u32 = 0o700;

/// The step of a write that failed, worded as the state file's journal has always shown it.
#[derive(Debug)]
pub(super) enum WriteError {
    TooLong(usize),
    NoDirectory,
    CreateDirectory(PathBuf, io::Error),
    Inspect(io::Error),
    Link,
    ClearCopy(PathBuf, io::Error),
    CreateCopy(PathBuf, io::Error),
    WriteCopy(PathBuf, io::Error),
    RenameCopy(PathBuf, io::Error),
    SyncDirectory(PathBuf, io::Error),
}

impl fmt::Display for WriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong(bytes) => write!(
                formatter,
                "refusing the line: {bytes} bytes is over {FILE_MAX}"
            ),
            Self::NoDirectory => write!(formatter, "finding its directory: the path has none"),
            Self::CreateDirectory(dir, cause) => {
                write!(formatter, "creating {}: {cause}", dir.display())
            }
            Self::Inspect(cause) => write!(formatter, "inspecting it: {cause}"),
            Self::Link => write!(
                formatter,
                "refusing to replace it: it is a symbolic link, and its target stays untouched"
            ),
            Self::ClearCopy(copy, cause) => {
                write!(formatter, "clearing {}: {cause}", copy.display())
            }
            Self::CreateCopy(copy, cause) => {
                write!(formatter, "creating {}: {cause}", copy.display())
            }
            Self::WriteCopy(copy, cause) => {
                write!(formatter, "writing {}: {cause}", copy.display())
            }
            Self::RenameCopy(copy, cause) => {
                write!(formatter, "renaming {} over it: {cause}", copy.display())
            }
            Self::SyncDirectory(dir, cause) => {
                write!(formatter, "syncing {}: {cause}", dir.display())
            }
        }
    }
}

impl PartialEq for WriteError {
    /// The same step with the same cause, which is what makes the logged sentence the same.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::TooLong(one), Self::TooLong(other)) => one == other,
            (Self::NoDirectory, Self::NoDirectory) | (Self::Link, Self::Link) => true,
            (Self::CreateDirectory(one, cause), Self::CreateDirectory(other, other_cause))
            | (Self::ClearCopy(one, cause), Self::ClearCopy(other, other_cause))
            | (Self::CreateCopy(one, cause), Self::CreateCopy(other, other_cause))
            | (Self::WriteCopy(one, cause), Self::WriteCopy(other, other_cause))
            | (Self::RenameCopy(one, cause), Self::RenameCopy(other, other_cause))
            | (Self::SyncDirectory(one, cause), Self::SyncDirectory(other, other_cause)) => {
                one == other && same_cause(cause, other_cause)
            }
            (Self::Inspect(cause), Self::Inspect(other_cause)) => same_cause(cause, other_cause),
            (
                Self::TooLong(_)
                | Self::NoDirectory
                | Self::CreateDirectory(..)
                | Self::Inspect(_)
                | Self::Link
                | Self::ClearCopy(..)
                | Self::CreateCopy(..)
                | Self::WriteCopy(..)
                | Self::RenameCopy(..)
                | Self::SyncDirectory(..),
                _,
            ) => false,
        }
    }
}

/// Logs a failing write when it starts, when its error changes and when it recovers.
#[derive(Debug)]
pub(super) struct Failures<E> {
    recovered: String,
    last: Option<E>,
}

impl<E: PartialEq + fmt::Display> Failures<E> {
    /// `recovered` is the line logged when a write succeeds after a failure.
    pub(super) fn new(recovered: String) -> Self {
        Self {
            recovered,
            last: None,
        }
    }

    pub(super) fn note(&mut self, result: Result<(), E>) {
        if let Some(message) = self.message(result) {
            log(&message);
        }
    }

    pub(super) fn message(&mut self, result: Result<(), E>) -> Option<String> {
        match result {
            Ok(()) => self.last.take().map(|_| self.recovered.clone()),
            Err(error) if self.last.as_ref() == Some(&error) => None,
            Err(error) => {
                let message = error.to_string();
                self.last = Some(error);
                Some(message)
            }
        }
    }

    pub(super) fn failing(&self) -> bool {
        self.last.is_some()
    }
}

pub(super) fn same_cause(one: &io::Error, other: &io::Error) -> bool {
    match (one.raw_os_error(), other.raw_os_error()) {
        (Some(one), Some(other)) => one == other,
        (None, None) => one.to_string() == other.to_string(),
        (Some(_), None) | (None, Some(_)) => false,
    }
}

/// A fresh 0600 copy, synced, renamed over, its directory synced; a link at either is never followed, one above them is.
pub(super) fn write(path: &Path, bytes: &[u8]) -> Result<(), WriteError> {
    if u64::try_from(bytes.len()).map_or(true, |len| len > FILE_MAX) {
        return Err(WriteError::TooLong(bytes.len()));
    }
    let dir = path.parent().ok_or(WriteError::NoDirectory)?;
    DirBuilder::new()
        .recursive(true)
        .mode(DIR_MODE)
        .create(dir)
        .map_err(|cause| WriteError::CreateDirectory(dir.to_path_buf(), cause))?;
    match fs::symlink_metadata(path) {
        Ok(found) if found.file_type().is_symlink() => return Err(WriteError::Link),
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(WriteError::Inspect(error)),
    }
    let mut temp_name = path.as_os_str().to_os_string();
    temp_name.push(TEMP_SUFFIX);
    let temp = PathBuf::from(temp_name);
    // A copy left by a crash would make the exclusive create below fail on every later write.
    match fs::remove_file(&temp) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(WriteError::ClearCopy(temp, error)),
    }
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut copy = match open(&temp, flags, Mode::from_raw_mode(FILE_MODE)) {
        Ok(copy) => File::from(copy),
        Err(errno) => return Err(WriteError::CreateCopy(temp, errno.into())),
    };
    if let Err(error) = copy.write_all(bytes).and_then(|()| copy.sync_all()) {
        drop(fs::remove_file(&temp));
        return Err(WriteError::WriteCopy(temp, error));
    }
    if let Err(error) = fs::rename(&temp, path) {
        drop(fs::remove_file(&temp));
        return Err(WriteError::RenameCopy(temp, error));
    }
    // The rename is an entry in the directory, so only the directory's sync keeps it through a power loss.
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    open(dir, flags, Mode::empty())
        .and_then(|opened| fsync(&opened))
        .map_err(|errno| WriteError::SyncDirectory(dir.to_path_buf(), errno.into()))
}

/// `None` when absent; a link, a directory, a pipe or a file over `FILE_MAX` is an error, and none of them is followed or waited on.
pub(super) fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    open_regular(path)?.map(read_bounded).transpose()
}

/// As `read`, once the file's mode is the owner's alone.
pub(super) fn read_private(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let Some(file) = open_regular(path)? else {
        return Ok(None);
    };
    if file.metadata()?.permissions().mode() & MODE_BITS != FILE_MODE {
        file.set_permissions(Permissions::from_mode(FILE_MODE))?;
    }
    read_bounded(file).map(Some)
}

fn open_regular(path: &Path) -> io::Result<Option<File>> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = match open(path, flags, Mode::empty()) {
        Ok(file) => File::from(file),
        Err(Errno::NOENT) => return Ok(None),
        Err(errno) => return Err(errno.into()),
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("it is not a regular file"));
    }
    Ok(Some(file))
}

fn read_bounded(file: File) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(FILE_MAX.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_or(true, |len| len > FILE_MAX) {
        return Err(io::Error::other(format!("it is over {FILE_MAX} bytes")));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
