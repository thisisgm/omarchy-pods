//! `setup --uninstall` run through the real installer: what it stops, what it removes and what it leaves.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use crate::bench::{Bench, Ran, Tree};
use crate::install::{BINARY, CLIENT, UNIT, USAGE_EXIT, USAGE_LINES, files, succeeded};
use crate::old_install::{
    Event, OLD_CONFIG, OLD_CONFIG_TEXT, events, exists, plant, plant_old_config,
    removed_after_naming, removed_paths,
};

const STATE: &str = "home/.local/state/librepods";
const KEYS: &str = "home/.local/state/librepods/keys.json";
const STATUS: &str = "home/.local/state/librepods/status.json";
const SETTINGS: &str = "home/.local/state/librepods/settings.json";
/// A state directory under an `XDG_STATE_HOME` outside the home.
const XDG_STATE: &str = "xdg state/librepods";
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
    bench.clear_calls();
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
    let kept = format!("Kept {}.\n{CLOSING}", unpaired.path(STATE).display());
    assert!(ran.stdout.ends_with(&kept), "{}", ran.stdout);
    assert!(!ran.stdout.contains("keys.json"), "{}", ran.stdout);
}

/// The error a refused run prints, and the run that leads to it.
type Refused = (&'static str, fn(&Bench) -> Ran);

#[test]
fn a_state_path_that_is_empty_relative_or_the_root_stops_before_any_removal() {
    let cases: [Refused; 7] = [
        ("setup: HOME is not an absolute path: unset\n", |bench| {
            bench.run_as(&UNINSTALL, "")
        }),
        ("setup: HOME is not an absolute path: home\n", |bench| {
            bench.run_as(&UNINSTALL, "home")
        }),
        (
            "setup: refusing to remove a state directory whose base is the root or not an absolute path: /librepods\n",
            |bench| bench.run_with_state_home(&UNINSTALL, "/"),
        ),
        (
            "setup: refusing to remove a state directory whose base is the root or not an absolute path: //librepods\n",
            |bench| bench.run_with_state_home(&UNINSTALL, "//"),
        ),
        (
            "setup: refusing to remove a state directory whose base is the root or not an absolute path: ///librepods\n",
            |bench| bench.run_with_state_home(&UNINSTALL, "///"),
        ),
        (
            "setup: refusing to remove a state directory whose base is the root or not an absolute path: /./librepods\n",
            |bench| bench.run_with_state_home(&UNINSTALL, "/."),
        ),
        (
            "setup: refusing to remove a state directory whose base is the root or not an absolute path: /../librepods\n",
            |bench| bench.run_with_state_home(&UNINSTALL, "/.."),
        ),
    ];
    for (said, run) in cases {
        let bench = Bench::new();
        install(&bench);
        plant_state(&bench, STATE);
        bench.record_removals();
        let before = bench.home_tree();
        let ran = run(&bench);
        assert_ne!(ran.code, Some(0), "{said}: {}", ran.stdout);
        assert_eq!(ran.stderr, said);
        assert_eq!(ran.stdout, "", "{said}");
        assert_eq!(removed_paths(&bench), Vec::<PathBuf>::new(), "{said}");
        assert_eq!(bench.calls_of("systemctl"), Vec::<String>::new(), "{said}");
        assert_eq!(bench.home_tree(), before, "{said}");
    }
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
fn uninstall_runs_nothing_of_the_binary() {
    for args in [&UNINSTALL[..], &KEEP_KEYS[..]] {
        let bench = Bench::new();
        install(&bench);
        plant_state(&bench, STATE);
        ok(&bench.run(args));
        assert_eq!(bench.calls_of("systemctl"), [STOP, RELOAD], "{args:?}");
        let programs: Vec<String> = bench
            .call_arguments()
            .into_iter()
            .map(|call| call[0].clone())
            .collect();
        assert!(
            !programs.iter().any(|program| program.contains("librepods")),
            "{programs:?}"
        );
    }
}

#[test]
fn uninstall_twice_is_quiet() {
    let bench = Bench::new();
    install(&bench);
    plant_state(&bench, STATE);
    ok(&bench.run(&UNINSTALL));
    bench.clear_calls();
    bench.record_removals();
    let ran = bench.run(&UNINSTALL);
    ok(&ran);
    assert_eq!(ran.stdout, CLOSING);
    assert_eq!(ran.stderr, "");
    assert_eq!(bench.calls(), [ID_CHECK]);

    let kept = Bench::new();
    install(&kept);
    plant_state(&kept, STATE);
    ok(&kept.run(&KEEP_KEYS));
    kept.clear_calls();
    kept.record_removals();
    let ran = kept.run(&KEEP_KEYS);
    ok(&ran);
    assert_eq!(ran.stdout, format!("{}{CLOSING}", kept_line(&kept, STATE)));
    assert_eq!(ran.stderr, "");
    assert_eq!(kept.calls(), [ID_CHECK]);
}

#[test]
fn uninstall_where_setup_never_ran_is_quiet_and_exits_zero() {
    for args in [&UNINSTALL[..], &KEEP_KEYS[..]] {
        let bench = Bench::new();
        bench.record_removals();
        let ran = bench.run(args);
        ok(&ran);
        assert_eq!(ran.stdout, CLOSING, "{args:?}");
        assert_eq!(ran.stderr, "", "{args:?}");
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
        let before = bench.outside_home(&[]);
        install(&bench);
        plant_state(&bench, STATE);
        ok(&bench.run(args));
        assert_eq!(bench.outside_home(&[]), before, "{args:?}");
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
