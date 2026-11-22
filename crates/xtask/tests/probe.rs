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

/// Every command, each with arguments that would otherwise get as far as the store.
fn every_command() -> [Vec<&'static str>; 7] {
    [
        vec!["init", "--stamp", STAMP],
        vec![
            "capture", "--stamp", STAMP, "--run", "page", "--script", "knock",
        ],
        vec!["stop"],
        vec!["hci", "trace.snoop", "--stamp", STAMP, "--run", "page"],
        vec!["analyse", "page"],
        vec!["decode", "page"],
        vec!["audio", "page"],
    ]
}

#[test]
fn every_command_refuses_an_unset_home() {
    for args in every_command() {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
        command.arg("probe").args(&args).env_remove("HOME");
        let refused = command.output().unwrap();
        assert!(!refused.status.success(), "{args:?}: {refused:?}");
        assert_eq!(refused.stdout, b"", "{args:?}");
        assert_eq!(
            error_of(&refused),
            "xtask probe: HOME is not set, and the store lives under it\n",
            "{args:?}"
        );
    }
}

#[test]
fn every_command_refuses_a_home_it_could_not_print_faithfully() {
    let parent = tempfile::tempdir().unwrap();
    let home = parent.path().join(OsStr::from_bytes(NOT_UTF8_NAME));
    fs::create_dir(&home).unwrap();

    let mut sentences = Vec::new();
    for args in every_command() {
        let refused = probe(&home, &args).output().unwrap();
        assert!(!refused.status.success(), "{args:?}: {refused:?}");
        assert_eq!(refused.stdout, b"", "{args:?}");
        let error = String::from_utf8(refused.stderr).unwrap();
        assert!(error.contains("is not valid UTF-8"), "{args:?}: {error}");
        assert!(
            error.contains(&format!("HOME {home:?}")),
            "{args:?}: {error}"
        );
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0, "{args:?}");
        sentences.push(error);
    }
    sentences.dedup();
    assert_eq!(sentences.len(), 1, "{sentences:?}");
}

#[test]
fn capture_refuses_a_script_it_cannot_build_before_it_creates_anything() {
    let home = tempfile::tempdir().unwrap();
    let run = ["capture", "--stamp", STAMP, "--run", "page", "--script"];
    for (script, sentence) in [
        (
            &["chat"][..],
            "the scripts are knock, listen, soak, voice, side, trials, early, firstword, att-knock, att-map and both",
        ),
        (
            &["att-knock"],
            "script att-knock is not a script of the uplink route",
        ),
        (
            &["knock", "--route", "dictation"],
            "script knock is not a script of the dictation route, whose scripts are soak, voice, firstword, att-knock and att-map",
        ),
        (
            &["knock", "--route", "both ways"],
            "that is not a route; the routes are uplink, dictation and both",
        ),
        (
            &["att-knock", "--route", "dictation", "--minutes", "1"],
            "script att-knock has a fixed length",
        ),
        (&["both"], "script both is not a script of the uplink route"),
        (
            &["voice", "--route", "both"],
            "script voice is not a script of the both route, whose scripts are both",
        ),
        (
            &["soak", "--route", "dictation"],
            "script soak runs for a set time",
        ),
        (
            &["att-map", "--route", "dictation", "--minutes", "1"],
            "script att-map has a fixed length",
        ),
        (
            &["knock", "--no-marker"],
            "--no-marker is refused on the uplink route",
        ),
        (
            &["knock", "--unchecked"],
            "--unchecked is refused on the uplink route",
        ),
        (
            &["voice", "--unchecked", "--no-marker"],
            "is refused on the uplink route",
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

    for stop in [&["stop"][..], &["stop", "--route", "dictation"]] {
        let refused = probe(home.path(), stop).output().unwrap();
        assert!(!refused.status.success(), "{stop:?}: {refused:?}");
        assert!(
            error_of(&refused).contains(&device),
            "{stop:?}: {refused:?}"
        );
        assert_eq!(refused.stdout, b"", "{stop:?}");
    }
}

#[test]
fn stop_refuses_both_routes_at_once_before_it_reads_anything() {
    let home = tempfile::tempdir().unwrap();
    let refused = probe(home.path(), &["stop", "--route", "both"])
        .output()
        .unwrap();
    assert!(!refused.status.success(), "{refused:?}");
    assert_eq!(
        error_of(&refused),
        "xtask probe: stop takes one route at a time: run it with --route uplink, then with --route dictation\n"
    );
    assert_eq!(refused.stdout, b"");
    assert!(!home.path().join(ROOT_NAME).exists());
}
