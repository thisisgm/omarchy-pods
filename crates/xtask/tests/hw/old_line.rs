//! C9 against the stand-in installed daemon: its line read before the stop, compared key by key with the line of the daemon under test.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use pods_core::state::State;
use pods_core::status::{ADDITIVE_KEYS, to_status_line};
use pods_proto::model::Model;
use serde_json::Value;

use super::bench::{Bench, assert_restarted_after_the_run, text};

/// A synthetic name holding the mark a real name carries, U+2019.
const MARKED_NAME: &str = "Synthetic\u{2019}s Pods";
const OLD_NAME: &str = "Synthetic Old Pods";
const NEW_NAME: &str = "Synthetic New Pods";
const OLD_MODEL_NUMBER: &str = "A0001";
const NEW_MODEL_NUMBER: &str = "A0002";

/// Copies what the run keeps of both lines at its first `do disconnect`, while the run directory is still there.
const KEEPING_BINARY: &str = r#"#!/bin/sh
if [ "$1 $2" = "do disconnect" ] && [ ! -e "$STUB/kept-modes" ]; then
    run=$(dirname "$XDG_RUNTIME_DIR")
    for name in old-line.json new-line.json; do
        stat -c %a "$run/$name" >> "$STUB/kept-modes"
        cp "$run/$name" "$STUB/kept-$name"
    done
fi
exec "$STUB/bin/pods-under-test" "$@"
"#;

/// The line of the daemon under test for pods named `name`.
fn new_line(name: &str, model_number: &str) -> String {
    to_status_line(&State {
        connected: true,
        device_name: name.to_owned(),
        model: Model::AirPodsPro2USBC,
        model_number: model_number.to_owned(),
        ..State::default()
    })
}

/// The same line as the old daemon writes it, without the keys this daemon adds.
fn old_line(name: &str, model_number: &str) -> String {
    let mut old: Value = serde_json::from_str(&new_line(name, model_number)).unwrap();
    for key in ADDITIVE_KEYS {
        old.as_object_mut().unwrap().remove(*key);
    }
    format!("{old}\n")
}

/// A bench whose daemon under test prints `new` and whose installed daemon prints `old`.
fn bench_with(old: &str, new: &str) -> Bench {
    let bench = Bench::new();
    bench.set("status-true.json", new);
    bench.set("installed-status.json", old);
    bench
}

#[test]
fn c9_passes_on_equal_lines() {
    let bench = bench_with(&old_line(MARKED_NAME, ""), &new_line(MARKED_NAME, ""));
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("C9 old line: passed\n"), "{stdout}");
    assert!(stdout.contains("\n  device_name: equal\n"), "{stdout}");
    assert!(stdout.contains("\n  connected: equal\n"), "{stdout}");
    for key in ADDITIVE_KEYS {
        assert!(stdout.contains(&format!("\n  {key}: only new")), "{stdout}");
    }
    let lengths = format!(
        "old line {} bytes, new line {} bytes",
        old_line(MARKED_NAME, "").trim_end().len(),
        new_line(MARKED_NAME, "").trim_end().len()
    );
    assert!(stdout.contains(&lengths), "{stdout}");
    assert_restarted_after_the_run(&bench);
}

#[test]
fn c9_fails_and_names_the_key_when_the_name_differs() {
    let escaped = old_line(MARKED_NAME, "").replace('\u{2019}', "\\u2019");
    let bench = bench_with(&escaped, &new_line(MARKED_NAME, ""));
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("C9 old line: FAILED\n"), "{stdout}");
    assert!(stdout.contains("\n  device_name: differs\n"), "{stdout}");
    assert!(stdout.contains("\n  model_name: equal\n"), "{stdout}");
    assert_restarted_after_the_run(&bench);
}

#[test]
fn c9_prints_no_value() {
    let old = old_line(OLD_NAME, OLD_MODEL_NUMBER);
    let new = new_line(NEW_NAME, NEW_MODEL_NUMBER);
    let bench = bench_with(&old, &new);
    fs::rename(bench.path("bin/pods"), bench.path("bin/pods-under-test")).unwrap();
    bench.set("bin/pods", KEEPING_BINARY);
    fs::set_permissions(bench.path("bin/pods"), fs::Permissions::from_mode(0o755)).unwrap();
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    let printed = format!("{stdout}{}", text(&output.stderr));
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("\n  device_name: differs\n"), "{stdout}");
    assert!(stdout.contains("\n  model_number: differs\n"), "{stdout}");
    for value in [OLD_NAME, NEW_NAME, OLD_MODEL_NUMBER, NEW_MODEL_NUMBER] {
        assert!(!printed.contains(value), "{value} reached the output");
    }
    assert_eq!(bench.read("kept-modes"), "600\n600\n");
    assert_eq!(bench.read("kept-old-line.json"), old.trim_end());
    assert_eq!(bench.read("kept-new-line.json"), new.trim_end());
    assert_restarted_after_the_run(&bench);
}

#[test]
fn c9_is_skipped_by_name_when_the_installed_daemon_is_not_the_old_one() {
    let bench = Bench::new();
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    let added = ADDITIVE_KEYS.len();
    let all_added = format!(
        "C9 old line: skipped\n  the installed daemon is not the old one: its line carries {added} of the {added} keys this daemon adds\n"
    );
    assert!(stdout.contains(&all_added), "{stdout}");
    assert_restarted_after_the_run(&bench);
    let old = old_line(MARKED_NAME, "");
    let one_added = format!(
        "{{\"{}\":0,{}",
        ADDITIVE_KEYS[0],
        old.strip_prefix('{').unwrap()
    );
    let bench = bench_with(&one_added, &new_line(MARKED_NAME, ""));
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.contains(&format!("C9 old line: skipped\n  the installed daemon is not the old one: its line carries 1 of the {added} keys this daemon adds\n")),
        "{stdout}"
    );
    let bench = Bench::new();
    bench.set("unit-podsd.service", "inactive");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.contains(
            "C9 old line: skipped\n  the installed daemon was not running, so it showed no line\n"
        ),
        "{stdout}"
    );
    assert!(bench.installed_is("inactive"));
}

#[test]
fn c9_fails_when_the_old_line_shows_no_name() {
    let bench = bench_with(&old_line("", ""), &new_line(MARKED_NAME, ""));
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("\n  the installed daemon showed no name\n"),
        "{stdout}"
    );
    assert_restarted_after_the_run(&bench);
}

#[test]
fn c9_fails_when_the_installed_line_is_not_a_state_object() {
    let bench = bench_with("not a line\n", &new_line(MARKED_NAME, ""));
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains(
            "C9 old line: FAILED\n  the installed pods-ctl status printed no state object\n"
        ),
        "{stdout}"
    );
    assert_restarted_after_the_run(&bench);
}

#[test]
fn c9_fails_when_the_installed_ctl_exits_with_an_error() {
    let bench = bench_with(&old_line(MARKED_NAME, ""), &new_line(MARKED_NAME, ""));
    bench.set("installed-ctl-fails", "");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains(
            "C9 old line: FAILED\n  the installed pods-ctl status exited with exit status: 3\n"
        ),
        "{stdout}"
    );
    assert_restarted_after_the_run(&bench);
}
