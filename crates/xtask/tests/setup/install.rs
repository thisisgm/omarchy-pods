//! Each case runs the real installer and judges its exit, its output, the calls it made and the files it left.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use crate::bench::{ADAPTER_OBJECT, ASSET, Bench, Node, Ran, Tree, VERSION, repo, script_constant};

/// How far past its wait a run that never hears the daemon may end.
const WAIT_SLACK: Duration = Duration::from_secs(5);
/// The runs of the silent daemon case, so its wall time is a range.
const SILENT_RUNS: usize = 3;
pub(crate) const USAGE_LINES: usize = 6;
pub(crate) const USAGE_EXIT: i32 = 2;
pub(crate) const BINARY: &str = ".local/bin/librepods";
pub(crate) const CLIENT: &str = ".local/bin/librepods-ctl";
pub(crate) const UNIT: &str = ".local/share/systemd/user/librepods.service";
const RESTART: &str = "systemctl --user restart librepods.service";
const NOT_STARTED: &str =
    "setup: librepods did not start. See: journalctl --user -u librepods.service -n 30\n";
const PRIVILEGE_WORDS: [&str; 5] = ["sudo", "pkexec", "doas", "su -c", "run0"];
/// The plugin id and the setting the success message tells the user to set, both from `manifest.json`.
const PLUGIN_ID: &str = "io.github.thisisgm.omapods";
const HIDE_KEY: &str = "hideWhenDisconnected";
/// Where curl's `--output` path sits in its arguments.
const CURL_OUTPUT_AT: usize = 11;
/// The exit status the cargo stub fails with, as cargo does on a compile error.
const CARGO_FAILED: &str = "exit status 101";

pub(crate) fn done_lines() -> String {
    format!(
        "librepods {VERSION} is running.\n\
         The bar icon stays hidden until AirPods are connected.\n\
         Keep it visible: omarchy bar set {PLUGIN_ID} {HIDE_KEY} false --json\n"
    )
}

pub(crate) fn succeeded(ran: &Ran) {
    assert_eq!(ran.code, Some(0), "the install failed: {}", ran.stderr);
    assert_eq!(ran.stdout, done_lines());
}

/// A refused run: non zero, one `setup:` line or more, nothing under the home and no unit touched.
fn installed_nothing(bench: &Bench, ran: &Ran) {
    assert_ne!(ran.code, Some(0), "the run succeeded: {}", ran.stdout);
    assert!(
        ran.stderr.contains("setup: "),
        "no setup line: {}",
        ran.stderr
    );
    assert_eq!(bench.home_tree(), Tree::new());
    assert_eq!(bench.calls_of("systemctl"), Vec::<String>::new());
}

pub(crate) fn files(tree: &Tree) -> BTreeSet<&Path> {
    tree.iter()
        .filter(|(_, node)| !matches!(node, Node::Dir(_)))
        .map(|(path, _)| path.as_path())
        .collect()
}

