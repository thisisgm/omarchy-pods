//! `setup --uninstall` run through the real installer: what it stops, what it removes and what it leaves.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use crate::bench::{BUILT_DAEMON_LINK, Bench, Ran, Tree};
use crate::install::{BINARY, CLIENT, UNIT, USAGE_EXIT, USAGE_LINES, files, succeeded};
use crate::old_install::{
    Event, OLD_CONFIG, OLD_CONFIG_TEXT, events, exists, plant, plant_old_config,
    removed_after_naming, removed_paths,
};

const STATE: &str = "home/.local/state/librepods";
const KEYS: &str = "home/.local/state/librepods/keys.json";
const STATUS: &str = "home/.local/state/librepods/status.json";
const SETTINGS: &str = "home/.local/state/librepods/settings.json";
/// A state directory under an `XDG_STATE_HOME` inside the home.
const XDG_STATE: &str = "home/xdg state/librepods";
/// What `timeout` exits with when it has to kill a command that outlived its deadline.
const KILLED: u8 = 137;
const KEYS_TEXT: &str = "{\"irk\":\"synthetic\"}\n";
const SETTINGS_TEXT: &str = "{\"noise_control\":\"anc\"}\n";
const CLOSING: &str =
    "Removed. To remove the plugin itself: omarchy plugin remove io.github.thisisgm.omapods\n";
const STOP: &str = "systemctl --user disable --now librepods.service";
const RELOAD: &str = "systemctl --user daemon-reload";
const UNINSTALL: [&str; 1] = ["--uninstall"];
const KEEP_KEYS: [&str; 2] = ["--uninstall", "--keep-keys"];
/// The one call an uninstall of nothing makes: the user id check every run shares.
const ID_CHECK: &str = "id -u";

/// `name` under the home, as a path from the bench root.
fn home(name: &str) -> String {
    format!("home/{name}")
}

fn install(bench: &Bench) {
    succeeded(&bench.run(&[]));
    bench.link_built_daemon();
    bench.clear_calls();
}

/// The line an uninstall prints when the installed binary is not there to name the state directory.
fn no_binary_line(bench: &Bench) -> String {
    format!(
        "setup: left any state directory in place, because {} is not there to name it.\n",
        bench.path(&home(BINARY)).display()
    )
}

/// The line an uninstall prints when the installed binary's `state-dir` exits with `status`.
fn lookup_failed_line(bench: &Bench, status: u8) -> String {
    format!(
        "setup: left any state directory in place, because {} state-dir exited with status {status}.\n",
        bench.path(&home(BINARY)).display()
    )
}

/// The refusal of a directory the daemon named outside the home.
fn outside_home(dir: &str) -> String {
    format!("setup: refusing to remove a state directory outside the home: {dir}\n")
}

/// Where a linked state directory's files live, inside the home.
const LINKED: &str = "home/elsewhere/librepods";

/// Plants the state in `LINKED` and makes the daemon's state directory a link to it.
fn plant_linked_state(bench: &Bench) {
    plant_state(bench, LINKED);
    fs::create_dir_all(bench.path("home/.local/state")).unwrap();
    symlink(bench.path(LINKED), bench.path(STATE)).unwrap();
}

/// What the daemon keeps in its state directory while it runs.
fn plant_state(bench: &Bench, dir: &str) {
    plant(
        &bench.path(&format!("{dir}/keys.json")),
        KEYS_TEXT.as_bytes(),
    );
    plant(
        &bench.path(&format!("{dir}/settings.json")),
        SETTINGS_TEXT.as_bytes(),
    );
    plant(
        &bench.path(&format!("{dir}/status.json")),
        b"{\"schema_version\":1}\n",
    );
}

fn ok(ran: &Ran) {
    assert_eq!(ran.code, Some(0), "the uninstall failed: {}", ran.stderr);
    assert!(ran.stdout.ends_with(CLOSING), "{}", ran.stdout);
}

fn kept_line(bench: &Bench, dir: &str) -> String {
    format!(
        "Kept {}: its keys.json holds the pairing secrets of your AirPods.\n",
        bench.path(dir).display()
    )
}

fn paths(bench: &Bench, names: &[&str]) -> BTreeSet<PathBuf> {
    names.iter().map(|name| bench.path(name)).collect()
}

