//! The store's rules, each driven through the public functions in a temporary directory of its own.

use std::fs::{self, Permissions};
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use proptest::prelude::*;
use rustix::fs::{CWD, FileType, Mode};
use tempfile::TempDir;

use super::*;

/// A stamp of the shape `date -u +%Y%m%dT%H%M%SZ` prints.
const STAMP: &str = "20261120T120000Z";
/// A locally administered address that names no real device.
const ADDRESS: &str = "02:00:00:00:00:01";
/// The octets of `ADDRESS`.
const OCTETS: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
/// The mode a test gives a file or directory it builds to pass the checks.
const OWNER_DIR: u32 = 0o700;
/// The mode a test gives a file it builds to pass the checks.
const OWNER_FILE: u32 = 0o600;
/// A directory every Linux host has, owned by uid 0.
const SYSTEM_DIR: &str = "/usr";
/// A regular file every Linux host has, owned by uid 0 and readable by everyone.
const SYSTEM_FILE: &str = "/etc/passwd";
/// A read of a planted FIFO that has not returned by then is a hang, and the test fails.
const FIFO_DEADLINE: Duration = Duration::from_secs(5);
/// How far past `MAX_DEVICE_BYTES` generated device contents run, so the parser sees oversized input too.
const DEVICE_SAMPLE_OVERRUN: u64 = 16;

fn mode_of(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & PERMISSION_BITS
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, Permissions::from_mode(mode)).unwrap();
}

fn root_of(home: &TempDir) -> PathBuf {
    root(home.path().as_os_str())
}

/// A root that exists already, with the given mode.
fn made_root(home: &TempDir, mode: u32) -> PathBuf {
    let root = root_of(home);
    fs::create_dir(&root).unwrap();
    set_mode(&root, mode);
    root
}

fn make_fifo(path: &Path) {
    rustix::fs::mknodat(
        CWD,
        path,
        FileType::Fifo,
        Mode::from_raw_mode(OWNER_FILE),
        0,
    )
    .unwrap();
}

/// Runs `read` on its own thread so a read that blocks on a FIFO fails the test instead of hanging it.
fn within_deadline<T: Send + 'static>(read: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || sender.send(read()));
    receiver
        .recv_timeout(FIFO_DEADLINE)
        .expect("the read blocked on a FIFO")
}

fn names_path(error: &StoreError, path: &Path) -> bool {
    error.to_string().contains(&path.display().to_string())
}

#[test]
fn creates_root_session_and_run_with_mode_0700() {
    let home = tempfile::tempdir().unwrap();
    let root = root_of(&home);
    assert_eq!(root, home.path().join("omapods-captures"));

    let session = Session::open(&root, STAMP).unwrap();
    assert_eq!(session.path(), root.join(STAMP));
    let run = Run::create(&session, "left-mic-1").unwrap();
    let run_path = session.path().join("left-mic-1");
    assert_eq!(mode_of(&root), OWNER_DIR);
    assert_eq!(mode_of(session.path()), OWNER_DIR);
    assert_eq!(mode_of(&run_path), OWNER_DIR);

    let mut file = run.create_file("uplink.bin").unwrap();
    file.write_all(b"units").unwrap();
    drop(file);
    let again = Session::open(&root, STAMP).unwrap();
    assert_eq!(again.path(), session.path());
    let reopened = Run::open(&run_path).unwrap();
    assert_eq!(reopened.read("uplink.bin").unwrap(), b"units");
}

#[test]
fn files_are_0600_and_never_overwritten() {
    let home = tempfile::tempdir().unwrap();
    let session = Session::open(&root_of(&home), STAMP).unwrap();
    let run = Run::create(&session, "files").unwrap();
    let run_path = session.path().join("files");

    let mut file = run.create_file("uplink.bin").unwrap();
    file.write_all(b"first").unwrap();
    drop(file);
    assert_eq!(mode_of(&run_path.join("uplink.bin")), OWNER_FILE);
    let error = run.create_file("uplink.bin").unwrap_err();
    assert!(matches!(error, StoreError::FileExists { .. }), "{error}");
    assert!(names_path(&error, &run_path.join("uplink.bin")), "{error}");
    assert_eq!(run.read("uplink.bin").unwrap(), b"first");

    let target = home.path().join("target");
    fs::write(&target, b"keep").unwrap();
    symlink(&target, run_path.join("planted.bin")).unwrap();
    let error = run.create_file("planted.bin").unwrap_err();
    assert!(matches!(error, StoreError::FileExists { .. }), "{error}");
    assert_eq!(fs::read(&target).unwrap(), b"keep");
}