#[test]
fn the_printed_command_names_the_manifest_id_and_setting() {
    let text = fs::read_to_string(repo().join("manifest.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(manifest["id"], PLUGIN_ID);
    let schema = manifest["barWidget"]["schema"].as_array().unwrap();
    assert!(
        schema.iter().any(|setting| setting["key"] == HIDE_KEY),
        "{schema:?}"
    );
}

#[test]
fn a_clean_install_puts_the_binary_the_link_and_the_unit_in_place() {
    let bench = Bench::new();
    let ran = bench.run(&[]);
    succeeded(&ran);
    let home = bench.home_tree();
    let unit = fs::read(repo().join("packaging/systemd/librepods.service")).unwrap();
    assert_eq!(
        home.get(Path::new(BINARY)),
        Some(&Node::File(0o755, bench.payload()))
    );
    assert_eq!(
        home.get(Path::new(CLIENT)),
        Some(&Node::Link(PathBuf::from("librepods")))
    );
    assert_eq!(home.get(Path::new(UNIT)), Some(&Node::File(0o644, unit)));
    let wanted = BTreeSet::from([Path::new(BINARY), Path::new(CLIENT), Path::new(UNIT)]);
    assert_eq!(files(&home), wanted);
}

#[test]
fn a_checksum_that_does_not_match_installs_nothing() {
    let bench = Bench::new();
    bench.set("served", "not the published file\n");
    let ran = bench.run(&[]);
    installed_nothing(&bench, &ran);
    assert!(ran.stderr.contains("does not match"), "{}", ran.stderr);
    assert!(ran.stderr.contains(ASSET), "{}", ran.stderr);
}

#[test]
fn a_download_that_fails_installs_nothing_and_names_the_asset() {
    let bench = Bench::new();
    bench.set("curl-fails", "");
    let ran = bench.run(&[]);
    installed_nothing(&bench, &ran);
    assert!(ran.stderr.contains("returned error: 404"), "{}", ran.stderr);
    assert!(ran.stderr.contains(ASSET), "{}", ran.stderr);
    assert!(ran.stderr.contains("curl exit status 22"), "{}", ran.stderr);
}

#[test]
fn an_empty_download_is_refused() {
    let bench = Bench::new();
    bench.set("served", "");
    let ran = bench.run(&[]);
    installed_nothing(&bench, &ran);
    assert!(ran.stderr.contains("empty"), "{}", ran.stderr);
    assert!(ran.stderr.contains(ASSET), "{}", ran.stderr);
}

#[test]
fn a_lock_line_that_is_not_a_checksum_is_refused() {
    let hash = Bench::new().lock_line();
    // Sample input: `9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08  librepods-9.8.7-x86_64`
    let hash = hash.split(' ').next().unwrap().to_owned();
    let short: String = hash.chars().skip(1).collect();
    let bad = [
        String::new(),
        "# no release yet\n".to_owned(),
        "\n".to_owned(),
        format!("{short}  {ASSET}\n"),
        format!("{short}g  {ASSET}\n"),
        format!("{hash} {ASSET}\n"),
        format!("{hash} *{ASSET}\n"),
        format!("{hash}  librepods-{VERSION}-aarch64\n"),
        format!("{hash}  other-{VERSION}-x86_64\n"),
        format!("{hash}  librepods--x86_64\n"),
        format!("{hash}  librepods-../../9.8.7-x86_64\n"),
        format!("{hash}  {ASSET} \n"),
    ];
    for text in bad {
        let bench = Bench::new();
        bench.write_lock(&text);
        let ran = bench.run(&[]);
        installed_nothing(&bench, &ran);
        assert!(ran.stderr.contains("checksum"), "{text:?}: {}", ran.stderr);
        assert_eq!(bench.calls_of("curl"), Vec::<String>::new(), "{text:?}");
    }
    let bench = Bench::new();
    let lock = bench.path("plugin/packaging/librepods.sha256");
    assert!(lock.is_absolute() && lock.starts_with(bench.root()));
    fs::remove_file(&lock).unwrap();
    let ran = bench.run(&[]);
    installed_nothing(&bench, &ran);
    assert!(ran.stderr.contains("librepods.sha256"), "{}", ran.stderr);
}

#[test]
fn curl_is_asked_for_https_only_with_a_time_bound() {
    let bench = Bench::new();
    succeeded(&bench.run(&[]));
    let curls = bench.arguments_of("curl");
    assert_eq!(curls.len(), 1, "{curls:?}");
    let words = &curls[0];
    let timeout = script_constant("download_timeout_s");
    assert!(timeout.parse::<u32>().unwrap() > 0);
    let url = format!(
        "{}/librepods-{VERSION}/{ASSET}",
        script_constant("release_url")
    );
    assert!(url.starts_with("https://"));
    let output = words
        .get(CURL_OUTPUT_AT)
        .unwrap_or_else(|| panic!("curl was given no output path: {words:?}"));
    let wanted = [
        "curl",
        "--fail",
        "--location",
        "--silent",
        "--show-error",
        "--proto",
        "=https",
        "--tlsv1.2",
        "--max-time",
        &timeout,
        "--output",
        output,
        &url,
    ];
    assert_eq!(*words, wanted);
    let output = Path::new(output);
    assert_eq!(
        output.parent().unwrap().parent(),
        Some(bench.path("runtime").as_path())
    );
    assert_eq!(output.file_name().unwrap(), ASSET);
}

#[test]
fn the_service_is_restarted_and_not_only_enabled() {
    let bench = Bench::new();
    succeeded(&bench.run(&[]));
    let wanted = [
        "systemctl --user daemon-reload",
        "systemctl --user enable librepods.service",
        RESTART,
    ];
    assert_eq!(bench.calls_of("systemctl"), wanted);
    let calls = bench.calls();
    let restart = calls.iter().position(|call| call == RESTART).unwrap();
    let status = calls
        .iter()
        .position(|call| call == "librepods status")
        .unwrap();
    assert!(status > restart, "{calls:?}");
}

#[test]
fn the_production_service_wait_is_ten_seconds() {
    assert_eq!(script_constant("service_wait_s"), "10");
}

#[test]
#[expect(
    clippy::disallowed_types,
    reason = "the installer clock needs an independent wall clock for comparison"
)]
fn the_production_service_clock_reports_epoch_microseconds() {
    let script = fs::read_to_string(repo().join("packaging/setup")).expect("read packaging/setup");
    // Sample input: `now_us() {\n    printf '%s' "${EPOCHREALTIME//[!0-9]/}"\n}\n`.
    let (_, function) = script
        .split_once("\nnow_us() {\n")
        .expect("packaging/setup defines now_us");
    let (body, _) = function
        .split_once("\n}\n")
        .expect("packaging/setup closes the now_us function");
    let mut bash = std::process::Command::new("/bin/bash");
    bash.env_clear()
        .args(["-euc", &format!("now_us() {{\n{body}\n}}\nnow_us")]);
    let wall_time_us = || {
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock follows the Unix epoch")
            .as_micros()
    };
    let before = wall_time_us();
    let output = bash.output().expect("run packaging/setup now_us");
    let after = wall_time_us();

    assert!(
        output.status.success(),
        "packaging/setup now_us failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = std::str::from_utf8(&output.stdout).expect("now_us prints UTF-8");
    // Sample input: `1763880000123456`.
    let clock_us = stdout
        .trim()
        .parse::<u128>()
        .unwrap_or_else(|_| panic!("packaging/setup now_us printed {stdout:?}"));
    // Both clocks read the realtime clock, so no slack: a seconds value padded to microseconds lands before `before`.
    let expected = before..=after;
    assert!(
        expected.contains(&clock_us),
        "packaging/setup now_us printed {clock_us}, outside {expected:?} microseconds since the Unix epoch"
    );
}

#[test]
#[expect(clippy::print_stderr, reason = "a timing test prints how long it took")]
fn a_service_that_never_answers_fails_loud_inside_the_bound() {
    let wait_s: u64 = script_constant("service_wait_s").parse().unwrap();
    let wait = Duration::from_secs(wait_s);
    let runs: Vec<_> = (0..SILENT_RUNS)
        .map(|_| {
            thread::spawn(|| {
                let bench = Bench::new();
                bench.set("silent", "");
                bench.use_service_clock();
                let ran = bench.run(&[]);
                (
                    ran,
                    bench.calls_of("librepods"),
                    bench.runtime_entries(),
                    bench.service_elapsed(),
                )
            })
        })
        .collect();
    let ended: Vec<_> = runs.into_iter().map(thread::JoinHandle::join).collect();
    let mut took = Vec::new();
    for run in ended {
        let (ran, asked, left, elapsed) = run.unwrap();
        assert_eq!(ran.code, Some(1), "{}", ran.stderr);
        assert_eq!(ran.stderr, NOT_STARTED);
        assert_eq!(ran.stdout, "");
        assert_eq!(left, Vec::<PathBuf>::new());
        assert_eq!(asked.len(), usize::try_from(wait_s).unwrap(), "{asked:?}");
        assert!(
            asked.iter().all(|call| call == "librepods status"),
            "{asked:?}"
        );
        assert!(elapsed >= wait, "ended after {elapsed:?}, before its wait");
        assert!(elapsed <= wait + WAIT_SLACK, "ended after {elapsed:?}");
        assert!(ran.took < WAIT_SLACK, "the installer took {:?}", ran.took);
        took.push(ran.took);
    }
    took.sort();
    eprintln!(
        "a daemon that never answers fails after {:.2} s to {:.2} s wall time over {SILENT_RUNS} runs with the test clock",
        took[0].as_secs_f64(),
        took[SILENT_RUNS - 1].as_secs_f64()
    );
}

#[test]
fn a_service_that_answers_slowly_fails_inside_the_same_bound() {
    let wait = Duration::from_secs(script_constant("service_wait_s").parse().unwrap());
    let bench = Bench::new();
    bench.set("slow", "");
    bench.use_service_clock();
    let ran = bench.run(&[]);
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    assert_eq!(ran.stderr, NOT_STARTED);
    let elapsed = bench.service_elapsed();
    assert!(elapsed >= wait, "ended after {elapsed:?}, before its wait");
    assert!(elapsed <= wait + WAIT_SLACK, "ended after {elapsed:?}");
    assert!(ran.took < WAIT_SLACK, "the installer took {:?}", ran.took);
}

#[test]
fn running_as_root_is_refused() {
    let bench = Bench::new();
    bench.set("uid", "0\n");
    let ran = bench.run(&[]);
    installed_nothing(&bench, &ran);
    assert!(ran.stderr.contains("not as root"), "{}", ran.stderr);
    assert_eq!(bench.calls_of("id"), ["id -u"]);
    assert_eq!(bench.calls_of("curl"), Vec::<String>::new());
}

#[test]
fn another_machine_type_is_sent_to_from_source() {
    let bench = Bench::new();
    bench.set("machine", "aarch64\n");
    let ran = bench.run(&[]);
    installed_nothing(&bench, &ran);
    assert!(ran.stderr.contains("x86-64"), "{}", ran.stderr);
    assert!(ran.stderr.contains("--from-source"), "{}", ran.stderr);
    assert!(ran.stderr.contains("aarch64"), "{}", ran.stderr);
    assert_eq!(bench.calls_of("curl"), Vec::<String>::new());
}

#[test]
fn from_source_builds_with_cargo_and_downloads_nothing() {
    let bench = Bench::new();
    bench.add_cargo();
    bench.set("machine", "aarch64\n");
    bench.write_lock("");
    succeeded(&bench.run(&["--from-source"]));
    let target = bench.path("plugin/target").display().to_string();
    let plugin = bench.path("plugin").display().to_string();
    let build = [
        "cargo",
        "build",
        "--release",
        "--locked",
        "-p",
        "librepods",
        "--target-dir",
        &target,
        "in",
        &plugin,
    ];
    assert_eq!(bench.arguments_of("cargo"), [build]);
    assert_eq!(bench.calls_of("curl"), Vec::<String>::new());
    assert_eq!(
        bench.home_tree().get(Path::new(BINARY)),
        Some(&Node::File(0o755, bench.payload()))
    );
}

#[test]
fn from_source_without_cargo_says_how_to_get_it() {
    let bench = Bench::new();
    let ran = bench.run(&["--from-source"]);
    installed_nothing(&bench, &ran);
    assert!(
        ran.stderr.contains("omarchy install dev-env rust"),
        "{}",
        ran.stderr
    );
    assert_eq!(bench.calls_of("curl"), Vec::<String>::new());
}

#[test]
fn a_failed_build_installs_nothing_and_names_the_status() {
    let bench = Bench::new();
    bench.add_cargo();
    bench.set("cargo-fails", "");
    let ran = bench.run(&["--from-source"]);
    installed_nothing(&bench, &ran);
    assert!(ran.stderr.contains("could not compile"), "{}", ran.stderr);
    assert!(ran.stderr.contains(CARGO_FAILED), "{}", ran.stderr);
}

#[test]
fn a_systemctl_step_that_fails_stops_the_install() {
    let steps = [
        ("daemon-reload", "could not reload the user units"),
        ("enable", "could not enable the service"),
        ("restart", "could not restart the service"),
    ];
    for (verb, said) in steps {
        let bench = Bench::new();
        bench.set("systemctl-fails", verb);
        let ran = bench.run(&[]);
        assert_eq!(ran.code, Some(1), "{verb}: {}", ran.stderr);
        assert_eq!(ran.stdout, "", "{verb}");
        assert!(
            ran.stderr.contains("Failed to connect to bus"),
            "{verb}: {}",
            ran.stderr
        );
        assert!(ran.stderr.contains(said), "{verb}: {}", ran.stderr);
        let last = bench.arguments_of("systemctl").pop().unwrap();
        assert_eq!(last.get(2).map(String::as_str), Some(verb), "{last:?}");
        assert_eq!(bench.calls_of("librepods"), Vec::<String>::new(), "{verb}");
        assert_eq!(bench.runtime_entries(), Vec::<PathBuf>::new(), "{verb}");
    }
}

#[test]
fn a_second_run_changes_nothing_but_the_restart() {
    let bench = Bench::new();
    succeeded(&bench.run(&[]));
    let first = bench.home_tree();
    bench.clear_calls();
    succeeded(&bench.run(&[]));
    assert_eq!(bench.home_tree(), first);
    assert!(
        bench
            .calls_of("systemctl")
            .iter()
            .any(|call| call == RESTART)
    );
}

#[test]
fn an_unknown_argument_prints_the_usage() {
    let cases: [&[&str]; 6] = [
        &["--no-experimental"],
        &["--bogus"],
        &["--from-source", "extra"],
        &["--uninstall", "--from-source"],
        &["-h"],
        &[""],
    ];
    for args in cases {
        let bench = Bench::new();
        let ran = bench.run(args);
        assert_eq!(ran.code, Some(USAGE_EXIT), "{args:?}: {}", ran.stderr);
        assert_eq!(ran.stdout, "", "{args:?}");
        assert_eq!(
            ran.stderr.lines().count(),
            USAGE_LINES,
            "{args:?}: {}",
            ran.stderr
        );
        assert!(ran.stderr.starts_with("usage: setup"), "{}", ran.stderr);
        assert_eq!(bench.calls(), Vec::<String>::new(), "{args:?}");
        assert_eq!(bench.home_tree(), Tree::new(), "{args:?}");
    }
}

#[test]
fn no_call_names_a_path_outside_the_bench() {
    let download = Bench::new();
    succeeded(&download.run(&[]));
    let built = Bench::new();
    built.add_cargo();
    succeeded(&built.run(&["--from-source"]));
    let release = format!("{}/", script_constant("release_url"));
    for bench in [download, built] {
        let calls = bench.call_arguments();
        assert_ne!(calls, Vec::<Vec<String>>::new());
        for call in &calls {
            for word in call {
                let path = Path::new(word);
                let climbs = path.components().any(|part| part == Component::ParentDir);
                assert!(!climbs, "{call:?}");
                let object_path = call[0] == "busctl" && word == ADAPTER_OBJECT;
                if word.starts_with('/') && !object_path {
                    assert!(path.starts_with(bench.root()), "{call:?}");
                }
                if word.contains("://") {
                    assert!(word.starts_with(&release), "{call:?}");
                }
            }
        }
    }
}

#[test]
fn the_temporary_directory_is_gone_after_success_and_after_failure() {
    let flags = [
        None,
        Some(("served", "not the published file\n")),
        Some(("served", "")),
        Some(("curl-fails", "")),
    ];
    for flag in flags {
        let bench = Bench::new();
        if let Some((name, body)) = flag {
            bench.set(name, body);
        }
        let ran = bench.run(&[]);
        assert_eq!(
            ran.code == Some(0),
            flag.is_none(),
            "{flag:?}: {}",
            ran.stderr
        );
        let curls = bench.calls_of("curl");
        assert_eq!(curls.len(), 1, "{flag:?}");
        assert!(curls[0].contains(&format!(
            "{}/omapods-setup.",
            bench.path("runtime").display()
        )));
        assert_eq!(bench.runtime_entries(), Vec::<PathBuf>::new(), "{flag:?}");
    }
}

/// The arguments of one run, what the case arranges before it, and whether the run installs.
type Arm = (&'static [&'static str], fn(&Bench), bool);