#[test]
fn uninstall_stops_the_unit_and_removes_what_setup_installed() {
    let bench = Bench::new();
    install(&bench);
    plant_state(&bench, STATE);
    let ran = bench.run(&UNINSTALL);
    ok(&ran);
    assert_eq!(ran.stderr, "");
    assert_eq!(files(&bench.home_tree()), BTreeSet::new());
    assert_eq!(bench.calls_of("systemctl"), [STOP, RELOAD]);
    assert_eq!(bench.calls_of("sudo"), Vec::<String>::new());
}

#[test]
fn the_state_directory_goes_with_the_keys_unless_asked_to_keep_them() {
    let bench = Bench::new();
    install(&bench);
    plant_state(&bench, STATE);
    ok(&bench.run(&UNINSTALL));
    assert!(!exists(&bench, STATE));

    let kept = Bench::new();
    install(&kept);
    plant_state(&kept, STATE);
    ok(&kept.run(&KEEP_KEYS));
    assert_eq!(fs::read_to_string(kept.path(KEYS)).unwrap(), KEYS_TEXT);
    assert_eq!(
        fs::read_to_string(kept.path(SETTINGS)).unwrap(),
        SETTINGS_TEXT
    );
    assert!(!exists(&kept, STATUS));
    let left: BTreeSet<&Path> = [
        Path::new(".local/state/librepods/keys.json"),
        Path::new(".local/state/librepods/settings.json"),
    ]
    .into();
    assert_eq!(files(&kept.home_tree()), left);
}

#[test]
fn the_state_directory_is_found_as_the_daemon_finds_it() {
    let bench = Bench::new();
    install(&bench);
    plant_state(&bench, STATE);
    plant_state(&bench, XDG_STATE);
    let state_home = bench
        .path(XDG_STATE)
        .parent()
        .unwrap()
        .display()
        .to_string();
    ok(&bench.run_with_state_home(&UNINSTALL, &state_home));
    assert!(!exists(&bench, XDG_STATE));
    assert!(exists(&bench, KEYS));

    let relative = Bench::new();
    install(&relative);
    plant_state(&relative, STATE);
    ok(&relative.run_with_state_home(&UNINSTALL, "xdg state"));
    assert!(!exists(&relative, STATE));
}

#[test]
fn kept_keys_are_named_with_their_warning() {
    let bench = Bench::new();
    install(&bench);
    plant_state(&bench, STATE);
    let ran = bench.run(&KEEP_KEYS);
    ok(&ran);
    assert!(
        ran.stdout.contains(&kept_line(&bench, STATE)),
        "{}",
        ran.stdout
    );

    let removed = Bench::new();
    install(&removed);
    plant_state(&removed, STATE);
    let ran = removed.run(&UNINSTALL);
    ok(&ran);
    assert!(!ran.stdout.contains("Kept "), "{}", ran.stdout);

    let unpaired = Bench::new();
    install(&unpaired);
    plant(&unpaired.path(SETTINGS), SETTINGS_TEXT.as_bytes());
    let ran = unpaired.run(&KEEP_KEYS);
    ok(&ran);
    let kept = format!("Kept {}.\n", unpaired.path(STATE).display());
    assert!(ran.stdout.contains(&kept), "{}", ran.stdout);
    assert!(!ran.stdout.contains("keys.json"), "{}", ran.stdout);
}

/// The value `XDG_STATE_HOME` is set to, and the directory the daemon then names.
type Refused = (&'static str, &'static str);

#[test]
fn a_state_directory_outside_the_home_stops_before_any_removal() {
    let cases: [Refused; 5] = [
        ("/", "/librepods"),
        ("//", "//librepods"),
        ("///", "///librepods"),
        ("/.", "/./librepods"),
        ("/..", "/../librepods"),
    ];
    for (state_home, named) in cases {
        let bench = Bench::new();
        install(&bench);
        plant_state(&bench, STATE);
        bench.record_removals();
        let before = bench.home_tree();
        let ran = bench.run_with_state_home(&UNINSTALL, state_home);
        assert_refused(&bench, &ran, &outside_home(named), &before);
    }
    for base in ["xdg state", "home/.."] {
        let bench = Bench::new();
        install(&bench);
        let dir = format!("{base}/librepods");
        plant_state(&bench, &dir);
        bench.record_removals();
        let before = bench.home_tree();
        let state_home = bench.path(base).display().to_string();
        let ran = bench.run_with_state_home(&UNINSTALL, &state_home);
        let named = format!("{state_home}/librepods");
        assert_refused(&bench, &ran, &outside_home(&named), &before);
        assert!(exists(&bench, &format!("{dir}/keys.json")), "{dir}");
    }
}

