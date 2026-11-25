//! The restore guard, the exit statuses and the test unit's properties.

use std::fs;
use std::path::Path;
use std::process::Stdio;

use super::bench::{
    Bench, QUICK_CONNECT_MS, RUN_DEADLINE, assert_restarted_after_the_run, finish, index_of,
    interrupted_by, last_line, processes_in, signalled, text, wait_for_call, wait_for_file,
    waiting_bench,
};

#[test]
fn the_installed_service_is_restored_on_success() {
    let bench = Bench::new();
    let output = bench.run(&[]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    assert!(text(&output.stdout).ends_with("result: every check passed\n"));
    assert_restarted_after_the_run(&bench);
    let run_dir = bench.read("run-dir");
    assert!(
        run_dir.starts_with(&bench.path("tmp").display().to_string()),
        "{run_dir}"
    );
    assert!(
        !Path::new(run_dir.trim()).exists(),
        "the run directory is removed"
    );
}

#[test]
fn the_installed_service_is_restored_on_sigint() {
    interrupted_by(&waiting_bench(), "-INT", "librepods status");
}

#[test]
fn the_installed_service_is_restored_on_sigterm_and_sighup() {
    interrupted_by(&waiting_bench(), "-TERM", "librepods status");
    interrupted_by(&waiting_bench(), "-HUP", "librepods status");
}

#[test]
fn a_hanging_status_is_killed_at_every_poll_and_the_service_restored() {
    let bench = Bench::new();
    bench.set("hang-status", "");
    let output = bench.run(&[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("did not answer status: ")
            && text(&output.stderr).contains("status did not finish within 5 s"),
        "{}",
        text(&output.stderr)
    );
    assert_eq!(bench.left_running(), Vec::<i32>::new());
    assert_restarted_after_the_run(&bench);
}

#[test]
fn a_signal_before_the_stop_leaves_the_installed_service_alone() {
    let bench = Bench::new();
    bench.set("hang-show", "");
    let call = "systemctl --user show --property=LoadState --value librepods-hw.service";
    let output = signalled(&bench, "-INT", call);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("stopped by a signal; nothing was stopped"),
        "{}",
        text(&output.stderr)
    );
    assert!(
        !bench
            .calls()
            .iter()
            .any(|call| call == "systemctl --user stop librepods.service"),
        "{:?}",
        bench.calls()
    );
}

#[test]
fn a_signal_during_the_stop_never_starts_the_daemon_under_test() {
    let bench = Bench::new();
    bench.set("hang-stop", "");
    let output = signalled(&bench, "-INT", "systemctl --user stop librepods.service");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("stopped by a signal"),
        "{}",
        text(&output.stderr)
    );
    assert!(
        !bench
            .calls()
            .iter()
            .any(|call| call.starts_with("systemd-run")),
        "{:?}",
        bench.calls()
    );
    let calls = bench.calls();
    let stopped = index_of(&calls, "systemctl --user stop librepods.service").expect("stopped");
    let started = index_of(&calls, "systemctl --user start librepods.service")
        .expect("the installed service was started again");
    assert!(stopped < started, "{calls:?}");
    assert!(bench.installed_is("active"));
}

#[test]
fn a_daemon_that_stops_answering_is_a_failure_not_absent_airpods() {
    let bench = Bench::new();
    bench.set("pods-absent", "");
    bench.set("status-once", "");
    let output = bench.run(&["--connect-wait-ms", QUICK_CONNECT_MS]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("did not answer status: librepods status exited with"),
        "{stderr}"
    );
    assert!(!stderr.contains("nothing was measured"), "{stderr}");
    assert_restarted_after_the_run(&bench);
}

#[test]
fn a_service_that_was_inactive_is_left_inactive() {
    let bench = Bench::new();
    bench.set("unit-librepods.service", "inactive");
    let output = bench.run(&[]);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        !bench
            .calls()
            .iter()
            .any(|call| call == "systemctl --user start librepods.service")
    );
    assert!(bench.installed_is("inactive"));
}

#[test]
fn absent_airpods_exit_77_after_restoring() {
    let bench = Bench::new();
    bench.set("pods-absent", "");
    let output = bench.run(&["--connect-wait-ms", QUICK_CONNECT_MS]);
    assert_eq!(output.status.code(), Some(77));
    assert!(text(&output.stderr).contains("nothing was measured"));
    assert!(
        !bench
            .calls()
            .iter()
            .any(|call| call.starts_with("systemctl --user restart"))
    );
    assert_restarted_after_the_run(&bench);
}