#[test]
fn refuses_a_root_with_group_or_other_bits() {
    for bad in [0o750, 0o705, 0o701, 0o1700, 0o500] {
        let home = tempfile::tempdir().unwrap();
        let root = made_root(&home, bad);
        let error = Session::open(&root, STAMP).unwrap_err();
        assert!(
            matches!(error, StoreError::WrongMode { mode, .. } if mode == bad),
            "{error}"
        );
        assert!(names_path(&error, &root), "{error}");
        assert!(!root.join(STAMP).exists(), "mode {bad:o}");
    }

    let home = tempfile::tempdir().unwrap();
    let root = made_root(&home, OWNER_DIR);
    let session = root.join(STAMP);
    fs::create_dir(&session).unwrap();
    set_mode(&session, 0o750);
    let error = Session::open(&root, STAMP).unwrap_err();
    assert!(matches!(error, StoreError::WrongMode { .. }), "{error}");
    assert!(names_path(&error, &session), "{error}");
}

#[test]
fn refuses_a_directory_or_file_owned_by_another_user() {
    let system_dir = Path::new(SYSTEM_DIR);
    let error = read_device(system_dir).unwrap_err();
    assert!(matches!(error, StoreError::WrongOwner { .. }), "{error}");
    assert!(names_path(&error, system_dir), "{error}");

    let system_file = Path::new(SYSTEM_FILE);
    let rule = ModeRule::OwnerOnly;
    let error = read_bounded(CWD, SYSTEM_FILE, system_file, rule, MAX_READ_BYTES).unwrap_err();
    assert!(matches!(error, StoreError::WrongOwner { .. }), "{error}");
    assert!(names_path(&error, system_file), "{error}");
}

#[test]
fn symlinked_root_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let real = home.path().join("real");
    fs::create_dir(&real).unwrap();
    set_mode(&real, OWNER_DIR);
    let root = root_of(&home);
    symlink(&real, &root).unwrap();
    let error = Session::open(&root, STAMP).unwrap_err();
    assert!(matches!(error, StoreError::Symlink { .. }), "{error}");
    assert!(names_path(&error, &root), "{error}");
    assert!(!real.join(STAMP).exists());

    let home = tempfile::tempdir().unwrap();
    let missing = home.path().join("missing");
    let root = root_of(&home);
    symlink(&missing, &root).unwrap();
    let error = Session::open(&root, STAMP).unwrap_err();
    assert!(matches!(error, StoreError::Symlink { .. }), "{error}");
    assert!(!missing.exists());
}

#[test]
fn symlinked_session_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let root = made_root(&home, OWNER_DIR);
    let real = home.path().join("real");
    fs::create_dir(&real).unwrap();
    set_mode(&real, OWNER_DIR);
    let session = root.join(STAMP);
    symlink(&real, &session).unwrap();
    let error = Session::open(&root, STAMP).unwrap_err();
    assert!(matches!(error, StoreError::Symlink { .. }), "{error}");
    assert!(names_path(&error, &session), "{error}");

    let error = Run::open(&session.join("run")).unwrap_err();
    assert!(matches!(error, StoreError::Symlink { .. }), "{error}");
    assert!(names_path(&error, &session), "{error}");
}

#[test]
fn refuses_a_directory_inside_a_git_work_tree() {
    let repo = tempfile::tempdir().unwrap();
    fs::create_dir(repo.path().join(".git")).unwrap();
    let nested = repo.path().join("a").join("b");
    fs::create_dir_all(&nested).unwrap();
    for home in [repo.path(), nested.as_path()] {
        let root = root(home.as_os_str());
        let error = Session::open(&root, STAMP).unwrap_err();
        assert!(matches!(error, StoreError::InRepository { .. }), "{error}");
        assert!(names_path(&error, &root), "{error}");
        assert!(!root.exists());
    }

    let linked = tempfile::tempdir().unwrap();
    let through = linked.path().join("home");
    symlink(&nested, &through).unwrap();
    let root = root(through.as_os_str());
    let error = Session::open(&root, STAMP).unwrap_err();
    assert!(matches!(error, StoreError::InRepository { .. }), "{error}");
    assert!(!nested.join("omapods-captures").exists());

    let worktree = tempfile::tempdir().unwrap();
    fs::write(worktree.path().join(".git"), "gitdir: elsewhere\n").unwrap();
    let error = Session::open(&root_of(&worktree), STAMP).unwrap_err();
    assert!(matches!(error, StoreError::InRepository { .. }), "{error}");

    let home = tempfile::tempdir().unwrap();
    let root = made_root(&home, OWNER_DIR);
    fs::create_dir(root.join(".git")).unwrap();
    let error = Session::open(&root, STAMP).unwrap_err();
    assert!(matches!(error, StoreError::InRepository { .. }), "{error}");
    assert!(!root.join(STAMP).exists());
}

