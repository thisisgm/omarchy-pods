//! The attended checks: the command census and the adapter cycle, against the stub socket.

use std::time::{Duration, Instant};

use pods_proto::model::Model;

use super::bench::{Bench, assert_restarted_after_the_run, text};

#[test]
fn the_command_census_sends_shows_reads_back_and_finds_no_bud_worn() {
    let bench = Bench::new();
    let (output, sets) = bench.run_attended();
    let stdout = text(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{stdout}{}",
        text(&output.stderr)
    );
    for row in [
        "noise transparency",
        "conversational_awareness false",
        "adaptive_level 40",
    ] {
        assert!(
            stdout.contains(&format!("set {row}: sent 10 of 10, shown at once 10 of 10")),
            "{stdout}"
        );
    }
    assert!(stdout.contains("settings put back: true"), "{stdout}");
    assert!(stdout.contains("answered not_worn"), "{stdout}");
    assert!(stdout.contains("gate G4c:"), "{stdout}");
    assert!(
        sets.before_prompt.ends_with(&[
            "noise adaptive".to_owned(),
            "adaptive_level 50".to_owned(),
            "noise off".to_owned(),
            "conversational_awareness false".to_owned(),
            "one_bud_anc false".to_owned(),
        ]),
        "{:?}",
        sets.before_prompt
    );
    assert_eq!(sets.after_prompt, ["noise anc"]);
    assert_eq!(bench.read("powered").trim(), "true");
}

#[test]
fn a_command_short_of_sent_or_a_worn_bud_fails_the_census() {
    let bench = Bench::new();
    bench.set("weak-census", "");
    let (output, sets) = bench.run_attended();
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("C6 commands: FAILED"), "{stdout}");
    assert!(
        stdout.contains("set noise anc was not sent 10 of 10"),
        "{stdout}"
    );
    assert!(
        stdout.contains("expected not_worn with no bud in"),
        "{stdout}"
    );
    assert_eq!(sets.after_prompt, ["noise anc", "noise off"]);
    assert!(
        stdout.contains("noise put back: false") && stdout.contains("could not put back noise"),
        "{stdout}"
    );
    assert!(bench.installed_is("active"));
}

#[test]
fn a_setting_put_back_that_does_not_read_back_fails_the_census() {
    let bench = Bench::new();
    bench.set("status-misses-noise-off", "");
    let (output, _) = bench.run_attended();
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("settings put back: false"), "{stdout}");
    assert!(stdout.contains("noise read back changed"), "{stdout}");
}

#[test]
fn a_stop_signal_after_a_half_typed_line_still_restores() {
    let bench = Bench::new();
    bench.set("half-line-at-prompt", "");
    let (output, sets) = bench.run_attended();
    let stdout = text(&output.stdout);
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stdout}{stderr}");
    assert!(!stdout.contains("C6 commands:"), "{stdout}");
    assert_eq!(sets.after_prompt, Vec::<String>::new());
    assert!(bench.installed_is("active"));
}

#[test]
fn a_setting_left_changed_is_reported_when_a_signal_ends_the_census() {
    let bench = Bench::new();
    bench.set("refuse-one-bud-off", "");
    bench.set("interrupt-at-prompt", "");
    let (output, sets) = bench.run_attended();
    let stdout = text(&output.stdout);
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stdout}{stderr}");
    assert!(
        stderr.contains("C6: could not put back one_bud_anc"),
        "{stderr}"
    );
    assert!(!stdout.contains("C6 commands:"), "{stdout}");
    assert_eq!(sets.after_prompt, Vec::<String>::new());
    assert!(bench.installed_is("active"));
}

#[test]
fn the_adapter_is_powered_back_on_when_the_cycle_fails() {
    let bench = Bench::new();
    bench.set("power-on-fails", "");
    let (output, _) = bench.run_attended();
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("C7 adapter: FAILED") && stdout.contains("adapter powered back on: true"),
        "{stdout}"
    );
    assert_eq!(bench.read("powered").trim(), "true");
    let powers: Vec<String> = bench
        .calls()
        .into_iter()
        .filter(|call| call.starts_with("bluetoothctl"))
        .collect();
    assert_eq!(
        powers,
        [
            "bluetoothctl power off",
            "bluetoothctl power on",
            "bluetoothctl power on"
        ]
    );
}

fn bluetoothctl_calls(bench: &Bench) -> Vec<String> {
    bench
        .calls()
        .into_iter()
        .filter(|call| call.starts_with("bluetoothctl"))
        .collect()
}