#[test]
fn a_failed_restore_exits_70_and_says_so_last() {
    let bench = Bench::new();
    bench.set("restore-fails", "");
    let output = bench.run(&[]);
    assert_eq!(output.status.code(), Some(70));
    assert!(
        last_line(&output.stderr).contains("RESTORE FAILED"),
        "{}",
        text(&output.stderr)
    );
}

#[test]
fn a_start_that_exits_0_but_does_not_take_exits_70() {
    let bench = Bench::new();
    bench.set("start-ignored", "");
    let output = bench.run(&[]);
    assert_eq!(output.status.code(), Some(70));
    let last = last_line(&output.stderr);
    assert!(
        last.contains("RESTORE FAILED")
            && last.contains("librepods.service is inactive after its start"),
        "{last}"
    );
}

#[test]
fn a_test_unit_that_will_not_stop_keeps_the_installed_service_down_and_exits_70() {
    let bench = Bench::new();
    bench.set("hw-stop-ignored", "");
    let output = bench.run(&[]);
    assert_eq!(output.status.code(), Some(70));
    let stdout = text(&output.stdout);
    assert!(
        stdout.contains("the unit was not inactive within 1000 ms"),
        "{stdout}"
    );
    let last = last_line(&output.stderr);
    assert!(
        last.contains("RESTORE FAILED")
            && last.contains("librepods-hw.service is still active after its stop"),
        "{last}"
    );
    assert!(
        !bench
            .calls()
            .iter()
            .any(|call| call == "systemctl --user start librepods.service"),
        "{:?}",
        bench.calls()
    );
}

#[test]
fn an_existing_test_unit_refuses_the_run() {
    let bench = Bench::new();
    bench.set("hw-exists", "");
    let output = bench.run(&[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("nothing was stopped"));
    assert_eq!(
        bench.calls(),
        ["systemctl --user show --property=LoadState --value librepods-hw.service"]
    );
}

#[test]
fn a_missing_installed_service_refuses_the_run() {
    let bench = Bench::new();
    bench.set("installed-missing", "");
    bench.set("unit-librepods.service", "inactive");
    let output = bench.run(&[]);
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("xtask hw: systemctl is-active librepods.service gave no state (exit status: 4); nothing was stopped"),
        "{stderr}"
    );
    assert_eq!(
        bench.calls(),
        [
            "systemctl --user show --property=LoadState --value librepods-hw.service",
            "systemctl --user is-active librepods.service"
        ]
    );
}

#[test]
fn wrong_usage_exits_2() {
    for wrong in ["--no-such-option", "--adapter-cycle"] {
        let bench = Bench::new();
        let output = bench.run(&[wrong]);
        assert_eq!(output.status.code(), Some(2), "{wrong}");
        assert_eq!(bench.calls(), Vec::<String>::new(), "{wrong}");
    }
}

/// The `[Service]` keys above `UMask` that start and restart the daemon rather than confine it.
const LIFECYCLE_KEYS: [&str; 4] = ["Type", "ExecStart", "Restart", "RestartSec"];
/// Well under the service file's count of hardening directives, so a parse that finds almost none fails.
const FEWEST_DIRECTIVES: usize = 10;

/// The unit the package installs, as the test reads the file itself.
fn packaged_unit() -> String {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/systemd/librepods.service");
    fs::read_to_string(path).unwrap()
}

