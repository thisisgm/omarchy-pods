//! What the cmake install of version 1.3.6 left under the home and in the plugin, run through the real installer.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use crate::bench::{Bench, Node, Ran, repo};
use crate::install::done_lines;

const DESKTOP: &str = "home/.local/share/applications/me.kavishdevar.librepods.desktop";
const ICON: &str = "home/.local/share/icons/hicolor/scalable/apps/librepods.svg";
const CATALOG: &str = "home/.local/share/openpods/translations/openpods_tr.qm";
const TRANSLATIONS: &str = "home/.local/share/openpods/translations";
const OPENPODS: &str = "home/.local/share/openpods";
const OLD_CLIENT: &str = "home/.local/bin/librepods-ctl";
const OLD_CONFIG: &str = "home/.config/AirPodsTrayApp/AirPodsTrayApp.conf";
const SETTINGS: &str = "home/.local/state/librepods/settings.json";
const BUILD: &str = "plugin/daemon/build";
const MANIFEST: &str = "plugin/daemon/build/install_manifest.txt";
const CACHE: &str = "plugin/daemon/build/CMakeCache.txt";
/// The release whose cmake install these files come from.
const OLD_TAG: &str = "v1.3.6";
const SHIPPED_DESKTOP: &str = "daemon/assets/me.kavishdevar.librepods.desktop";
const SHIPPED_ICON: &str = "daemon/assets/librepods.svg";
/// The manifest's time, and the catalog's a minute before or after it, well past the time granularity of any file system.
const MANIFEST_TIME: Duration = Duration::from_secs(1_000_000_000);
const BEFORE_MANIFEST: Duration = Duration::from_secs(999_999_940);
const AFTER_MANIFEST: Duration = Duration::from_secs(1_000_000_060);
const OLD_CONFIG_TEXT: &str = "[General]\nnoiseControlMode=1\n";
const SETTINGS_TEXT: &str = "{\"noise_control\":\"anc\"}\n";

/// The bytes the old tag ships at `path`, read from the repository's own history.
fn shipped(path: &str) -> Vec<u8> {
    let shown = Command::new("git")
        .arg("-C")
        .arg(repo())
        .arg("show")
        .arg(format!("{OLD_TAG}:{path}"))
        .output()
        .unwrap();
    assert!(shown.status.success(), "git show failed: {shown:?}");
    shown.stdout
}

fn plant(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

/// Sets a file's modification time to `since_epoch` after the Unix epoch.
fn set_time(path: &Path, since_epoch: Duration) {
    let file = File::options().write(true).open(path).unwrap();
    file.set_modified(UNIX_EPOCH.checked_add(since_epoch).unwrap())
        .unwrap();
}

/// The manifest cmake wrote, its last line without a newline as cmake leaves it.
fn manifest_text(bench: &Bench, listed: &[&str]) -> String {
    let lines: Vec<String> = listed
        .iter()
        .map(|name| bench.path(name).display().to_string())
        .collect();
    lines.join("\n")
}

/// What a 1.3.6 install left: the client, the entry and the icon as the tag ships them, the catalog, and the build tree whose manifest lists it.
fn plant_old_install(bench: &Bench) {
    plant(&bench.path(OLD_CLIENT), b"the old client\n");
    plant(&bench.path(DESKTOP), &shipped(SHIPPED_DESKTOP));
    plant(&bench.path(ICON), &shipped(SHIPPED_ICON));
    plant(&bench.path(CATALOG), b"a compiled catalog\n");
    set_time(&bench.path(CATALOG), BEFORE_MANIFEST);
    let listed = manifest_text(bench, &[OLD_CLIENT, DESKTOP, ICON, CATALOG]);
    plant(&bench.path(MANIFEST), listed.as_bytes());
    set_time(&bench.path(MANIFEST), MANIFEST_TIME);
    plant(&bench.path(CACHE), b"CMAKE_BUILD_TYPE:STRING=Release\n");
}

/// The old config file and the settings the daemon writes on its first start after it read that file.
fn plant_old_config(bench: &Bench) {
    plant(&bench.path(OLD_CONFIG), OLD_CONFIG_TEXT.as_bytes());
    plant(&bench.path(SETTINGS), SETTINGS_TEXT.as_bytes());
}

fn removing(bench: &Bench, name: &str) -> String {
    format!("Removing {}\n", bench.path(name).display())
}

fn left(bench: &Bench, name: &str) -> String {
    format!(
        "setup: left {}: it is not the file version 1.3 installed.\n",
        bench.path(name).display()
    )
}

fn ok(ran: &Ran) {
    assert_eq!(ran.code, Some(0), "the install failed: {}", ran.stderr);
}

fn exists(bench: &Bench, name: &str) -> bool {
    fs::symlink_metadata(bench.path(name)).is_ok()
}

fn removed_paths(bench: &Bench) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for program in ["rm", "rmdir"] {
        for call in bench.arguments_of(program) {
            paths.extend(
                call.iter()
                    .skip(1)
                    .filter(|word| !word.starts_with('-'))
                    .map(PathBuf::from),
            );
        }
    }
    paths
}

