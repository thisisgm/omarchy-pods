//! The checks against a scripted link, and what the run must never do or print.

use std::path::Path;

use super::bench::{
    ADDRESS, Bench, DEVICE_NAME, DEVICE_PATH, assert_restarted_after_the_run, interrupted_by,
    signalled, text,
};

#[test]
fn the_installed_service_is_restored_when_a_check_fails() {
    let bench = Bench::new();
    bench.set("ctl-differs", "");
    let output = bench.run(&[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stdout).contains("C3 same contract: FAILED"));
    assert_restarted_after_the_run(&bench);
}

#[test]
fn a_signal_during_a_check_ends_its_watch_and_restores() {
    let bench = Bench::new();
    interrupted_by(&bench, "-INT", "librepods watch");
    let watch = bench.read("watch-pid");
    assert!(
        !Path::new(&format!("/proc/{}", watch.trim())).exists(),
        "the watch outlived the run"
    );
}

#[test]
fn a_hanging_tool_is_killed_and_the_run_goes_on() {
    let bench = Bench::new();
    bench.set("hang-pactl", "");
    let output = bench.run(&[]);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        text(&output.stdout)
            .contains("audio card in a headset profile: unknown (pactl list did not finish")
    );
    assert_eq!(bench.left_running(), Vec::<i32>::new());
    assert_restarted_after_the_run(&bench);
}

#[test]
fn a_failing_journal_read_is_reported_rather_than_waited_out() {
    let bench = Bench::new();
    bench.set("journal-fails", "");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("did not start again to a populated state: journalctl exited with"),
        "{stdout}"
    );
    assert!(
        stdout.contains("C1 adoption: FAILED\n  journalctl exited with"),
        "{stdout}"
    );
    assert!(
        stdout.contains("round 1: journalctl exited with"),
        "{stdout}"
    );
    assert!(!stdout.contains("not populated"), "{stdout}");
    assert_eq!(bench.left_running(), Vec::<i32>::new());
    assert_restarted_after_the_run(&bench);
}

#[test]
fn a_failing_main_process_read_names_systemctl() {
    let bench = Bench::new();
    bench.set("main-pid-fails", "");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("systemctl show librepods-hw.service exited with"),
        "{stdout}"
    );
    assert_restarted_after_the_run(&bench);
}

#[test]
fn a_populated_line_stamped_before_its_opening_fails_c4() {
    let bench = Bench::new();
    bench.set("populated-first", "");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("round 1: the populated line is stamped before the opening line"),
        "{stdout}"
    );
    assert!(stdout.contains("gate G1b: no samples"), "{stdout}");
    assert_restarted_after_the_run(&bench);
}

#[test]
fn a_round_with_no_opening_line_fails_c4() {
    let bench = Bench::new();
    bench.set("one-opening-missing", "");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("C4 reconnect: FAILED"), "{stdout}");
    assert!(
        stdout.contains("round 1: no link: idle to opening line before populated"),
        "{stdout}"
    );
    assert_restarted_after_the_run(&bench);
}

#[test]
fn a_link_down_before_the_stop_is_not_blamed_on_the_stop() {
    let bench = Bench::new();
    bench.set("link-drops", "");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("BlueZ did not report the device connected before the stop"),
        "{stdout}"
    );
    assert!(!stdout.contains("BlueZ dropped the device"), "{stdout}");
    assert_restarted_after_the_run(&bench);
}

#[test]
fn a_failed_activity_query_after_the_stop_is_named() {
    let bench = Bench::new();
    bench.set("hw-activity-fails", "");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("systemctl is-active librepods-hw.service gave no state (exit status: 1)"),
        "{stdout}"
    );
    assert!(!stdout.contains("was not inactive within"), "{stdout}");
    assert_restarted_after_the_run(&bench);
}

#[test]
fn the_watch_handshake_is_not_counted_as_idle_wakeups() {
    let bench = Bench::new();
    bench.set("watch-wakes-daemon", "");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("C5 idle: passed"), "{stdout}");
}

#[test]
fn a_link_down_before_the_idle_check_is_not_measured_as_link_up() {
    let bench = Bench::new();
    bench.set("link-down-at-idle", "");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("the link was not up for the link up window"),
        "{stdout}"
    );
    assert!(!stdout.contains("G3 link up:"), "{stdout}");
    assert_restarted_after_the_run(&bench);
}

