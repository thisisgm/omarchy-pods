//! The built binary's `probe` commands, run with `HOME` set to a temporary directory.

use std::ffi::OsStr;
use std::fs::{self, Permissions};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

/// A stamp of the shape `date -u +%Y%m%dT%H%M%SZ` prints.
const STAMP: &str = "20261120T120000Z";
/// A root mode with group and other bits, which init must refuse.
const WIDE_MODE: u32 = 0o755;
/// A directory name that is not UTF-8: one byte no UTF-8 text holds.
const NOT_UTF8_NAME: &[u8] = &[0xff];
/// The store under the temporary home.
const ROOT_NAME: &str = "omapods-captures";

/// One probe command of the built binary, with `HOME` set to `home`.
fn probe(home: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command.arg("probe").args(args).env("HOME", home);
    command
}

/// What a command wrote on its standard error.
fn error_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

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

#[test]
fn capture_refuses_a_script_it_cannot_build_before_it_creates_anything() {
    let home = tempfile::tempdir().unwrap();
    let run = ["capture", "--stamp", STAMP, "--run", "page", "--script"];
    for (script, sentence) in [
        (
            &["chat"][..],
            "the scripts are knock, listen, soak, voice, side, trials, early and firstword",
        ),
        (&["listen"], "script listen runs for a set time"),
        (&["soak"], "pass --minutes 1 to 90"),
        (
            &["voice", "--minutes", "2"],
            "script voice has a fixed length",
        ),
        (
            &["soak", "--minutes", "0"],
            "--minutes 0 is outside 1 to 90",
        ),
        (
            &["listen", "--minutes", "91"],
            "--minutes 91 is outside 1 to 90",
        ),
    ] {
        let refused = probe(home.path(), &[&run[..], script].concat())
            .output()
            .unwrap();
        let error = error_of(&refused);
        assert!(!refused.status.success(), "{script:?}: {refused:?}");
        assert!(error.contains(sentence), "{script:?}: {error}");
        assert_eq!(refused.stdout, b"", "{script:?}");
        assert!(!home.path().join(ROOT_NAME).exists(), "{script:?}");
    }
}

#[test]
fn capture_and_stop_fail_at_the_device_file_before_any_connect() {
    let home = tempfile::tempdir().unwrap();
    let created = probe(home.path(), &["init", "--stamp", STAMP])
        .output()
        .unwrap();
    assert!(created.status.success(), "{created:?}");
    let root = home.path().join(ROOT_NAME);
    let device = root.join("device").display().to_string();

    let capture = ["capture", "--stamp", STAMP, "--run", "page"];
    let script = ["--script", "listen", "--minutes", "1"];
    let refused = probe(home.path(), &[capture.as_slice(), &script].concat())
        .output()
        .unwrap();
    assert!(!refused.status.success(), "{refused:?}");
    assert!(error_of(&refused).contains(&device), "{refused:?}");
    assert_eq!(refused.stdout, b"");
    // The run name is still free, because the device is read before the run is created.
    assert!(!root.join(STAMP).join("page").exists());

    let refused = probe(home.path(), &["stop"]).output().unwrap();
    assert!(!refused.status.success(), "{refused:?}");
    assert!(error_of(&refused).contains(&device), "{refused:?}");
    assert_eq!(refused.stdout, b"");
}