#[test]
fn a_home_that_is_empty_or_relative_stops_before_any_removal() {
    for (home, said) in [("", "unset"), ("home", "home")] {
        let bench = Bench::new();
        install(&bench);
        plant_state(&bench, STATE);
        bench.record_removals();
        let before = bench.home_tree();
        let ran = bench.run_as(&UNINSTALL, home);
        let refusal = format!("setup: HOME is not an absolute path: {said}\n");
        assert_refused(&bench, &ran, &refusal, &before);
    }
}

fn assert_refused(bench: &Bench, ran: &Ran, said: &str, before: &Tree) {
    assert_ne!(ran.code, Some(0), "{said}: {}", ran.stdout);
    assert_eq!(ran.stderr, said);
    assert_eq!(ran.stdout, "", "{said}");
    assert_eq!(removed_paths(bench), Vec::<PathBuf>::new(), "{said}");
    assert_eq!(bench.calls_of("systemctl"), Vec::<String>::new(), "{said}");
    assert_eq!(&bench.home_tree(), before, "{said}");
}

#[test]
fn a_binary_that_cannot_name_the_directory_leaves_it_and_removes_the_rest() {
    let missing = Bench::new();
    install(&missing);
    plant_state(&missing, STATE);
    fs::remove_file(missing.path(&home(BINARY))).unwrap();
    let ran = missing.run(&UNINSTALL);
    ok(&ran);
    assert_eq!(ran.stderr, no_binary_line(&missing));
    assert_only_the_state_is_left(&missing);

    let failing = Bench::new();
    install(&failing);
    plant_state(&failing, STATE);
    failing.set("state-dir-fails", "");
    let ran = failing.run(&KEEP_KEYS);
    ok(&ran);
    assert_eq!(
        ran.stderr,
        format!(
            "librepods: cannot place the state file\n{}",
            lookup_failed_line(&failing, 1)
        )
    );
    assert!(!ran.stdout.contains("Kept "), "{}", ran.stdout);
    assert_only_the_state_is_left(&failing);

    let hanging = Bench::new();
    install(&hanging);
    plant_state(&hanging, STATE);
    hanging.set("state-dir-hangs", "");
    let ran = hanging.run(&UNINSTALL);
    ok(&ran);
    assert_eq!(ran.stderr, lookup_failed_line(&hanging, KILLED));
    assert_only_the_state_is_left(&hanging);
}

#[test]
fn a_rerun_after_a_failed_reload_still_finds_the_state() {
    let bench = Bench::new();
    install(&bench);
    plant_state(&bench, STATE);
    bench.set("systemctl-fails", "daemon-reload");
    let ran = bench.run(&UNINSTALL);
    assert_ne!(ran.code, Some(0), "{}", ran.stderr);
    fs::remove_file(bench.path("stub/systemctl-fails")).unwrap();
    let ran = bench.run(&UNINSTALL);
    ok(&ran);
    assert_eq!(ran.stderr, "");
    assert_eq!(files(&bench.home_tree()), BTreeSet::new());
}

#[test]
fn a_linked_state_directory_is_removed_through_its_target_then_the_link() {
    let bench = Bench::new();
    install(&bench);
    plant_linked_state(&bench);
    let ran = bench.run(&UNINSTALL);
    ok(&ran);
    let removing = format!(
        "Removing {}\nRemoving {}\n",
        bench.path(LINKED).display(),
        bench.path(STATE).display()
    );
    assert!(!exists(&bench, LINKED), "{}", ran.stdout);
    assert!(!exists(&bench, STATE), "{}", ran.stdout);
    assert!(ran.stdout.contains(&removing), "{}", ran.stdout);
}

#[test]
fn a_state_link_to_a_missing_directory_is_removed() {
    let bench = Bench::new();
    install(&bench);
    fs::create_dir_all(bench.path(STATE).parent().unwrap()).unwrap();
    symlink(bench.path(LINKED), bench.path(STATE)).unwrap();
    let ran = bench.run(&UNINSTALL);
    ok(&ran);
    assert!(!exists(&bench, STATE), "{}", ran.stdout);
}