/// `<home>/omapods-captures/<STAMP>/run`, each level made 0700 by hand so no store check runs.
fn hand_made_run(home: &Path) -> PathBuf {
    let run = root(home.as_os_str()).join(STAMP).join("run");
    fs::create_dir_all(&run).unwrap();
    for dir in run.ancestors().take(3) {
        set_mode(dir, OWNER_DIR);
    }
    run
}

#[test]
fn run_opened_inside_a_git_work_tree_is_refused() {
    let repo = tempfile::tempdir().unwrap();
    fs::create_dir(repo.path().join(".git")).unwrap();
    let run = hand_made_run(&repo.path().join("home"));
    let error = Run::open(&run).unwrap_err();
    assert!(matches!(error, StoreError::InRepository { .. }), "{error}");

    let home = tempfile::tempdir().unwrap();
    let run = hand_made_run(home.path());
    fs::create_dir(run.join(".git")).unwrap();
    let error = Run::open(&run).unwrap_err();
    assert!(matches!(error, StoreError::InRepository { .. }), "{error}");
    assert!(names_path(&error, &run), "{error}");
    assert_eq!(fs::read_dir(&run).unwrap().count(), 1);

    let home = tempfile::tempdir().unwrap();
    let run = hand_made_run(home.path());
    let session = run.parent().unwrap();
    fs::create_dir(session.join(".git")).unwrap();
    let error = Run::open(&run).unwrap_err();
    assert!(
        matches!(&error, StoreError::InRepository { path, .. } if path == session),
        "{error}"
    );
    let error = Session::open(&root_of(&home), STAMP).unwrap_err();
    assert!(
        matches!(&error, StoreError::InRepository { path, .. } if path == session),
        "{error}"
    );

    let relative = Path::new(ROOT_NAME).join(STAMP).join("run");
    let error = Run::open(&relative).unwrap_err();
    assert!(matches!(error, StoreError::BadRoot { .. }), "{error}");
}

#[test]
fn run_is_opened_only_inside_the_store_and_never_through_a_link() {
    let elsewhere = tempfile::tempdir().unwrap();
    let real_run = hand_made_run(elsewhere.path());
    Run::open(&real_run).unwrap();

    let home = tempfile::tempdir().unwrap();
    let session = made_root(&home, OWNER_DIR).join(STAMP);
    symlink(real_run.parent().unwrap(), &session).unwrap();
    let error = Run::open(&session.join("run")).unwrap_err();
    assert!(matches!(error, StoreError::Symlink { .. }), "{error}");
    assert!(names_path(&error, &session), "{error}");

    let outside = elsewhere.path().join("run");
    fs::create_dir(&outside).unwrap();
    set_mode(&outside, OWNER_DIR);
    let error = Run::open(&outside).unwrap_err();
    assert!(matches!(error, StoreError::NotARun { .. }), "{error}");
    assert!(names_path(&error, &outside), "{error}");
}

#[test]
fn refuses_a_stamp_that_is_a_path() {
    let home = tempfile::tempdir().unwrap();
    let root = root_of(&home);
    for stamp in [
        "../x",
        "2026",
        "",
        "2026/120T120000Z",
        "20261120T12000/Z",
        "20261120T120000Z/",
        "20261120T120000ZZ",
        "20261120t120000Z",
        "2026112OT120000Z",
        "20261120T120000z",
        "20261120\u{663}120000Z",
    ] {
        let error = Session::open(&root, stamp).unwrap_err();
        assert!(
            matches!(error, StoreError::BadStamp { .. }),
            "{stamp:?}: {error}"
        );
    }
    assert!(!root.exists());
}