#[test]
fn a_signal_between_disconnect_and_connect_leaves_the_airpods_connected() {
    let bench = Bench::new();
    bench.set("slow-disconnect", "");
    let output = signalled(&bench, "-INT", "librepods do disconnect");
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("stopped by a signal"));
    let calls = bench.calls();
    let disconnected = calls
        .iter()
        .rposition(|call| call == "librepods do disconnect")
        .unwrap();
    assert!(
        calls
            .iter()
            .skip(disconnected)
            .any(|call| call == "librepods do connect"),
        "{calls:?}"
    );
    assert_restarted_after_the_run(&bench);
}

#[test]
fn a_signal_before_the_disconnect_shows_still_leaves_the_airpods_connected() {
    let bench = Bench::new();
    bench.set("slow-disconnect", "");
    bench.set("disconnect-unseen", "");
    let output = signalled(&bench, "-INT", "librepods do disconnect");
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("stopped by a signal"));
    let calls = bench.calls();
    let disconnected = calls
        .iter()
        .rposition(|call| call == "librepods do disconnect")
        .unwrap();
    assert!(
        calls
            .iter()
            .skip(disconnected)
            .any(|call| call == "librepods do connect"),
        "{calls:?}"
    );
    assert_restarted_after_the_run(&bench);
}

#[test]
fn a_failed_reconnect_after_a_signal_is_reported() {
    let bench = Bench::new();
    bench.set("slow-disconnect", "");
    bench.set("connect-fails", "");
    let output = signalled(&bench, "-INT", "librepods do disconnect");
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(
            "xtask hw: the AirPods were left disconnected: librepods do connect exited with"
        ),
        "{stderr}"
    );
    assert_restarted_after_the_run(&bench);
}

#[test]
fn a_watch_that_never_answers_is_not_measured() {
    let bench = Bench::new();
    bench.set("watch-silent", "");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("link up: librepods watch printed nothing within 5 s"),
        "{stdout}"
    );
    assert!(!stdout.contains("G3 link up:"), "{stdout}");
    assert_eq!(bench.left_running(), Vec::<i32>::new());
}

#[test]
fn forget_is_never_called() {
    let bench = Bench::new();
    let (output, sets) = bench.run_attended();
    assert_eq!(output.status.code(), Some(0));
    for call in bench
        .calls()
        .iter()
        .chain(&sets.before_prompt)
        .chain(&sets.after_prompt)
    {
        let lower = call.to_lowercase();
        for word in ["forget", "remove", "pair", "trust", "untrust", "connect 02"] {
            assert!(!lower.contains(word), "{call}");
        }
    }
    let bluetoothctl: Vec<String> = bench
        .calls()
        .into_iter()
        .filter(|call| call.starts_with("bluetoothctl"))
        .collect();
    assert_eq!(
        bluetoothctl,
        ["bluetoothctl power off", "bluetoothctl power on"]
    );
    let busctl = "busctl --system --json=short call org.bluez / org.freedesktop.DBus.ObjectManager GetManagedObjects";
    for call in bench.calls() {
        assert!(!call.starts_with("busctl") || call == busctl, "{call}");
    }
}

#[test]
fn systemctl_names_only_the_two_units() {
    let bench = Bench::new();
    let (output, _) = bench.run_attended();
    assert_eq!(output.status.code(), Some(0));
    let systemctl: Vec<String> = bench
        .calls()
        .into_iter()
        .filter(|call| call.starts_with("systemctl"))
        .collect();
    assert!(
        systemctl.contains(&"systemctl --user stop librepods.service".to_owned()),
        "{systemctl:?}"
    );
    for call in &systemctl {
        assert!(call.starts_with("systemctl --user "), "{call}");
        for word in call.split(' ').skip(3) {
            assert!(
                word.starts_with("--")
                    || word == "librepods.service"
                    || word == "librepods-hw.service",
                "{call}"
            );
        }
    }
}

#[test]
fn no_card_profile_command_is_ever_run() {
    let bench = Bench::new();
    bench.set("profile", "headset-head-unit");
    let (output, _) = bench.run_attended();
    assert!(text(&output.stdout).contains("audio card in a headset profile: yes"));
    let pactl: Vec<String> = bench
        .calls()
        .into_iter()
        .filter(|call| call.starts_with("pactl"))
        .collect();
    assert_eq!(pactl, ["pactl list cards"]);
}