/// The paths of the removals, leaving out the installer's own scratch directory.
fn removed_leftovers(bench: &Bench) -> BTreeSet<PathBuf> {
    let scratch = bench.path("runtime");
    removed_paths(bench)
        .into_iter()
        .filter(|path| !path.starts_with(&scratch))
        .collect()
}

/// One entry of the call log of a run whose output went into it.
#[derive(Debug, PartialEq)]
enum Event {
    Printed(String),
    Call(Vec<String>),
}

fn events(bench: &Bench) -> Vec<Event> {
    let text = fs::read_to_string(bench.path("calls.log")).unwrap();
    // Sample input: `Removing /tmp/setup bench Ab12/home/x\n` then `rm\0-f\0--\0/tmp/setup bench Ab12/home/x\0\n`
    text.lines()
        .map(|line| match line.strip_suffix('\0') {
            Some(ended) => Event::Call(ended.split('\0').map(str::to_owned).collect()),
            None => Event::Printed(line.to_owned()),
        })
        .collect()
}

/// Where the removal of `name` by `program` sits in the log, after its `Removing` line and never before it.
fn removed_after_naming(log: &[Event], bench: &Bench, program: &str, name: &str) -> usize {
    let path = bench.path(name).display().to_string();
    let named = Event::Printed(format!("Removing {path}"));
    let said = log.iter().position(|event| *event == named);
    let done = log.iter().position(|event| {
        matches!(event, Event::Call(call) if call.first().is_some_and(|first| first == program)
            && call.last() == Some(&path))
    });
    let (said, done) = (said.unwrap(), done.unwrap());
    assert!(said < done, "{path} was removed before it was named");
    done
}

#[test]
fn the_desktop_entry_the_icon_and_the_catalog_are_removed_and_named_first() {
    let bench = Bench::new();
    plant_old_install(&bench);
    bench.record_removals();
    ok(&bench.run_into_log(&[]));
    let log = events(&bench);
    for name in [DESKTOP, ICON, CATALOG] {
        removed_after_naming(&log, &bench, "rm", name);
        assert!(!exists(&bench, name), "{name}");
    }
}

#[test]
fn an_install_without_them_removes_nothing_and_says_nothing() {
    let bench = Bench::new();
    plant(&bench.path("plugin/daemon/CMakeLists.txt"), b"project(x)\n");
    fs::create_dir_all(bench.path(TRANSLATIONS)).unwrap();
    bench.record_removals();
    let ran = bench.run(&[]);
    ok(&ran);
    assert_eq!(ran.stdout, done_lines());
    assert_eq!(ran.stderr, "");
    assert_eq!(removed_leftovers(&bench), BTreeSet::new());
    assert!(exists(&bench, TRANSLATIONS));
}