/// The directives of the packaged unit's `[Service]` section from `UMask` on; sample input `UMask=0077` through `LimitCORE=0`, then `[Install]`.
fn service_directives() -> Vec<String> {
    let text = packaged_unit();
    let service = text
        .split("[Service]")
        .nth(1)
        .unwrap()
        .split("\n[")
        .next()
        .unwrap();
    let (above, rest) = service.split_once("\nUMask=").unwrap();
    for line in above.lines().map(str::trim) {
        let key = line.split_once('=').map_or("", |(key, _)| key);
        assert!(
            line.is_empty() || line.starts_with('#') || LIFECYCLE_KEYS.contains(&key),
            "{line} sits above UMask, where the hardening block is not read"
        );
    }
    format!("UMask={rest}")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_test_unit_copies_the_hardening_of_the_packaged_unit() {
    let bench = Bench::new();
    let output = bench.run(&[]);
    assert_eq!(output.status.code(), Some(0));
    let args: Vec<String> = bench
        .read("systemd-run.args")
        .lines()
        .map(str::to_owned)
        .collect();
    let properties: Vec<&str> = args
        .windows(2)
        .filter(|pair| pair[0] == "-p")
        .map(|pair| pair[1].as_str())
        .collect();
    let exceptions = [
        "PrivateTmp=",
        "StateDirectory=",
        "StateDirectoryMode=",
        "ReadWritePaths=",
    ];
    let directives = service_directives();
    assert!(directives.len() > FEWEST_DIRECTIVES, "{directives:?}");
    for directive in &directives {
        let excepted = exceptions
            .iter()
            .any(|exception| directive.starts_with(exception));
        assert_eq!(
            properties.contains(&directive.as_str()),
            !excepted,
            "{directive}"
        );
    }
    let run_dir = bench.read("run-dir");
    let dir = Path::new(run_dir.trim())
        .parent()
        .unwrap()
        .display()
        .to_string();
    assert!(properties.contains(&format!("ReadWritePaths={dir}").as_str()));
    assert!(properties.contains(&"StartLimitIntervalSec=0"));
    assert!(args.contains(&format!("--setenv=XDG_STATE_HOME={dir}/state")));
    assert!(args.iter().any(|arg| {
        arg.starts_with("--setenv=DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/")
            && arg.ends_with("/bus")
    }));
    assert!(
        args.contains(&"--unit=librepods-hw".to_owned()) && args.contains(&"--collect".to_owned())
    );
    assert_eq!(
        args[args.len() - 2..],
        [
            bench.path("bin/librepods").display().to_string(),
            "--headless".to_owned()
        ]
    );
}

#[test]
fn the_packaged_unit_names_no_directory_of_the_old_daemon() {
    let text = packaged_unit();
    assert!(!text.contains("AirPodsTrayApp"), "{text}");
    assert!(!text.contains("QT_"), "{text}");
}

#[test]
fn the_packaged_unit_starts_the_daemon_subcommand() {
    let text = packaged_unit();
    let starts: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("ExecStart"))
        .collect();
    assert_eq!(starts, ["ExecStart=%h/.local/bin/librepods daemon"]);
}

#[test]
fn a_run_killed_outright_leaves_no_process_once_its_bench_is_gone() {
    let bench = Bench::new();
    bench.set("hang-status", "");
    let root = bench.path("");
    let mut command = bench.command(Some(["--idle-window-seconds", "1"]), &[]);
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let mut run = bench.spawn(command);
    let status: i32 = wait_for_file(&bench.path("status-pid"))
        .trim()
        .parse()
        .unwrap();
    run.kill().unwrap();
    run.wait().unwrap();
    assert!(
        processes_in(&root).contains(&status),
        "the hanging status of a run killed outright is still there until the bench ends it"
    );
    drop(bench);
    assert_eq!(processes_in(&root), Vec::<i32>::new());
}

#[test]
fn a_second_run_during_a_run_stops_and_starts_nothing() {
    let bench = Bench::new();
    bench.set("hang-stop", "");
    let mut command = bench.command(Some(["--idle-window-seconds", "1"]), &[]);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let first = bench.spawn(command);
    wait_for_call(&bench, "systemctl --user stop librepods.service");
    let mut command = bench.command(Some(["--idle-window-seconds", "1"]), &[]);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let second = bench.spawn(command);
    let second = finish(second, RUN_DEADLINE);
    let calls = bench.calls();
    let first = finish(first, RUN_DEADLINE);
    assert_eq!(second.status.code(), Some(2), "{}", text(&second.stderr));
    assert_eq!(
        text(&second.stderr),
        "xtask hw: a run is in progress; nothing was stopped\n"
    );
    let count = |call: &str| calls.iter().filter(|made| *made == call).count();
    assert_eq!(
        count("systemctl --user stop librepods.service"),
        1,
        "{calls:?}"
    );
    assert_eq!(
        count("systemctl --user show --property=LoadState --value librepods-hw.service"),
        1,
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|call| call.starts_with("systemd-run")),
        "{calls:?}"
    );
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stdout));
    assert_restarted_after_the_run(&bench);
}