#[test]
fn output_never_contains_an_address_or_a_name() {
    let bench = Bench::new();
    bench.set("slow-connect", "");
    bench.set("ctl-differs", "");
    let (output, _) = bench.run_attended();
    let all = format!("{}{}", text(&output.stdout), text(&output.stderr));
    assert_eq!(output.status.code(), Some(1), "{all}");
    assert!(
        all.contains(
            "phases:\n    +0 ms link: idle to opening\n    +325 ms link: opening to handshake\n    +650 ms link: state populated\n"
        ),
        "{all}"
    );
    for private in [
        ADDRESS,
        DEVICE_PATH,
        DEVICE_NAME,
        "02_00_00_00_00",
        "02:00:00:00:00",
        "Synthetic",
    ] {
        assert!(!all.contains(private), "{private} in {all}");
    }
}

#[test]
fn timings_are_printed_as_ranges_with_their_count() {
    let bench = Bench::new();
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert!(
        stdout.contains("gate G1a: 110 to 200 ms over 10, bound at most 2000 ms in 10 of 10: met"),
        "{stdout}"
    );
    assert!(
        stdout.contains("gate G1b: 250 to 250 ms over 5, bound at most 500 ms in 4 of 5: met"),
        "{stdout}"
    );
    assert!(
        stdout.contains("gate G4b:")
            && stdout.contains("ms over 5, bound at most 15000 ms in 5 of 5: met"),
        "{stdout}"
    );
}

#[test]
fn a_stop_that_leaves_the_socket_and_state_file_fails() {
    let bench = Bench::new();
    bench.set("stop-leaves-files", "");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("C2 stop: FAILED"), "{stdout}");
    assert!(
        stdout.contains("the socket is still there")
            && stdout.contains("the state file is still there"),
        "{stdout}"
    );
}

#[test]
fn a_daemon_woken_in_the_idle_window_misses_g3() {
    let bench = Bench::new();
    bench.set("daemon-busy", "");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("C5 idle: FAILED"), "{stdout}");
    assert!(
        stdout.contains(
            "G3 link up: 2 wakeups in 1 s, 120.0 a minute, bound at most 12, watch lines 1"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("G3 link up: over 12 wakeups a minute"),
        "{stdout}"
    );
    assert_restarted_after_the_run(&bench);
}

#[test]
fn an_adapter_left_discovering_fails_c5() {
    let bench = Bench::new();
    bench.set("discovering", "");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("C5 idle: FAILED"), "{stdout}");
    assert!(
        stdout.contains("link up: the adapter was discovering at the start"),
        "{stdout}"
    );
    assert!(!stdout.contains("over 12 wakeups"), "{stdout}");
    assert_restarted_after_the_run(&bench);
}

#[test]
fn a_watch_that_ends_inside_the_window_fails_c5() {
    let bench = Bench::new();
    bench.set("watch-ends", "");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("C5 idle: FAILED"), "{stdout}");
    assert!(
        stdout.contains("  link up: librepods watch ended before the window did: "),
        "{stdout}"
    );
    assert!(!stdout.contains("G3 link up:"), "{stdout}");
    assert_eq!(bench.left_running(), Vec::<i32>::new());
}

#[test]
fn a_watch_that_cannot_start_fails_c5() {
    let bench = Bench::new();
    bench.set("watch-unstartable", "");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("C5 idle: FAILED"), "{stdout}");
    assert!(
        stdout.contains("  link up: librepods watch could not start: "),
        "{stdout}"
    );
    assert!(!stdout.contains("G3 link up:"), "{stdout}");
    // The do steps run the same binary straight after the watch, so they fail too.
    assert!(
        stdout.contains("bin/librepods could not start: Permission denied"),
        "{stdout}"
    );
    assert!(
        stdout.contains("  not connected again after the windows: "),
        "{stdout}"
    );
    assert_restarted_after_the_run(&bench);
}

#[test]
fn a_failed_reconnect_after_the_idle_windows_fails_c5() {
    let bench = Bench::new();
    bench.set("final-connect-fails", "");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("C5 idle: FAILED"), "{stdout}");
    assert!(
        stdout
            .contains("  not connected again after the windows: librepods do connect exited with"),
        "{stdout}"
    );
    assert_restarted_after_the_run(&bench);
}

#[test]
fn a_reconnect_that_never_shows_after_the_idle_windows_fails_c5() {
    let bench = Bench::new();
    bench.set("final-connect-unseen", "");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("C5 idle: FAILED"), "{stdout}");
    assert!(
        stdout
            .contains("  not connected again after the windows: still down 15 s after do connect"),
        "{stdout}"
    );
    assert_restarted_after_the_run(&bench);
}