#[test]
fn keep_keys_keeps_a_linked_state_directory_and_its_link() {
    let bench = Bench::new();
    install(&bench);
    plant_linked_state(&bench);
    let ran = bench.run(&KEEP_KEYS);
    ok(&ran);
    assert!(
        ran.stdout.contains(&kept_line(&bench, LINKED)),
        "{}",
        ran.stdout
    );
    let keys = fs::read_to_string(bench.path(&format!("{LINKED}/keys.json"))).unwrap();
    assert_eq!(keys, KEYS_TEXT);
    assert!(exists(&bench, &format!("{LINKED}/settings.json")));
    assert!(!exists(&bench, &format!("{LINKED}/status.json")));
    assert_eq!(
        fs::read_link(bench.path(STATE)).unwrap(),
        bench.path(LINKED)
    );
}

#[test]
fn a_state_link_out_of_the_home_stops_before_any_removal() {
    let bench = Bench::new();
    install(&bench);
    plant_state(&bench, "outside/librepods");
    fs::create_dir_all(bench.path("home/.local/state")).unwrap();
    symlink(bench.path("outside/librepods"), bench.path(STATE)).unwrap();
    bench.record_removals();
    let before = bench.home_tree();
    let ran = bench.run(&UNINSTALL);
    assert_refused(
        &bench,
        &ran,
        &outside_home(&bench.path(STATE).display().to_string()),
        &before,
    );
    assert!(exists(&bench, "outside/librepods/keys.json"));
}

fn assert_only_the_state_is_left(bench: &Bench) {
    let left: BTreeSet<&Path> = [
        Path::new(".local/state/librepods/keys.json"),
        Path::new(".local/state/librepods/settings.json"),
        Path::new(".local/state/librepods/status.json"),
    ]
    .into();
    assert_eq!(files(&bench.home_tree()), left);
    assert_eq!(bench.calls_of("systemctl"), [STOP, RELOAD]);
}

#[test]
fn every_removal_is_printed_first() {
    for (args, state) in [(&UNINSTALL[..], STATE), (&KEEP_KEYS[..], STATUS)] {
        let bench = Bench::new();
        install(&bench);
        plant_state(&bench, STATE);
        bench.record_removals();
        let ran = bench.run_into_log(args);
        assert_eq!(ran.code, Some(0), "{args:?}: {}", ran.stderr);
        let log = events(&bench);
        let removed = [home(CLIENT), home(BINARY), home(UNIT), state.to_owned()];
        for name in &removed {
            removed_after_naming(&log, &bench, "rm", name);
        }
        let named: Vec<&str> = removed.iter().map(String::as_str).collect();
        let removals: BTreeSet<PathBuf> = removed_paths(&bench).into_iter().collect();
        assert_eq!(removals, paths(&bench, &named), "{args:?}");
        let closing = Event::Printed(CLOSING.trim_end().to_owned());
        assert_eq!(log.last(), Some(&closing), "{args:?}");
    }
}

#[test]
fn a_client_that_is_a_real_file_is_left_and_said() {
    let arrangements: [fn(&Path); 2] = [
        |client| fs::write(client, "a client of my own\n").unwrap(),
        |client| symlink("elsewhere", client).unwrap(),
    ];
    for arrange in arrangements {
        let bench = Bench::new();
        install(&bench);
        let client = bench.path(&home(CLIENT));
        fs::remove_file(&client).unwrap();
        arrange(&client);
        let before = fs::symlink_metadata(&client).unwrap().file_type();
        let ran = bench.run(&UNINSTALL);
        ok(&ran);
        assert_eq!(
            ran.stderr,
            format!(
                "setup: left {}: it is not the link setup made.\n",
                client.display()
            )
        );
        assert_eq!(fs::symlink_metadata(&client).unwrap().file_type(), before);
        assert!(!exists(&bench, &home(BINARY)));
    }
}

#[test]
fn uninstall_asks_the_binary_only_where_its_state_is() {
    for args in [&UNINSTALL[..], &KEEP_KEYS[..]] {
        let bench = Bench::new();
        install(&bench);
        plant_state(&bench, STATE);
        ok(&bench.run(args));
        assert_eq!(bench.calls_of("systemctl"), [STOP, RELOAD], "{args:?}");
        let asked: Vec<Vec<String>> = bench
            .call_arguments()
            .into_iter()
            .filter(|call| call[0].contains("librepods"))
            .collect();
        assert_eq!(asked, [["librepods", "state-dir"]], "{args:?}");
    }
}

