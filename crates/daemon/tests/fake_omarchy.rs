//! Support tests: the stand-in `omarchy` on the session's path, run as the daemon's command would find it.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "these tests run the stand-in alone, so the daemon helpers the other suites share go unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::path::Path;
    use std::process::ExitStatus;
    use std::time::{Duration, Instant};

    use super::support::omarchy::{HANG, Mood, Omarchy, PROGRAM};
    use super::support::rig::{assert_gap, not_before};
    use super::support::{DAEMON_LOG, Daemon, Session};

    /// The quiet a test waits through to see that nothing more ran.
    const QUIET: Duration = Duration::from_millis(300);

    /// Runs the stand-in by name, as found on the session's path, under a guard, and waits for its exit.
    fn run(session: &Session, args: &[&str]) -> ExitStatus {
        let mut command = session.command(Path::new(PROGRAM), args);
        session.spawn_command(&mut command, DAEMON_LOG).wait_exit()
    }

    fn owned(args: &[&str]) -> Vec<String> {
        args.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn a_run_is_recorded_with_its_arguments_in_order() {
        let session = Session::new();
        let omarchy = Omarchy::install(&session, Mood::Works);
        let args = [
            "notification",
            "send",
            "--",
            "",
            "  two  spaces  ",
            "\u{f025} Left 9%",
            "%s $HOME `x`",
        ];
        assert!(run(&session, &args).success());
        assert_eq!(omarchy.calls(), [owned(&args)]);
    }

    #[test]
    fn two_runs_are_two_entries() {
        let session = Session::new();
        let omarchy = Omarchy::install(&session, Mood::Works);
        assert!(run(&session, &["notification", "send", "one"]).success());
        assert!(run(&session, &["notification", "send", "two"]).success());
        let calls = omarchy.wait_calls(2, QUIET);
        assert_eq!(
            calls,
            [
                owned(&["notification", "send", "one"]),
                owned(&["notification", "send", "two"]),
            ]
        );
        omarchy.none_for(QUIET);
    }

    #[test]
    fn fails_exits_non_zero_and_still_records() {
        let session = Session::new();
        let omarchy = Omarchy::install(&session, Mood::Works);
        omarchy.become_(Mood::Fails);
        let status = run(&session, &["notification", "send", "refused"]);
        assert!(!status.success(), "{status}");
        assert_eq!(
            omarchy.calls(),
            [owned(&["notification", "send", "refused"])]
        );
    }

    #[test]
    fn hangs_ends_by_itself_within_10_s() {
        let session = Session::new();
        let omarchy = Omarchy::install(&session, Mood::Hangs);
        let started = Instant::now();
        let status = run(&session, &["notification", "send", "stuck"]);
        assert_gap(
            "the hung stand-in's exit",
            started,
            Instant::now(),
            &not_before(HANG),
        );
        assert!(status.success(), "{status}");
        assert_eq!(omarchy.calls(), [owned(&["notification", "send", "stuck"])]);
    }

    #[test]
    fn absent_leaves_no_program() {
        let session = Session::new();
        let omarchy = Omarchy::install(&session, Mood::Works);
        omarchy.become_(Mood::Absent);
        let spawned = session
            .command(Path::new(PROGRAM), &["notification", "send", "nobody"])
            .spawn();
        let error = match spawned {
            Ok(child) => {
                let _guard = Daemon {
                    child,
                    log: session.dir.path().join(DAEMON_LOG),
                };
                panic!("a program named {PROGRAM} ran from the session's path");
            }
            Err(error) => error,
        };
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert_eq!(omarchy.calls(), Vec::<Vec<String>>::new());
    }
}
