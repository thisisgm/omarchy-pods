//! The built binary's `probe capture` and `probe stop` around the installed service: stopped before the first dial and started again on every way out, with a stub `systemctl` ahead of every other on `PATH`.

#[cfg(test)]
#[expect(
    dead_code,
    reason = "the parts of the fake pods only the unit tests of the capture loop read go unused here"
)]
#[path = "../src/probe/capture/tests/att_pods.rs"]
mod att_pods;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "what the tests of the capture itself read of a run goes unused where only the service is watched"
)]
#[path = "support/capture.rs"]
mod capture;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the parts of the fake pods only the unit tests of the capture loop read go unused here"
)]
#[path = "../src/probe/capture/tests/pods.rs"]
mod pods;

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::time::Duration;

    use pods_proto::packets::{ATT_PSM, CONTROL_PSM};
    use pods_sys::l2cap::Channel;
    use pods_trap::{Answer, Outcome};
    use rustix::fs::FlockOperation;
    use rustix::io::Errno;
    use rustix::process::Signal;
    use tempfile::TempDir;

    use super::att_pods::AttPods;
    use super::capture::{
        A_FEW, ASKED, Analysed, CALLS_NAME, FAILED, HANG_LIMIT, INSTALLED, LOCK_HELD, LOCKED_NAME,
        RUN_LOCK, RUNTIME_NAME, Ran, Running, STAMP, STARTED, STOPPED, capture, expected, far_end,
        home_with_device, kinds, probe, read, refuse, route, run_path, stubs_of, transcript,
    };
    use super::pods::{self, Pods};

    /// What the stub log holds for a dial that came while the installed service was stopped.
    const DIALLED_STOPPED: &str = "dialled while librepods.service is inactive";
    const SAID_STOPPED: &str = "xtask probe: stopped librepods.service for this run";
    const SAID_STARTED: &str = "xtask probe: started librepods.service again";
    const SAID_NOT_STARTED: &str = "xtask probe: systemctl start librepods.service exited with exit status: 1, so librepods.service is not as the run found it; run systemctl --user start librepods.service";
    const SAID_REFUSED: &str =
        "xtask probe: could not connect the L2CAP channel: Host is down (os error 112)";
    const LOCK_HELD_REASON: &str =
        "a hardware run or another live probe is in progress, so nothing was stopped";
    const STOP_SENT: &str = "xtask probe: STOP was sent and the channel was kept open for 1000 ms";
    /// The script that dials the attribute channel alone and ends by itself.
    const MAP: [&str; 4] = ["--route", "dictation", "--script", "att-map"];

    /// Answers a dial by handing its far end over, after logging the dial among the stub's calls with the state the installed service is in.
    fn logged_dial(home: &Path) -> (Answer, mpsc::Receiver<Channel>) {
        let stubs = stubs_of(home);
        let (handed, far) = mpsc::channel();
        let serve = move |channel| {
            let state = fs::read_to_string(stubs.join(INSTALLED)).unwrap_or_default();
            let line = format!("dialled while {INSTALLED} is {}\n", state.trim());
            // A panic here would end the trap's thread, so a failed write only leaves the line missing.
            if let Ok(mut log) = OpenOptions::new().append(true).open(stubs.join(CALLS_NAME)) {
                drop(log.write_all(line.as_bytes()));
            }
            // The receiver is gone only when the test already failed.
            drop(handed.send(channel));
        };
        let answer = Answer::Accept {
            after: Duration::ZERO,
            serve: Arc::new(serve),
        };
        (answer, far)
    }

    /// A home whose installed service runs.
    fn home_with_service() -> TempDir {
        let home = home_with_device();
        fs::write(stubs_of(home.path()).join(INSTALLED), "active\n").unwrap();
        home
    }

    fn calls(home: &TempDir) -> Vec<String> {
        let log = fs::read_to_string(stubs_of(home.path()).join(CALLS_NAME)).unwrap();
        log.lines().map(str::to_owned).collect()
    }

    fn state(home: &TempDir) -> String {
        fs::read_to_string(stubs_of(home.path()).join(INSTALLED)).unwrap()
    }

    /// Runs the capture of the attribute table into run `name`, which ends by itself.
    fn map(home: &TempDir, name: &str) -> Ran {
        let (att, att_far) = logged_dial(home.path());
        let running = Running::start(capture(home, name, &MAP), vec![route(ATT_PSM, att)]);
        let played = pods::spawn_on(
            None,
            Some(far_end(&att_far)),
            Pods::default(),
            Arc::default(),
        );
        let ran = running.finish();
        drop(transcript(&played));
        ran
    }

    #[test]
    fn a_capture_stops_the_installed_service_before_it_dials_and_starts_it_after() {
        let home = home_with_service();
        let ran = map(&home, "map");

        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        let dialled = [DIALLED_STOPPED];
        assert_eq!(calls(&home), expected(&[&STOPPED, &dialled, &STARTED]));
        assert_eq!(state(&home), "active\n");
        let said: Vec<&str> = ran.stderr.lines().collect();
        let [first, summary, last] = said[..] else {
            panic!("{}", ran.stderr);
        };
        assert_eq!((first, last), (SAID_STOPPED, SAID_STARTED));
        assert!(
            summary.starts_with("xtask probe: capture complete: "),
            "{summary}"
        );
        // Every call of the stub found the hardware run's lock held by the capture.
        let locked = fs::read_to_string(stubs_of(home.path()).join(LOCKED_NAME)).unwrap();
        let held = locked.lines().filter(|line| *line == LOCK_HELD);
        assert_eq!(held.count(), STOPPED.len() + STARTED.len(), "{locked}");
    }

    #[test]
    fn a_capture_whose_connect_is_refused_starts_the_installed_service_again() {
        let home = home_with_service();
        let command = capture(&home, "page", &["--script", "soak", "--minutes", "1"]);
        let refused = vec![route(CONTROL_PSM, refuse(Errno::HOSTDOWN))];
        let ran = Running::start(command, refused).finish();

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        assert_eq!(calls(&home), expected(&[&STOPPED, &STARTED]));
        assert_eq!(state(&home), "active\n");
        let said: Vec<&str> = ran.stderr.lines().collect();
        assert_eq!(said, [SAID_STOPPED, SAID_STARTED, SAID_REFUSED]);
        assert_eq!(
            ran.dials,
            [(CONTROL_PSM, Outcome::Refused(Errno::HOSTDOWN))]
        );
    }

    #[test]
    fn a_refused_connect_and_a_start_that_fails_are_both_said_the_start_first() {
        let home = home_with_service();
        let fails = stubs_of(home.path()).join(format!("{INSTALLED}.start-fails"));
        fs::write(fails, "").unwrap();
        let command = capture(&home, "page", &["--script", "soak", "--minutes", "1"]);
        let refused = vec![route(CONTROL_PSM, refuse(Errno::HOSTDOWN))];
        let ran = Running::start(command, refused).finish();

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        assert_eq!(state(&home), "inactive\n");
        let said: Vec<&str> = ran.stderr.lines().collect();
        assert_eq!(said, [SAID_STOPPED, SAID_NOT_STARTED, SAID_REFUSED]);
    }

    #[test]
    fn a_stop_that_leaves_the_service_running_dials_nothing_and_starts_it_again() {
        let home = home_with_service();
        let stops_as = stubs_of(home.path()).join(format!("{INSTALLED}.stops-as"));
        fs::write(stops_as, "deactivating\n").unwrap();
        let (att, _) = logged_dial(home.path());
        let command = capture(&home, "map", &MAP);
        let ran = Running::start(command, vec![route(ATT_PSM, att)]).finish();

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        let reason = "librepods.service is still deactivating after its stop; librepods.service was started again";
        assert_eq!(ran.stderr, format!("xtask probe: {reason}\n"));
        assert_eq!(calls(&home), expected(&[&STOPPED, &STARTED]));
        assert_eq!(state(&home), "active\n");
        assert_eq!(ran.dials, []);
        let marks = read(&run_path(&home, "map"), "marks.tsv");
        assert_eq!(kinds(&marks), ["end"]);
        assert!(marks.ends_with(&format!("\t{reason}\n")), "{marks}");
    }

    #[test]
    fn probe_stop_starts_the_installed_service_again_when_it_ends_and_when_its_connect_is_refused()
    {
        let home = home_with_service();
        let (control, control_far) = logged_dial(home.path());
        let command = probe(home.path(), &["stop"]);
        let running = Running::start(command, vec![route(CONTROL_PSM, control)]);
        let played = pods::spawn(far_end(&control_far), Pods::default(), Arc::default());
        let ran = running.finish();
        drop(transcript(&played));

        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        let dialled = [DIALLED_STOPPED];
        assert_eq!(calls(&home), expected(&[&STOPPED, &dialled, &STARTED]));
        assert_eq!(state(&home), "active\n");
        let said: Vec<&str> = ran.stderr.lines().collect();
        assert_eq!(said, [SAID_STOPPED, STOP_SENT, SAID_STARTED]);

        let home = home_with_service();
        let command = probe(home.path(), &["stop"]);
        let refused = vec![route(CONTROL_PSM, refuse(Errno::HOSTDOWN))];
        let ran = Running::start(command, refused).finish();

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        assert_eq!(calls(&home), expected(&[&STOPPED, &STARTED]));
        assert_eq!(state(&home), "active\n");
        let said: Vec<&str> = ran.stderr.lines().collect();
        assert_eq!(said, [SAID_STOPPED, SAID_STARTED, SAID_REFUSED]);
    }

    #[test]
    fn a_signal_mid_capture_starts_the_installed_service_again() {
        let home = home_with_service();
        let (control, control_far) = logged_dial(home.path());
        let command = capture(&home, "uplink", &["--script", "soak", "--minutes", "1"]);
        let running = Running::start(command, vec![route(CONTROL_PSM, control)]);
        let flag = Arc::new(AtomicBool::new(false));
        let part = Pods {
            interrupt_after: Some(3),
            ..Pods::default()
        };
        let played = pods::spawn(far_end(&control_far), part, Arc::clone(&flag));
        running.interrupt_within(HANG_LIMIT, &flag);
        let ran = running.finish();
        drop(transcript(&played));

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        let dialled = [DIALLED_STOPPED];
        assert_eq!(calls(&home), expected(&[&STOPPED, &dialled, &STARTED]));
        assert_eq!(state(&home), "active\n");
        let said: Vec<&str> = ran.stderr.lines().collect();
        let [first, summary, last] = said[..] else {
            panic!("{}", ran.stderr);
        };
        assert_eq!((first, last), (SAID_STOPPED, SAID_STARTED));
        let early = "xtask probe: capture ended early: interrupted by a signal; STOP was sent (";
        assert!(summary.starts_with(early), "{summary}");
    }

    #[test]
    fn a_signal_during_probe_stop_ends_it_and_starts_the_installed_service_again() {
        for signal in [Signal::INT, Signal::TERM] {
            let home = home_with_service();
            let (control, control_far) = logged_dial(home.path());
            let command = probe(home.path(), &["stop"]);
            let running = Running::start(command, vec![route(CONTROL_PSM, control)]);
            // Pods that acknowledge nothing, so the signal lands while the stop waits on its handshake.
            let part = Pods {
                acks: 0,
                ..Pods::default()
            };
            let played = pods::spawn(far_end(&control_far), part, Arc::default());
            running.signal(signal);
            let ran = running.finish();
            drop(transcript(&played));

            let dialled = [DIALLED_STOPPED];
            let wanted = expected(&[&STOPPED, &dialled, &STARTED]);
            assert_eq!(calls(&home), wanted, "{signal:?}: {}", ran.stderr);
            assert_eq!(state(&home), "active\n", "{signal:?}");
            assert_eq!(ran.code, FAILED, "{signal:?}: {}", ran.stderr);
            let said: Vec<&str> = ran.stderr.lines().collect();
            let ended = "xtask probe: STOP was not sent: interrupted by a signal";
            assert_eq!(said, [SAID_STOPPED, ended, SAID_STARTED], "{signal:?}");
        }
    }

    #[test]
    fn a_service_that_was_stopped_stays_stopped_and_nothing_is_said_of_it() {
        let home = home_with_device();
        let ran = map(&home, "map");

        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        let dialled = [ASKED, DIALLED_STOPPED];
        assert_eq!(calls(&home), expected(&[&dialled]));
        assert_eq!(state(&home), "inactive\n");
        assert_eq!(ran.stderr.lines().count(), 1, "{}", ran.stderr);
        assert!(!ran.stderr.contains(INSTALLED), "{}", ran.stderr);
    }

    #[test]
    fn a_start_that_fails_is_the_last_thing_said_and_fails_the_capture() {
        let home = home_with_service();
        let fails = stubs_of(home.path()).join(format!("{INSTALLED}.start-fails"));
        fs::write(fails, "").unwrap();
        let ran = map(&home, "map");

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        assert_eq!(state(&home), "inactive\n");
        let said: Vec<&str> = ran.stderr.lines().collect();
        let [first, summary, last] = said[..] else {
            panic!("{}", ran.stderr);
        };
        assert_eq!(first, SAID_STOPPED);
        assert!(
            summary.starts_with("xtask probe: capture complete: "),
            "{summary}"
        );
        assert_eq!(last, SAID_NOT_STARTED);
        // The script ended the marks itself, so the failed start adds no second end.
        let marks = read(&run_path(&home, "map"), "marks.tsv");
        let ends = kinds(&marks).into_iter().filter(|kind| *kind == "end");
        assert_eq!(ends.count(), 1, "{marks}");
    }

    #[test]
    fn a_capture_refused_before_its_dial_leaves_the_installed_service_alone() {
        let home = home_with_service();
        // A run is never reused, which the capture finds last of all before it dials.
        fs::create_dir(run_path(&home, "taken")).unwrap();
        let (att, _) = logged_dial(home.path());
        let command = capture(&home, "taken", &MAP);
        let ran = Running::start(command, vec![route(ATT_PSM, att)]).finish();

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        assert!(
            ran.stderr.contains("a run is never reused"),
            "{}",
            ran.stderr
        );
        assert_eq!(calls(&home), Vec::<String>::new());
        assert_eq!(state(&home), "active\n");
        assert_eq!(ran.dials, []);
    }

    #[test]
    fn a_capture_during_a_hardware_run_leaves_its_name_free() {
        let home = home_with_service();
        let lock = home.path().join(RUNTIME_NAME).join(RUN_LOCK);
        let held = fs::File::create(lock).unwrap();
        rustix::fs::flock(&held, FlockOperation::NonBlockingLockExclusive).unwrap();
        let (att, _) = logged_dial(home.path());
        let name = "map";
        let command = capture(&home, name, &MAP);
        let ran = Running::start(command, vec![route(ATT_PSM, att)]).finish();

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        assert_eq!(ran.stderr, format!("xtask probe: {LOCK_HELD_REASON}\n"));
        assert_eq!(calls(&home), Vec::<String>::new());
        assert_eq!(state(&home), "active\n");
        assert_eq!(ran.dials, []);
        let run = run_path(&home, name);
        assert!(!run.exists(), "the refused capture created {run:?}");
        drop(held);

        let retried = map(&home, name);
        assert_eq!(retried.code, Some(0), "{}", retried.stderr);
        assert!(run.is_dir(), "the retried capture did not create {run:?}");
    }

    #[test]
    fn the_commands_that_only_read_files_never_run_systemctl() {
        let att = AttPods {
            interrupt_after: Some(A_FEW),
            ..AttPods::default()
        };
        // The capture asks for the state of the service once; the analysis of its run asks nothing.
        let found = Analysed::of(att, HANG_LIMIT);
        assert_eq!(calls(&found.home), [ASKED]);
        let init = probe(found.home.path(), &["init", "--stamp", STAMP])
            .output()
            .unwrap();
        assert!(init.status.success(), "{init:?}");
        assert_eq!(calls(&found.home), [ASKED]);
    }
}