#[test]
fn uninstall_twice_removes_nothing_more() {
    let bench = Bench::new();
    install(&bench);
    plant_state(&bench, STATE);
    ok(&bench.run(&UNINSTALL));
    bench.clear_calls();
    bench.record_removals();
    let ran = bench.run(&UNINSTALL);
    ok(&ran);
    assert_eq!(ran.stdout, CLOSING);
    assert_eq!(ran.stderr, no_binary_line(&bench));
    assert_eq!(bench.calls(), [ID_CHECK]);

    let kept = Bench::new();
    install(&kept);
    plant_state(&kept, STATE);
    ok(&kept.run(&KEEP_KEYS));
    kept.clear_calls();
    kept.record_removals();
    let ran = kept.run(&KEEP_KEYS);
    ok(&ran);
    assert_eq!(ran.stdout, CLOSING);
    assert_eq!(ran.stderr, no_binary_line(&kept));
    assert_eq!(kept.calls(), [ID_CHECK]);
    assert_eq!(fs::read_to_string(kept.path(KEYS)).unwrap(), KEYS_TEXT);
}

#[test]
fn uninstall_where_setup_never_ran_removes_nothing_and_exits_zero() {
    for args in [&UNINSTALL[..], &KEEP_KEYS[..]] {
        let bench = Bench::new();
        bench.record_removals();
        let ran = bench.run(args);
        ok(&ran);
        assert_eq!(ran.stdout, CLOSING, "{args:?}");
        assert_eq!(ran.stderr, no_binary_line(&bench), "{args:?}");
        assert_eq!(bench.calls(), [ID_CHECK], "{args:?}");
        assert_eq!(bench.home_tree(), Tree::new(), "{args:?}");
    }
}

#[test]
fn the_plugin_directory_and_the_old_config_are_never_removed() {
    let bench = Bench::new();
    install(&bench);
    plant_old_config(&bench);
    bench.record_removals();
    let before = bench.outside_home(&[]);
    ok(&bench.run(&UNINSTALL));
    assert_eq!(
        fs::read_to_string(bench.path(OLD_CONFIG)).unwrap(),
        OLD_CONFIG_TEXT
    );
    assert_eq!(bench.outside_home(&[]), before);
    for path in removed_paths(&bench) {
        assert!(
            !path.starts_with(bench.path("plugin")),
            "{}",
            path.display()
        );
        assert!(
            !path.starts_with(bench.path("home/.config")),
            "{}",
            path.display()
        );
    }
}

#[test]
fn keep_keys_without_uninstall_is_a_usage_error() {
    let cases: [&[&str]; 3] = [
        &["--keep-keys"],
        &["--keep-keys", "--from-source"],
        &["--from-source", "--keep-keys"],
    ];
    for args in cases {
        let bench = Bench::new();
        install(&bench);
        let before = bench.home_tree();
        let ran = bench.run(args);
        assert_eq!(ran.code, Some(USAGE_EXIT), "{args:?}: {}", ran.stderr);
        assert_eq!(ran.stdout, "", "{args:?}");
        assert_eq!(ran.stderr.lines().count(), USAGE_LINES, "{}", ran.stderr);
        assert!(ran.stderr.starts_with("usage: setup"), "{}", ran.stderr);
        assert_eq!(bench.calls(), Vec::<String>::new(), "{args:?}");
        assert_eq!(bench.home_tree(), before, "{args:?}");
    }
}

#[test]
fn nothing_outside_the_bench_home_is_touched() {
    for args in [&UNINSTALL[..], &KEEP_KEYS[..]] {
        let bench = Bench::new();
        let before = bench.outside_home(&[BUILT_DAEMON_LINK]);
        install(&bench);
        plant_state(&bench, STATE);
        ok(&bench.run(args));
        assert_eq!(bench.outside_home(&[BUILT_DAEMON_LINK]), before, "{args:?}");
    }
}

#[test]
fn a_service_that_cannot_be_stopped_stops_the_uninstall() {
    let bench = Bench::new();
    install(&bench);
    plant_state(&bench, STATE);
    bench.set("systemctl-fails", "disable");
    let before = bench.home_tree();
    let ran = bench.run(&UNINSTALL);
    assert_ne!(ran.code, Some(0), "{}", ran.stdout);
    assert_eq!(
        ran.stderr,
        "Failed to connect to bus: No medium found\n\
         setup: systemctl could not stop and disable the service: librepods.service\n"
    );
    assert_eq!(ran.stdout, "");
    assert_eq!(bench.home_tree(), before);
}