#[test]
fn refuses_a_run_name_with_a_dot_or_slash() {
    let home = tempfile::tempdir().unwrap();
    let session = Session::open(&root_of(&home), STAMP).unwrap();
    let too_long = "a".repeat(RUN_NAME_MAX + 1);
    for name in [
        ".",
        "..",
        "a.b",
        "a/b",
        "/a",
        "",
        "A",
        "a_b",
        "a b",
        too_long.as_str(),
    ] {
        let error = Run::create(&session, name).unwrap_err();
        assert!(
            matches!(error, StoreError::BadRunName { .. }),
            "{name:?}: {error}"
        );
    }
    assert_eq!(fs::read_dir(session.path()).unwrap().count(), 0);

    let longest = "z".repeat(RUN_NAME_MAX);
    for name in [longest.as_str(), "0-9", "-"] {
        Run::create(&session, name).unwrap();
    }
}

#[test]
fn existing_run_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let session = Session::open(&root_of(&home), STAMP).unwrap();
    Run::create(&session, "r1").unwrap();
    let error = Run::create(&session, "r1").unwrap_err();
    assert!(matches!(error, StoreError::RunExists { .. }), "{error}");
    assert!(names_path(&error, &session.path().join("r1")), "{error}");

    let elsewhere = home.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    symlink(&elsewhere, session.path().join("r2")).unwrap();
    let error = Run::create(&session, "r2").unwrap_err();
    assert!(matches!(error, StoreError::RunExists { .. }), "{error}");

    fs::write(session.path().join("r3"), b"").unwrap();
    let error = Run::create(&session, "r3").unwrap_err();
    assert!(matches!(error, StoreError::RunExists { .. }), "{error}");
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
}

/// A root holding a device file with `contents` and `mode`.
fn root_with_device(home: &TempDir, contents: &[u8], mode: u32) -> PathBuf {
    let root = made_root(home, OWNER_DIR);
    let device = root.join("device");
    fs::write(&device, contents).unwrap();
    set_mode(&device, mode);
    root
}

fn device_address(contents: &[u8], mode: u32) -> BdAddr {
    let home = tempfile::tempdir().unwrap();
    read_device(&root_with_device(&home, contents, mode)).unwrap()
}

fn device_refusal(contents: &[u8], mode: u32) -> StoreError {
    let home = tempfile::tempdir().unwrap();
    read_device(&root_with_device(&home, contents, mode)).unwrap_err()
}

#[test]
fn device_file_must_be_0600_regular_and_short() {
    let with_newline = format!("{ADDRESS}\n");
    for contents in [ADDRESS.as_bytes(), with_newline.as_bytes()] {
        let addr = device_address(contents, OWNER_FILE);
        assert_eq!(addr.octets(), OCTETS);
    }
    for bad in [0o644, 0o640, 0o400, 0o700] {
        let error = device_refusal(with_newline.as_bytes(), bad);
        assert!(
            matches!(error, StoreError::WrongMode { .. }),
            "{bad:o}: {error}"
        );
    }
    let device_max = usize::try_from(MAX_DEVICE_BYTES).unwrap();
    let error = device_refusal(&vec![b'A'; device_max + 1], OWNER_FILE);
    assert!(matches!(error, StoreError::TooLarge { .. }), "{error}");
    let error = device_refusal(&vec![b'A'; device_max], OWNER_FILE);
    assert!(
        matches!(error, StoreError::DeviceNotAddress { .. }),
        "{error}"
    );
    let error = device_refusal(format!("{ADDRESS}\n\n").as_bytes(), OWNER_FILE);
    assert!(
        matches!(error, StoreError::DeviceNotAddress { .. }),
        "{error}"
    );

    let home = tempfile::tempdir().unwrap();
    let real = tempfile::tempdir().unwrap();
    let target = real.path().join("device");
    fs::write(&target, ADDRESS).unwrap();
    set_mode(&target, OWNER_FILE);
    let root = made_root(&home, OWNER_DIR);
    symlink(&target, root.join("device")).unwrap();
    let error = read_device(&root).unwrap_err();
    assert!(matches!(error, StoreError::Symlink { .. }), "{error}");
    assert!(names_path(&error, &root.join("device")), "{error}");

    let home = tempfile::tempdir().unwrap();
    let root = made_root(&home, OWNER_DIR);
    fs::create_dir(root.join("device")).unwrap();
    let error = read_device(&root).unwrap_err();
    assert!(matches!(error, StoreError::WrongType { .. }), "{error}");

    let home = tempfile::tempdir().unwrap();
    let root = made_root(&home, OWNER_DIR);
    make_fifo(&root.join("device"));
    let error = within_deadline(move || read_device(&root)).unwrap_err();
    assert!(matches!(error, StoreError::WrongType { .. }), "{error}");

    let home = tempfile::tempdir().unwrap();
    let root = made_root(&home, 0o755);
    fs::write(root.join("device"), ADDRESS).unwrap();
    set_mode(&root.join("device"), OWNER_FILE);
    let error = read_device(&root).unwrap_err();
    assert!(matches!(error, StoreError::WrongMode { .. }), "{error}");
}