#[test]
fn an_unattended_run_leaves_the_adapter_and_the_settings_alone() {
    let bench = Bench::new();
    bench.set("unattended", "");
    let (output, sets) = bench.run_attended();
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(
        !stdout.contains("C6 commands") && !stdout.contains("C7 adapter"),
        "{stdout}"
    );
    assert_eq!(sets.before_prompt, Vec::<String>::new());
    assert_eq!(sets.after_prompt, Vec::<String>::new());
    assert_eq!(bluetoothctl_calls(&bench), Vec::<String>::new());
}

#[test]
fn an_attended_run_without_the_cycle_leaves_the_adapter_alone() {
    let bench = Bench::new();
    bench.set("attended-only", "");
    let (output, _) = bench.run_attended();
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.contains("C6 commands") && !stdout.contains("C7 adapter"),
        "{stdout}"
    );
    assert_eq!(bluetoothctl_calls(&bench), Vec::<String>::new());
}

#[test]
fn a_link_already_down_fails_the_adapter_cycle() {
    let bench = Bench::new();
    bench.set("link-down-after-prompt", "");
    let (output, _) = bench.run_attended();
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("C7 adapter: FAILED")
            && stdout.contains("not connected before the adapter went off"),
        "{stdout}"
    );
}

#[test]
fn a_stop_signal_during_the_adapter_cycle_powers_the_adapter_back_on() {
    let bench = Bench::new();
    bench.set("interrupt-at-power-off", "");
    let (output, _) = bench.run_attended();
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("stopped by a signal"), "{stderr}");
    assert!(
        !text(&output.stdout).contains("C7 adapter"),
        "C7 ran to its end"
    );
    assert_eq!(bench.read("powered").trim(), "true");
    assert_eq!(
        bluetoothctl_calls(&bench),
        ["bluetoothctl power off", "bluetoothctl power on"]
    );
    assert!(bench.installed_is("active"));
}

#[test]
fn an_adapter_left_off_is_reported_when_a_signal_ends_the_cycle() {
    let bench = Bench::new();
    bench.set("interrupt-at-power-off", "");
    bench.set("power-on-fails", "");
    let (output, _) = bench.run_attended();
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("C7: bluetoothctl power on exited with")
            && stderr.contains("stopped by a signal"),
        "{stderr}"
    );
    assert!(
        !text(&output.stdout).contains("C7 adapter"),
        "C7 ran to its end"
    );
    assert!(bench.installed_is("active"));
}

#[test]
fn a_model_without_the_census_features_fails_c6_and_sets_nothing() {
    let bench = Bench::new();
    bench.set("attended-only", "");
    bench.set_model(Model::Unknown);
    let (output, sets) = bench.run_attended();
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("C6 commands: FAILED"), "{stdout}");
    assert!(
        stdout.contains(
            "  the AirPods lack supports_adaptive, supports_conversational_awareness, supports_one_bud_anc, so nothing was set"
        ),
        "{stdout}"
    );
    assert!(sets.before_prompt.is_empty(), "{:?}", sets.before_prompt);
    assert!(sets.after_prompt.is_empty(), "{:?}", sets.after_prompt);
    assert!(!text(&output.stderr).contains("take both buds out"));
}

/// A run whose daemon never answers waits out one 10 s census bound and one 10 s restore bound.
const SILENT_RUN_AT_LEAST: Duration = Duration::from_secs(20);
/// The two bounds and the other checks' few seconds.
const SILENT_RUN_AT_MOST: Duration = Duration::from_secs(40);

#[test]
fn a_daemon_that_never_answers_ends_c6_within_two_bounds_and_the_run_restores() {
    let bench = Bench::new();
    bench.set("attended-only", "");
    bench.set("census-silent", "");
    let began = Instant::now();
    let (output, _) = bench.run_attended();
    let took = began.elapsed();
    let stdout = text(&output.stdout);
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stdout}{stderr}");
    assert!(
        stdout.contains(
            "  set noise transparency: no reply within 10000 ms; the census stopped there"
        ),
        "{stdout}"
    );
    assert!(
        stderr.contains("C6: could not put back noise, adaptive_level, noise, conversational_awareness, one_bud_anc: no reply within 10000 ms"),
        "{stderr}"
    );
    assert!(!stderr.contains("take both buds out"), "{stderr}");
    assert!(took >= SILENT_RUN_AT_LEAST, "{took:?}");
    assert!(took < SILENT_RUN_AT_MOST, "{took:?}");
    assert_restarted_after_the_run(&bench);
}