#[test]
fn a_file_the_user_put_there_is_left_and_named() {
    let bench = Bench::new();
    plant_old_install(&bench);
    plant(&bench.path(DESKTOP), b"[Desktop Entry]\nName=Mine\n");
    plant(&bench.path(ICON), b"<svg/>\n");
    let ran = bench.run(&[]);
    ok(&ran);
    assert_eq!(ran.stderr, left(&bench, DESKTOP) + &left(&bench, ICON));
    assert_eq!(
        fs::read(bench.path(DESKTOP)).unwrap(),
        b"[Desktop Entry]\nName=Mine\n"
    );
    assert_eq!(fs::read(bench.path(ICON)).unwrap(), b"<svg/>\n");
    assert!(!exists(&bench, CATALOG));
}

/// Runs over the old install as `arrange` changed it, and holds the catalog to having stayed and been named.
fn catalog_stays(arrange: fn(&Bench)) {
    let bench = Bench::new();
    plant_old_install(&bench);
    arrange(&bench);
    let ran = bench.run(&[]);
    ok(&ran);
    assert_eq!(ran.stderr, left(&bench, CATALOG));
    assert_eq!(
        fs::read(bench.path(CATALOG)).unwrap(),
        b"a compiled catalog\n"
    );
    assert!(
        !ran.stdout.contains(&removing(&bench, CATALOG)),
        "{}",
        ran.stdout
    );
    assert!(!exists(&bench, DESKTOP));
}

#[test]
fn the_catalog_stays_without_the_manifest() {
    catalog_stays(|bench| fs::remove_file(bench.path(MANIFEST)).unwrap());
}

#[test]
fn the_catalog_stays_when_the_manifest_does_not_list_it() {
    catalog_stays(|bench| {
        let listed = manifest_text(bench, &[DESKTOP, ICON]) + "\n";
        let near = format!("{}.old\n", bench.path(CATALOG).display());
        plant(&bench.path(MANIFEST), (listed + &near).as_bytes());
    });
}

#[test]
fn a_catalog_newer_than_the_manifest_stays() {
    catalog_stays(|bench| set_time(&bench.path(CATALOG), AFTER_MANIFEST));
}

#[test]
fn the_two_empty_directories_go_after_the_catalog() {
    let bench = Bench::new();
    plant_old_install(&bench);
    bench.record_removals();
    ok(&bench.run_into_log(&[]));
    let log = events(&bench);
    let catalog = removed_after_naming(&log, &bench, "rm", CATALOG);
    let translations = removed_after_naming(&log, &bench, "rmdir", TRANSLATIONS);
    let openpods = removed_after_naming(&log, &bench, "rmdir", OPENPODS);
    assert!(catalog < translations && translations < openpods, "{log:?}");
    assert!(!exists(&bench, OPENPODS));
    assert!(exists(&bench, "home/.local/share/applications"));
}

#[test]
fn a_directory_that_holds_another_file_is_kept() {
    for (dir, other) in [(TRANSLATIONS, "openpods_de.qm"), (OPENPODS, ".notes")] {
        let bench = Bench::new();
        plant_old_install(&bench);
        let kept = format!("{dir}/{other}");
        plant(&bench.path(&kept), b"mine\n");
        let ran = bench.run(&[]);
        ok(&ran);
        assert!(
            !ran.stdout.contains(&removing(&bench, dir)),
            "{}",
            ran.stdout
        );
        assert!(
            !ran.stdout.contains(&removing(&bench, OPENPODS)),
            "{}",
            ran.stdout
        );
        assert!(!exists(&bench, CATALOG), "{dir}");
        assert_eq!(fs::read(bench.path(&kept)).unwrap(), b"mine\n", "{dir}");
        assert!(exists(&bench, OPENPODS), "{dir}");
    }
}

#[test]
fn the_directories_stay_when_the_catalog_was_left() {
    let bench = Bench::new();
    plant_old_install(&bench);
    set_time(&bench.path(CATALOG), AFTER_MANIFEST);
    bench.record_removals();
    let ran = bench.run(&[]);
    ok(&ran);
    assert_eq!(bench.calls_of("rmdir"), Vec::<String>::new());
    assert!(
        !ran.stdout.contains(&removing(&bench, TRANSLATIONS)),
        "{}",
        ran.stdout
    );
    assert!(exists(&bench, CATALOG));
}

