//! The private store for probe captures: owner only, outside any git repository, never through a symbolic link.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "init opens only a session; the capture commands use the rest"
    )
)]

use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};

use pods_proto::addr::{BdAddr, BdAddrError};
use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags};
use rustix::io::Errno;

/// The store's directory under the home directory.
const ROOT_NAME: &str = "omapods-captures";
/// The marker a git repository or work tree keeps in its top directory.
const GIT_MARKER: &str = ".git";
/// The device file's name inside the root.
const DEVICE_NAME: &str = "device";
/// Root, session and run directories: owner read, write and search, nothing else.
const DIR_MODE: Mode = Mode::from_raw_mode(0o700);
/// Capture files and the device file: owner read and write, nothing else.
const FILE_MODE: Mode = Mode::from_raw_mode(0o600);
/// The permission bits of `st_mode`, setuid, setgid and sticky included.
const PERMISSION_BITS: u32 = 0o7777;
/// Group and other permission bits; a capture read refuses a file with any of them.
const GROUP_OTHER_BITS: u32 = 0o077;
/// A capture read is refused above 64 MiB.
const MAX_READ_BYTES: u64 = 64 * 1024 * 1024;
/// The device file is refused above 64 bytes.
const MAX_DEVICE_BYTES: u64 = 64;
/// The shape of `date -u +%Y%m%dT%H%M%SZ`, where each `DIGIT_SLOT` stands for one ASCII digit.
const STAMP_SHAPE: &[u8] = b"########T######Z";
/// The byte of `STAMP_SHAPE` that stands for one digit.
const DIGIT_SLOT: u8 = b'#';
/// A stamp is exactly this many bytes.
const STAMP_LEN: usize = STAMP_SHAPE.len();
/// A run name is at most this many characters.
const RUN_NAME_MAX: usize = 32;

/// What the store expects to find at a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Directory,
    RegularFile,
}

impl Kind {
    fn file_type(self) -> FileType {
        match self {
            Kind::Directory => FileType::Directory,
            Kind::RegularFile => FileType::RegularFile,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Kind::Directory => f.write_str("directory"),
            Kind::RegularFile => f.write_str("regular file"),
        }
    }
}

/// The permission bits a checked path may carry.
#[derive(Debug, Clone, Copy)]
enum ModeRule {
    /// These bits and no others.
    Exact(Mode),
    /// Any owner bits, no group or other bits.
    OwnerOnly,
}

/// Why the store refused; each names the path or the input and the rule it broke, never a byte read from a file.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreError {
    #[error(
        "stamp {stamp:?} is not of the form YYYYMMDDTHHMMSSZ; take it from date -u +%Y%m%dT%H%M%SZ"
    )]
    BadStamp { stamp: String },
    #[error("run name {name:?} is not 1 to {RUN_NAME_MAX} characters of a-z, 0-9 and -")]
    BadRunName { name: String },
    #[error(
        "file name {name:?} is not a plain name: it must not be empty, . or .., nor hold / or a NUL byte"
    )]
    BadFileName { name: String },
    #[error("{} is not an absolute path below a parent directory", .path.display())]
    BadRoot { path: PathBuf },
    #[error("{} is inside a git repository ({} exists); captures must never be committed", .path.display(), .marker.display())]
    InRepository { path: PathBuf, marker: PathBuf },
    #[error("{} is a symbolic link; the store never follows one", .path.display())]
    Symlink { path: PathBuf },
    #[error("{} is not a {expected}", .path.display())]
    WrongType { path: PathBuf, expected: Kind },
    #[error("{} is owned by uid {owner}, not by the caller, uid {caller}", .path.display())]
    WrongOwner {
        path: PathBuf,
        owner: u32,
        caller: u32,
    },
    #[error("{} has mode {mode:04o}, expected exactly {expected:04o}; set it with chmod {expected:o}", .path.display())]
    WrongMode {
        path: PathBuf,
        mode: u32,
        expected: u32,
    },
    #[error("{} has mode {mode:04o}; a capture is read only when group and others have no access", .path.display())]
    NotOwnerOnly { path: PathBuf, mode: u32 },
    #[error("{} is larger than {limit} bytes", .path.display())]
    TooLarge { path: PathBuf, limit: u64 },
    #[error("{} already exists; a run is never reused", .path.display())]
    RunExists { path: PathBuf },
    #[error("{} already exists; the store never overwrites a file", .path.display())]
    FileExists { path: PathBuf },
    #[error("{} is not UTF-8 text after byte {valid_up_to}; it must hold one Bluetooth address", .path.display())]
    DeviceNotText { path: PathBuf, valid_up_to: usize },
    #[error("{} does not hold one Bluetooth address and an optional newline: {reason}", .path.display())]
    DeviceNotAddress { path: PathBuf, reason: BdAddrError },
    #[error("could not {action} {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        action: &'static str,
        source: io::Error,
    },
}

