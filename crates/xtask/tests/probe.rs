//! The built binary's `probe` commands, run with `HOME` set to a temporary directory.

use std::ffi::OsStr;
use std::fs::{self, Permissions};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

/// A stamp of the shape `date -u +%Y%m%dT%H%M%SZ` prints.
const STAMP: &str = "20261120T120000Z";
/// A root mode with group and other bits, which init must refuse.
const WIDE_MODE: u32 = 0o755;
/// A directory name that is not UTF-8: one byte no UTF-8 text holds.
const NOT_UTF8_NAME: &[u8] = &[0xff];

#[test]
fn init_prints_the_session_path_and_nothing_else() {
    let home = tempfile::tempdir().unwrap();
    let init = || {
        Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["probe", "init", "--stamp", STAMP])
            .env("HOME", home.path())
            .output()
            .unwrap()
    };
    let root = home.path().join("omapods-captures");
    let session = root.join(STAMP);

    let created = init();
    assert!(created.status.success(), "{created:?}");
    assert_eq!(
        String::from_utf8(created.stdout).unwrap(),
        format!("{}\n", session.display())
    );
    assert_eq!(String::from_utf8(created.stderr).unwrap(), "");
    assert!(session.is_dir());

    fs::set_permissions(&root, Permissions::from_mode(WIDE_MODE)).unwrap();
    let refused = init();
    assert!(!refused.status.success(), "{refused:?}");
    assert_eq!(String::from_utf8(refused.stdout).unwrap(), "");
    let error = String::from_utf8(refused.stderr).unwrap();
    assert!(error.contains(&root.display().to_string()), "{error}");
}

#[test]
fn init_refuses_a_home_it_could_not_print_faithfully() {
    let parent = tempfile::tempdir().unwrap();
    let home = parent.path().join(OsStr::from_bytes(NOT_UTF8_NAME));
    fs::create_dir(&home).unwrap();

    let refused = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["probe", "init", "--stamp", STAMP])
        .env("HOME", &home)
        .output()
        .unwrap();

    assert!(!refused.status.success(), "{refused:?}");
    assert_eq!(refused.stdout, b"");
    let error = String::from_utf8(refused.stderr).unwrap();
    assert!(error.contains("is not valid UTF-8"), "{error}");
    assert!(error.contains(&format!("HOME {home:?}")), "{error}");
    assert_eq!(fs::read_dir(&home).unwrap().count(), 0);
}