#[test]
fn the_old_build_tree_goes_only_with_its_marker() {
    let bench = Bench::new();
    plant_old_install(&bench);
    let ran = bench.run(&[]);
    ok(&ran);
    assert!(
        ran.stdout.contains(&removing(&bench, BUILD)),
        "{}",
        ran.stdout
    );
    assert!(!exists(&bench, BUILD));
    let unmarked = Bench::new();
    plant_old_install(&unmarked);
    fs::remove_file(unmarked.path(CACHE)).unwrap();
    let ran = unmarked.run(&[]);
    ok(&ran);
    assert!(!ran.stdout.contains("daemon/build"), "{}", ran.stdout);
    assert!(exists(&unmarked, MANIFEST));
}

#[test]
fn the_files_beside_the_build_tree_are_never_touched() {
    let bench = Bench::new();
    plant_old_install(&bench);
    let beside = [
        "plugin/daemon/CMakeLists.txt",
        "plugin/daemon/main.cpp",
        "plugin/daemon/assets/librepods.svg",
        "plugin/daemon/build.log",
    ];
    for name in beside {
        plant(&bench.path(name), name.as_bytes());
    }
    bench.record_removals();
    ok(&bench.run(&[]));
    assert!(!exists(&bench, BUILD));
    for name in beside {
        assert_eq!(
            fs::read(bench.path(name)).unwrap(),
            name.as_bytes(),
            "{name}"
        );
    }
    assert!(!removed_leftovers(&bench).contains(&bench.path("plugin/daemon")));
}

#[test]
fn the_old_config_file_is_never_touched() {
    let bench = Bench::new();
    plant_old_config(&bench);
    bench.record_removals();
    let ran = bench.run(&[]);
    ok(&ran);
    assert_eq!(ran.stdout, done_lines());
    assert_eq!(ran.stderr, "");
    assert_eq!(
        fs::read_to_string(bench.path(OLD_CONFIG)).unwrap(),
        OLD_CONFIG_TEXT
    );
    assert_eq!(removed_leftovers(&bench), BTreeSet::new());
}

#[test]
fn the_old_client_file_is_now_a_link() {
    let bench = Bench::new();
    plant_old_install(&bench);
    ok(&bench.run(&[]));
    let tree = bench.home_tree();
    assert_eq!(
        tree.get(Path::new(".local/bin/librepods-ctl")),
        Some(&Node::Link(PathBuf::from("librepods")))
    );
}

#[test]
fn every_removed_path_is_one_of_the_named_paths() {
    let bench = Bench::new();
    plant_old_install(&bench);
    plant_old_config(&bench);
    bench.record_removals();
    ok(&bench.run(&[]));
    let named: BTreeSet<PathBuf> = [DESKTOP, ICON, CATALOG, TRANSLATIONS, OPENPODS, BUILD]
        .iter()
        .map(|name| bench.path(name))
        .collect();
    assert_eq!(removed_leftovers(&bench), named);
    for path in removed_paths(&bench) {
        assert!(path.is_absolute(), "{}", path.display());
        let name = path.file_name().unwrap().to_string_lossy();
        let scratch = path.parent() == Some(bench.path("runtime").as_path())
            && name.starts_with("omapods-setup.");
        assert!(named.contains(&path) || scratch, "{}", path.display());
    }
}

#[test]
fn a_home_that_is_empty_or_relative_stops_setup_before_any_removal() {
    for home in ["", "home"] {
        let bench = Bench::new();
        plant_old_install(&bench);
        plant_old_config(&bench);
        bench.record_removals();
        let before = bench.home_tree();
        let ran = bench.run_as(&[], home);
        assert_ne!(ran.code, Some(0), "{home:?}: {}", ran.stdout);
        assert!(
            ran.stderr.contains("setup: HOME"),
            "{home:?}: {}",
            ran.stderr
        );
        assert_eq!(removed_paths(&bench), Vec::<PathBuf>::new(), "{home:?}");
        assert_eq!(bench.home_tree(), before, "{home:?}");
        assert!(exists(&bench, CACHE), "{home:?}");
    }
}