impl StoreError {
    fn io(path: &Path, action: &'static str, source: impl Into<io::Error>) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            action,
            source: source.into(),
        }
    }
}

/// The store's root under a home directory.
#[must_use]
pub(crate) fn root(home: &OsStr) -> PathBuf {
    Path::new(home).join(ROOT_NAME)
}

/// One recording session: `<root>/<stamp>`, checked and held open.
#[derive(Debug)]
pub(crate) struct Session {
    path: PathBuf,
    dir: OwnedFd,
}

impl Session {
    /// Creates or checks the root and the session directory under it.
    pub(crate) fn open(root: &Path, stamp: &str) -> Result<Session, StoreError> {
        check_stamp(stamp)?;
        refuse_repository_ancestors(root)?;
        let root_dir = make_dir(CWD, root, root)?;
        refuse_git_marker(&root_dir, root)?;
        let path = root.join(stamp);
        let dir = make_dir(root_dir.as_fd(), Path::new(stamp), &path)?;
        Ok(Session { path, dir })
    }

    #[must_use]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// One capture run inside a session, checked and held open.
#[derive(Debug)]
pub(crate) struct Run {
    path: PathBuf,
    dir: OwnedFd,
}

impl Run {
    /// Creates a new run directory; an existing name is refused, never reused.
    pub(crate) fn create(session: &Session, name: &str) -> Result<Run, StoreError> {
        check_run_name(name)?;
        let path = session.path.join(name);
        match rustix::fs::mkdirat(session.dir.as_fd(), name, DIR_MODE) {
            Ok(()) => {}
            Err(Errno::EXIST) => return Err(StoreError::RunExists { path }),
            Err(errno) => return Err(StoreError::io(&path, "create", errno)),
        }
        let dir = open_dir(session.dir.as_fd(), Path::new(name), &path)?;
        Ok(Run { path, dir })
    }

    /// Opens an existing run directory with the same checks as the root.
    pub(crate) fn open(path: &Path) -> Result<Run, StoreError> {
        refuse_repository_ancestors(path)?;
        let dir = open_dir(CWD, path, path)?;
        refuse_git_marker(&dir, path)?;
        Ok(Run {
            path: path.to_path_buf(),
            dir,
        })
    }

    /// Creates a new capture file with mode 0600; an existing name is refused, never overwritten.
    pub(crate) fn create_file(&self, name: &str) -> Result<File, StoreError> {
        check_file_name(name)?;
        let path = self.path.join(name);
        // O_EXCL fails on any existing name, a symbolic link included, so nothing is followed.
        let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC;
        let fd = match rustix::fs::openat(self.dir.as_fd(), name, flags, FILE_MODE) {
            Ok(fd) => fd,
            Err(Errno::EXIST) => return Err(StoreError::FileExists { path }),
            Err(errno) => return Err(StoreError::io(&path, "create", errno)),
        };
        check(&fd, &path, Kind::RegularFile, ModeRule::Exact(FILE_MODE))?;
        Ok(File::from(fd))
    }