#[test]
fn device_file_error_does_not_echo_its_contents() {
    let samples: [&[u8]; 5] = [
        b"12:34:56:78:9A:BG",
        b"12:34:56:78:9A:BC:DE",
        b"12-34-56-78-9A-BC",
        b"\xff12:34:56:78:9A:BC",
        b"12:34:56:78:9A:BC\n\n",
    ];
    for contents in samples {
        let error = device_refusal(contents, OWNER_FILE);
        let shown = format!("{error} {error:?}");
        for piece in ["12:", "34:", "56:", "78:", "9A:", ":BG", ":BC", "12-"] {
            assert!(!shown.contains(piece), "{piece} in {shown}");
        }
    }
}

#[test]
fn file_name_with_a_slash_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let session = Session::open(&root_of(&home), STAMP).unwrap();
    let run = Run::create(&session, "names").unwrap();
    for name in ["a/b", "../escape", "/abs", "", ".", "..", "a\0b"] {
        let error = run.create_file(name).unwrap_err();
        assert!(
            matches!(error, StoreError::BadFileName { .. }),
            "{name:?}: {error}"
        );
        let error = run.read(name).unwrap_err();
        assert!(
            matches!(error, StoreError::BadFileName { .. }),
            "{name:?}: {error}"
        );
    }
    assert!(!session.path().join("escape").exists());
    assert_eq!(
        fs::read_dir(session.path().join("names")).unwrap().count(),
        0
    );
}

#[test]
fn reads_are_regular_owner_only_and_bounded() {
    let home = tempfile::tempdir().unwrap();
    let session = Session::open(&root_of(&home), STAMP).unwrap();
    let run = Run::create(&session, "reads").unwrap();
    let run_path = session.path().join("reads");

    fs::write(run_path.join("shared.bin"), b"x").unwrap();
    set_mode(&run_path.join("shared.bin"), 0o640);
    let error = run.read("shared.bin").unwrap_err();
    assert!(matches!(error, StoreError::NotOwnerOnly { .. }), "{error}");
    set_mode(&run_path.join("shared.bin"), 0o400);
    assert_eq!(run.read("shared.bin").unwrap(), b"x");

    symlink(run_path.join("shared.bin"), run_path.join("link.bin")).unwrap();
    let error = run.read("link.bin").unwrap_err();
    assert!(matches!(error, StoreError::Symlink { .. }), "{error}");
    fs::create_dir(run_path.join("dir.bin")).unwrap();
    let error = run.read("dir.bin").unwrap_err();
    assert!(matches!(error, StoreError::WrongType { .. }), "{error}");
    make_fifo(&run_path.join("fifo.bin"));
    let fifo_run = Run::open(&run_path).unwrap();
    let error = within_deadline(move || fifo_run.read("fifo.bin")).unwrap_err();
    assert!(matches!(error, StoreError::WrongType { .. }), "{error}");

    let largest = run.create_file("largest.bin").unwrap();
    largest.set_len(MAX_READ_BYTES).unwrap();
    drop(largest);
    let read = run.read("largest.bin").unwrap();
    assert_eq!(u64::try_from(read.len()).unwrap(), MAX_READ_BYTES);
    let over = run.create_file("over.bin").unwrap();
    over.set_len(MAX_READ_BYTES + 1).unwrap();
    drop(over);
    let error = run.read("over.bin").unwrap_err();
    assert!(matches!(error, StoreError::TooLarge { .. }), "{error}");
}

