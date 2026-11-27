use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use super::*;

/// A directory its owner can enter and change but not list.
const WRITE_AND_SEARCH: u32 = 0o300;

fn max() -> usize {
    usize::try_from(FILE_MAX).unwrap()
}

fn owned_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("state").join("pods").join("status.json")
}

#[test]
fn write_creates_the_directory_and_a_0600_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = owned_path(&dir);
    write(&path, b"{\"a\":1}\n").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "{\"a\":1}\n");
    let mode = fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, FILE_MODE);
    let dir_mode = fs::metadata(path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(dir_mode & 0o777, DIR_MODE);
}

#[test]
fn write_replaces_the_file_with_a_new_inode() {
    let dir = tempfile::tempdir().unwrap();
    let path = owned_path(&dir);
    write(&path, b"one\n").unwrap();
    let first = fs::metadata(&path).unwrap().ino();
    write(&path, b"two\n").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "two\n");
    assert_ne!(fs::metadata(&path).unwrap().ino(), first);
}

#[test]
fn write_refuses_a_symlink_and_leaves_its_target() {
    let dir = tempfile::tempdir().unwrap();
    let path = owned_path(&dir);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let target = dir.path().join("target");
    fs::write(&target, "original").unwrap();
    symlink(&target, &path).unwrap();
    let error = write(&path, b"line\n").unwrap_err();
    let step = "refusing to replace it: it is a symbolic link, and its target stays untouched";
    assert_eq!(error.to_string(), step);
    assert_eq!(fs::read_to_string(&target).unwrap(), "original");
    assert!(fs::symlink_metadata(&path).unwrap().is_symlink());
}

#[test]
fn write_replaces_a_leftover_copy_without_following_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = owned_path(&dir);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let target = dir.path().join("target");
    fs::write(&target, "original").unwrap();
    let mut temp = path.as_os_str().to_os_string();
    temp.push(TEMP_SUFFIX);
    symlink(&target, PathBuf::from(&temp)).unwrap();
    write(&path, b"line\n").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "line\n");
    assert_eq!(fs::read_to_string(&target).unwrap(), "original");
    assert!(!PathBuf::from(temp).exists());
}

#[test]
fn write_goes_through_a_linked_directory() {
    let dir = tempfile::tempdir().unwrap();
    let path = owned_path(&dir);
    fs::create_dir(dir.path().join("state")).unwrap();
    let real = dir.path().join("real");
    fs::create_dir(&real).unwrap();
    symlink(&real, path.parent().unwrap()).unwrap();
    write(&path, b"line\n").unwrap();
    assert_eq!(
        fs::read_to_string(real.join("status.json")).unwrap(),
        "line\n"
    );
}

#[test]
fn write_refuses_bytes_over_the_bound() {
    let dir = tempfile::tempdir().unwrap();
    let path = owned_path(&dir);
    let error = write(&path, "x".repeat(max() + 1).as_bytes()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "refusing the line: 65537 bytes is over 65536"
    );
    assert!(!path.exists());
    write(&path, "x".repeat(max()).as_bytes()).unwrap();
}

#[test]
fn read_gives_none_for_a_missing_file_and_the_bytes_of_a_present_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = owned_path(&dir);
    assert_eq!(read(&path).unwrap(), None);
    write(&path, "x".repeat(max()).as_bytes()).unwrap();
    assert_eq!(read(&path).unwrap(), Some("x".repeat(max()).into_bytes()));
}

#[test]
fn read_refuses_a_file_over_the_bound() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big");
    fs::write(&path, "x".repeat(max() + 1)).unwrap();
    let error = read(&path).unwrap_err();
    assert_eq!(error.to_string(), "it is over 65536 bytes");
}

#[test]
fn read_refuses_a_directory_a_link_and_a_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let directory = dir.path().join("directory");
    fs::create_dir(&directory).unwrap();
    assert_eq!(
        read(&directory).unwrap_err().to_string(),
        "it is not a regular file"
    );
    let target = dir.path().join("target");
    fs::write(&target, "secret").unwrap();
    let link = dir.path().join("link");
    symlink(&target, &link).unwrap();
    assert_eq!(
        read(&link).unwrap_err().raw_os_error(),
        Some(Errno::LOOP.raw_os_error())
    );
    let pipe = dir.path().join("pipe");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &pipe,
        rustix::fs::FileType::Fifo,
        Mode::from_raw_mode(FILE_MODE),
        0,
    )
    .unwrap();
    assert_eq!(
        read(&pipe).unwrap_err().to_string(),
        "it is not a regular file"
    );
}

#[test]
fn a_failed_rename_leaves_no_copy_behind() {
    let dir = tempfile::tempdir().unwrap();
    let path = owned_path(&dir);
    fs::create_dir_all(&path).unwrap();
    let error = write(&path, b"{\"a\":1}\n").unwrap_err();
    assert!(matches!(error, WriteError::RenameCopy(..)), "{error}");
    let mut copy = path.as_os_str().to_os_string();
    copy.push(TEMP_SUFFIX);
    assert!(!Path::new(&copy).exists(), "{}", Path::new(&copy).display());
    assert!(path.is_dir());
}

#[test]
fn write_opens_the_directory_to_sync_it_after_the_rename() {
    let dir = tempfile::tempdir().unwrap();
    let path = owned_path(&dir);
    write(&path, b"{\"a\":1}\n").unwrap();
    let parent = path.parent().unwrap();
    // Search and write but no read: the rename works and only opening the directory to sync it fails.
    fs::set_permissions(parent, Permissions::from_mode(WRITE_AND_SEARCH)).unwrap();
    let result = write(&path, b"{\"a\":2}\n");
    fs::set_permissions(parent, Permissions::from_mode(DIR_MODE)).unwrap();
    let error = result.unwrap_err();
    assert!(matches!(error, WriteError::SyncDirectory(..)), "{error}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "{\"a\":2}\n");
}
