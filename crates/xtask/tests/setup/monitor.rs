//! Whether BlueZ serves the advertisement monitor manager, read once at the end of an install through the real installer.

use std::fs;

use crate::bench::{ADAPTER_OBJECT, Bench, Ran, SERVED_TYPES, repo, script_constant};
use crate::install::done_lines;

const POINTER: &str = "BlueZ experimental features are off, so the case battery, lid and ear state do not show while the AirPods are away. The README section \"Case battery while away\" says how to turn them on.\n";
const READ: [&str; 8] = [
    "busctl",
    "--system",
    "--timeout=2",
    "get-property",
    "org.bluez",
    ADAPTER_OBJECT,
    "org.bluez.AdvertisementMonitorManager1",
    "SupportedMonitorTypes",
];
const BINARY: &str = "home/.local/bin/librepods";
const RESTART: &str = "systemctl --user restart librepods.service";

/// How a case arranges the read, and whether BlueZ then counts as serving the manager.
type Arm = (&'static str, fn(&Bench), bool);

const ARMS: [Arm; 5] = [
    ("served", |_| {}, true),
    ("not served", not_served, false),
    (
        "failed with the served reply",
        failed_with_the_served_reply,
        false,
    ),
    ("served without or_patterns", without_or_patterns, false),
    ("no busctl", without_busctl, false),
];

fn not_served(bench: &Bench) {
    bench.set("busctl-output", "");
    bench.set("busctl-exit", "1\n");
}

fn failed_with_the_served_reply(bench: &Bench) {
    bench.set("busctl-output", SERVED_TYPES);
    bench.set("busctl-exit", "1\n");
}

fn without_or_patterns(bench: &Bench) {
    bench.set("busctl-output", "as 0\n");
}

fn without_busctl(bench: &Bench) {
    fs::remove_file(bench.path("bin/busctl")).unwrap();
}

/// An install that succeeded, said nothing on standard error, and printed the pointer after its own lines or did not.
fn reported(ran: &Ran, pointer: bool) {
    assert_eq!(ran.code, Some(0), "the install failed: {}", ran.stderr);
    assert_eq!(ran.stderr, "");
    let wanted = if pointer {
        format!("{}{POINTER}", done_lines())
    } else {
        done_lines()
    };
    assert_eq!(ran.stdout, wanted);
}

#[test]
fn a_served_manager_prints_nothing_about_it() {
    let bench = Bench::new();
    reported(&bench.run(&[]), false);
    assert_eq!(bench.arguments_of("busctl"), [READ]);
}

#[test]
fn a_manager_not_served_prints_the_pointer() {
    let bench = Bench::new();
    not_served(&bench);
    reported(&bench.run(&[]), true);
    assert_eq!(bench.arguments_of("busctl"), [READ]);
}

#[test]
fn a_failed_busctl_reads_as_not_served_whatever_it_prints() {
    let bench = Bench::new();
    failed_with_the_served_reply(&bench);
    reported(&bench.run(&[]), true);
    assert_eq!(bench.arguments_of("busctl"), [READ]);
}

#[test]
fn an_exit_of_zero_without_or_patterns_reads_as_not_served() {
    let bench = Bench::new();
    without_or_patterns(&bench);
    reported(&bench.run(&[]), true);
    assert_eq!(bench.arguments_of("busctl"), [READ]);
}

#[test]
fn a_missing_busctl_reads_as_not_served() {
    let bench = Bench::new();
    without_busctl(&bench);
    reported(&bench.run(&[]), true);
    assert_eq!(bench.calls_of("busctl"), Vec::<String>::new());
}

#[test]
fn the_read_is_the_one_busctl_call() {
    let bench = Bench::new();
    let stub = fs::symlink_metadata(bench.path("bin/busctl")).unwrap();
    assert!(stub.is_file(), "the bench's busctl is not its own stub");
    let text = fs::read_to_string(bench.path("bin/busctl")).unwrap();
    assert!(text.contains("log busctl"), "{text}");
    reported(&bench.run(&[]), false);
    let calls = bench.call_arguments();
    assert_eq!(bench.arguments_of("busctl"), [READ]);
    assert_eq!(calls.last().unwrap(), &READ, "{calls:?}");
}

#[test]
fn setup_succeeds_whether_or_not_the_manager_is_served() {
    for (name, arrange, served) in ARMS {
        let bench = Bench::new();
        arrange(&bench);
        let ran = bench.run(&[]);
        assert_eq!(ran.code, Some(0), "{name}: {}", ran.stderr);
        assert_eq!(ran.stdout.contains(POINTER), !served, "{name}");
        assert!(bench.path(BINARY).is_file(), "{name}");
        assert!(
            bench
                .calls_of("systemctl")
                .iter()
                .any(|call| call == RESTART),
            "{name}"
        );
    }
}

#[test]
fn nothing_is_written_outside_the_bench_home() {
    for (name, arrange, served) in ARMS {
        let bench = Bench::new();
        arrange(&bench);
        let before = bench.outside_home(&[]);
        let ran = bench.run(&[]);
        assert_eq!(ran.stdout.contains(POINTER), !served, "{name}");
        assert_eq!(bench.outside_home(&[]), before, "{name}");
    }
}

#[test]
fn the_pointer_names_a_readme_heading() {
    let section = script_constant("readme_section");
    let readme = fs::read_to_string(repo().join("README.md")).unwrap();
    // Sample input: `## Case battery while away`
    let headings: Vec<&str> = readme
        .lines()
        .filter(|line| line.starts_with('#'))
        .filter_map(|line| line.trim_start_matches('#').strip_prefix(' '))
        .collect();
    assert!(
        headings.contains(&section.as_str()),
        "{section:?} in {headings:?}"
    );
    assert!(POINTER.contains(&format!("\"{section}\"")));
}