/// Every install arm, each held to its own outcome: the download, the build, and each refusal.
fn every_arm() -> Vec<Bench> {
    let arms: [Arm; 10] = [
        (&[], |_| {}, true),
        (&["--from-source"], Bench::add_cargo, true),
        (&["--from-source"], |_| {}, false),
        (
            &["--from-source"],
            |bench| {
                bench.add_cargo();
                bench.set("cargo-fails", "");
            },
            false,
        ),
        (&[], |bench| bench.set("uid", "0\n"), false),
        (&[], |bench| bench.set("machine", "aarch64\n"), false),
        (
            &[],
            |bench| bench.set("served", "not the published file\n"),
            false,
        ),
        (&[], |bench| bench.set("served", ""), false),
        (&[], |bench| bench.set("curl-fails", ""), false),
        (&[], |bench| bench.write_lock(""), false),
    ];
    let mut benches = Vec::new();
    for (args, arrange, installs) in arms {
        let bench = Bench::new();
        arrange(&bench);
        let ran = bench.run(args);
        if installs {
            succeeded(&ran);
        } else {
            installed_nothing(&bench, &ran);
        }
        benches.push(bench);
    }
    benches
}

#[test]
fn the_installer_never_asks_for_privilege() {
    let probe = Bench::new();
    let refused = std::process::Command::new(probe.path("bin/sudo"))
        .arg("true")
        .status()
        .unwrap();
    assert!(!refused.success());
    assert_eq!(probe.calls_of("sudo"), ["sudo true"]);
    for bench in every_arm() {
        let calls = bench.calls();
        assert!(calls.iter().any(|call| call == "id -u"), "{calls:?}");
        assert_eq!(bench.calls_of("sudo"), Vec::<String>::new(), "{calls:?}");
    }
}

#[test]
fn the_installer_names_no_privilege_tool() {
    let script = fs::read_to_string(repo().join("packaging/setup"))
        .unwrap()
        .to_lowercase();
    for word in PRIVILEGE_WORDS {
        assert!(!script.contains(word), "packaging/setup names {word}");
    }
}

#[test]
fn nothing_outside_the_home_changes() {
    let download = Bench::new();
    let before = download.outside_home(&[]);
    succeeded(&download.run(&[]));
    assert_eq!(download.outside_home(&[]), before);
    let built = Bench::new();
    built.add_cargo();
    let target = ["plugin/target"];
    let before = built.outside_home(&target);
    succeeded(&built.run(&["--from-source"]));
    assert_eq!(built.outside_home(&target), before);
}