    /// Reads a capture file: a regular file, owner only, at most `MAX_READ_BYTES`.
    pub(crate) fn read(&self, name: &str) -> Result<Vec<u8>, StoreError> {
        check_file_name(name)?;
        let path = self.path.join(name);
        read_bounded(
            self.dir.as_fd(),
            name,
            &path,
            ModeRule::OwnerOnly,
            MAX_READ_BYTES,
        )
    }
}

/// Reads the owner's device address from `<root>/device`.
pub(crate) fn read_device(root: &Path) -> Result<BdAddr, StoreError> {
    let root_dir = open_dir(CWD, root, root)?;
    let path = root.join(DEVICE_NAME);
    let rule = ModeRule::Exact(FILE_MODE);
    let contents = read_bounded(root_dir.as_fd(), DEVICE_NAME, &path, rule, MAX_DEVICE_BYTES)?;
    parse_device(&path, &contents)
}

// Sample input: b"AA:BB:CC:DD:EE:FF\n", one address and an optional newline.
fn parse_device(path: &Path, contents: &[u8]) -> Result<BdAddr, StoreError> {
    let line = contents.strip_suffix(b"\n").unwrap_or(contents);
    let text = std::str::from_utf8(line).map_err(|error| StoreError::DeviceNotText {
        path: path.to_path_buf(),
        valid_up_to: error.valid_up_to(),
    })?;
    text.parse().map_err(|reason| StoreError::DeviceNotAddress {
        path: path.to_path_buf(),
        reason,
    })
}

// Sample input: 20261120T143005Z, as date -u +%Y%m%dT%H%M%SZ prints it.
fn check_stamp(stamp: &str) -> Result<(), StoreError> {
    let fits = stamp.len() == STAMP_LEN
        && stamp.bytes().zip(STAMP_SHAPE).all(|(byte, &shape)| {
            if shape == DIGIT_SLOT {
                byte.is_ascii_digit()
            } else {
                byte == shape
            }
        });
    if fits {
        Ok(())
    } else {
        Err(StoreError::BadStamp {
            stamp: stamp.to_owned(),
        })
    }
}

// Sample input: left-mic-2, one to 32 characters of a-z, 0-9 and -.
fn check_run_name(name: &str) -> Result<(), StoreError> {
    let fits = (1..=RUN_NAME_MAX).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if fits {
        Ok(())
    } else {
        Err(StoreError::BadRunName {
            name: name.to_owned(),
        })
    }
}

// Sample input: uplink.bin, one name inside the run directory.
fn check_file_name(name: &str) -> Result<(), StoreError> {
    let plain = !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0']);
    if plain {
        Ok(())
    } else {
        Err(StoreError::BadFileName {
            name: name.to_owned(),
        })
    }
}

/// Refuses a store directory whose parent, or any directory above it, holds a git marker.
fn refuse_repository_ancestors(path: &Path) -> Result<(), StoreError> {
    let bad_root = || StoreError::BadRoot {
        path: path.to_path_buf(),
    };
    if !path.is_absolute() {
        return Err(bad_root());
    }
    let parent = path.parent().ok_or_else(bad_root)?;
    // Resolved, so a home reached through a symbolic link is judged by where it really is.
    let real_parent =
        fs::canonicalize(parent).map_err(|source| StoreError::io(parent, "resolve", source))?;
    for dir in real_parent.ancestors() {
        let marker = dir.join(GIT_MARKER);
        match fs::symlink_metadata(&marker) {
            Ok(_) => {
                return Err(StoreError::InRepository {
                    path: path.to_path_buf(),
                    marker,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(StoreError::io(&marker, "look for", error)),
        }
    }
    Ok(())
}

/// Refuses an open store directory that holds a git marker itself.
fn refuse_git_marker(dir: &OwnedFd, path: &Path) -> Result<(), StoreError> {
    let marker = path.join(GIT_MARKER);
    match rustix::fs::statat(dir, GIT_MARKER, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Err(StoreError::InRepository {
            path: path.to_path_buf(),
            marker,
        }),
        Err(Errno::NOENT) => Ok(()),
        Err(errno) => Err(StoreError::io(&marker, "look for", errno)),
    }
}

/// Creates `name` under `dir` with mode 0700 unless it exists, then opens and checks it.
fn make_dir(dir: BorrowedFd<'_>, name: &Path, path: &Path) -> Result<OwnedFd, StoreError> {
    match rustix::fs::mkdirat(dir, name, DIR_MODE) {
        Ok(()) | Err(Errno::EXIST) => open_dir(dir, name, path),
        Err(errno) => Err(StoreError::io(path, "create", errno)),
    }
}

/// Opens `name` under `dir` without following a final symbolic link and checks what was opened.
fn open_dir(dir: BorrowedFd<'_>, name: &Path, path: &Path) -> Result<OwnedFd, StoreError> {
    // O_PATH with O_NOFOLLOW opens a symbolic link itself, so the check below sees it and refuses.
    let flags = OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = rustix::fs::openat(dir, name, flags, Mode::empty())
        .map_err(|errno| StoreError::io(path, "open", errno))?;
    check(&fd, path, Kind::Directory, ModeRule::Exact(DIR_MODE))?;
    Ok(fd)
}

/// Opens a regular file under `dir` without following a symbolic link and reads at most `limit` bytes.
fn read_bounded(
    dir: BorrowedFd<'_>,
    name: &str,
    path: &Path,
    rule: ModeRule,
    limit: u64,
) -> Result<Vec<u8>, StoreError> {
    // O_NONBLOCK keeps a FIFO planted under the name from blocking the open; the check refuses it.
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = match rustix::fs::openat(dir, name, flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(Errno::LOOP) => {
            return Err(StoreError::Symlink {
                path: path.to_path_buf(),
            });
        }
        Err(errno) => return Err(StoreError::io(path, "open", errno)),
    };
    check(&fd, path, Kind::RegularFile, rule)?;
    let mut contents = Vec::new();
    File::from(fd)
        .take(limit.saturating_add(1))
        .read_to_end(&mut contents)
        .map_err(|source| StoreError::io(path, "read", source))?;
    if !u64::try_from(contents.len()).is_ok_and(|len| len <= limit) {
        return Err(StoreError::TooLarge {
            path: path.to_path_buf(),
            limit,
        });
    }
    Ok(contents)
}

/// Checks the open descriptor, not the path, so what was checked is what is used.
fn check(fd: &OwnedFd, path: &Path, kind: Kind, rule: ModeRule) -> Result<(), StoreError> {
    let stat = rustix::fs::fstat(fd).map_err(|errno| StoreError::io(path, "inspect", errno))?;
    let file_type = FileType::from_raw_mode(stat.st_mode);
    if file_type == FileType::Symlink {
        return Err(StoreError::Symlink {
            path: path.to_path_buf(),
        });
    }
    if file_type != kind.file_type() {
        return Err(StoreError::WrongType {
            path: path.to_path_buf(),
            expected: kind,
        });
    }
    let caller = rustix::process::geteuid().as_raw();
    if stat.st_uid != caller {
        return Err(StoreError::WrongOwner {
            path: path.to_path_buf(),
            owner: stat.st_uid,
            caller,
        });
    }
    let mode = stat.st_mode & PERMISSION_BITS;
    match rule {
        ModeRule::Exact(expected) if mode != expected.as_raw_mode() => Err(StoreError::WrongMode {
            path: path.to_path_buf(),
            mode,
            expected: expected.as_raw_mode(),
        }),
        ModeRule::OwnerOnly if mode & GROUP_OTHER_BITS != 0 => Err(StoreError::NotOwnerOnly {
            path: path.to_path_buf(),
            mode,
        }),
        ModeRule::Exact(_) | ModeRule::OwnerOnly => Ok(()),
    }
}

#[cfg(test)]
mod tests;