#[test]
fn a_trace_is_read_up_to_the_bound_it_is_given() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("hci.btsnoop");
    fs::write(&path, b"0123456789").unwrap();
    set_mode(&path, OWNER_FILE);
    assert_eq!(read_trace(&path, 10).unwrap(), b"0123456789");
    let error = read_trace(&path, 9).unwrap_err();
    assert!(
        matches!(error, StoreError::TooLarge { limit: 9, .. }),
        "{error}"
    );
    assert!(names_path(&error, &path), "{error}");

    set_mode(&path, 0o640);
    let error = read_trace(&path, 10).unwrap_err();
    assert!(matches!(error, StoreError::NotOwnerOnly { .. }), "{error}");
    let error = read_trace(Path::new(SYSTEM_FILE), 10).unwrap_err();
    assert!(matches!(error, StoreError::WrongOwner { .. }), "{error}");
}

/// Calls that remove, rename or overwrite; the code before the tests holds none of them.
const FORBIDDEN: [&str; 12] = [
    "remove_",
    "unlink",
    "rename",
    "rmdir",
    "TRUNC",
    "truncate(",
    "set_len",
    "OpenOptions",
    "File::create",
    "File::options",
    "fs::write",
    "fs::copy",
];

/// An inline tests module, which clippy's `items_after_test_module` keeps last, so the scan stops there.
const TESTS_MODULE: &str = "#[cfg(test)]\nmod tests {";
/// A file of unit tests, which names the forbidden calls itself.
const TESTS_FILE: &str = "tests.rs";

/// Every source file of the crate except test files, read at run time so a module added later is scanned too.
fn crate_sources() -> Vec<(PathBuf, String)> {
    let mut pending = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    let mut sources = Vec::new();
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension() == Some(OsStr::new("rs")) && !path.ends_with(TESTS_FILE) {
                let source = fs::read_to_string(&path).unwrap();
                sources.push((path, source));
            }
        }
    }
    sources
}

#[test]
fn no_code_path_removes_or_overwrites() {
    let sources = crate_sources();
    for known in ["main.rs", "mod.rs", "store.rs"] {
        let found = sources.iter().any(|(path, _)| path.ends_with(known));
        assert!(found, "{known} was not scanned");
    }
    for (path, source) in &sources {
        let code = source
            .split_once(TESTS_MODULE)
            .map_or(source.as_str(), |(code, _)| code);
        for call in FORBIDDEN {
            assert!(!code.contains(call), "{} holds {call}", path.display());
        }
    }
}

/// Device file contents from empty to `DEVICE_SAMPLE_OVERRUN` bytes past the device limit.
fn device_contents() -> impl Strategy<Value = Vec<u8>> {
    let longest = usize::try_from(MAX_DEVICE_BYTES + DEVICE_SAMPLE_OVERRUN).unwrap();
    proptest::collection::vec(any::<u8>(), 0..=longest)
}

proptest! {
    #[test]
    fn stamp_check_never_panics(text in any::<String>()) {
        if check_stamp(&text).is_ok() {
            prop_assert_eq!(text.len(), STAMP.len());
            prop_assert!(text.bytes().all(|byte| byte.is_ascii_digit() || byte == b'T' || byte == b'Z'));
        }
    }

    #[test]
    fn run_name_check_never_panics(text in any::<String>()) {
        if check_run_name(&text).is_ok() {
            prop_assert!(!text.is_empty() && text.len() <= RUN_NAME_MAX);
            prop_assert!(!text.contains(['.', '/']));
        }
    }

    #[test]
    fn file_name_check_never_panics(text in any::<String>()) {
        if check_file_name(&text).is_ok() {
            prop_assert!(!text.is_empty() && text != "." && text != "..");
            prop_assert!(!text.contains(['/', '\0']));
        }
    }

    #[test]
    fn device_parse_never_panics(contents in device_contents()) {
        if let Err(error) = parse_device(Path::new("/store/device"), &contents) {
            prop_assert!(!error.to_string().is_empty());
        }
    }

    #[test]
    fn device_parse_never_echoes_an_address(text in "([0-9A-F]{2}:){5}[0-9A-Z]{2}[ \n]{0,2}") {
        if let Err(error) = parse_device(Path::new("/store/device"), text.as_bytes()) {
            let shown = format!("{error} {error:?}");
            for pair in text.split(':').take(5) {
                prop_assert!(!shown.contains(&format!("{pair}:")), "{} in {}", pair, shown);
            }
        }
    }
}
