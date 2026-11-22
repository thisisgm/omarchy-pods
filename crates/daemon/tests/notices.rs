//! The disconnect notice through the built daemon: the fake pods fill the state, the link is lost, and the stand-in `omarchy` on the session's path records what the daemon ran.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::fs::{self, Permissions};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use pods_fake::{Fault, Profile};
    use rustix::process::{getpid, set_child_subreaper};
    use serde_json::Value;

    use super::support::children::{children, reaped_children_faults, the_only_child};
    use super::support::omarchy::{
        DISCONNECT_NOTICE, Mood, NOTIFY_WAIT, PROGRAM, RECORDED_WITHIN, REFUSAL,
    };
    use super::support::rig::{ABSENT_GRACE, REAL_TIME_SLACK, Rig, assert_gap, not_before};
    use super::support::{HANG_LIMIT, pause, wait_for};

    /// The daemon's wait for the notifier to stop.
    const NOTIFIER_STOP_WAIT: Duration = Duration::from_millis(500);

    /// The run in flight, then the queue's 8, then the one dropped.
    const FULL_AT_LOSS: usize = 10;

    /// A stand-in the daemon finds on its path and cannot run.
    const READ_WRITE: u32 = 0o600;

    const NOT_FOUND: &str = "notify: omarchy not found";
    const CANNOT_RUN: &str = "notify: omarchy failed: Permission denied (os error 13)";
    const EXITED_ONE: &str = "notify: omarchy failed: exit status: 1";
    const QUEUE_FULL: &str = "notify: queue full";
    const NOTIFY_PREFIX: &str = "notify:";

    /// The lines of the daemon's log that the notifier wrote.
    fn notifier_lines(rig: &mut Rig) -> Vec<String> {
        rig.daemon()
            .log()
            .lines()
            .filter(|line| line.starts_with(NOTIFY_PREFIX))
            .map(str::to_owned)
            .collect()
    }

    /// Two user disconnects with the stand-in in `mood`, then the status, the runs recorded and the notifier's lines.
    fn two_losses(mood: Mood) -> (Value, usize, Vec<String>) {
        let (mut rig, device) = Rig::with_linked_pods();
        rig.omarchy().become_(mood);
        rig.disconnect();
        rig.relink(&device);
        rig.disconnect();
        pause(RECORDED_WITHIN);
        let status = rig.status();
        let runs = rig.omarchy().calls().len();
        let lines = notifier_lines(&mut rig);
        rig.finish();
        (status, runs, lines)
    }

    #[test]
    fn a_lost_link_sends_one_disconnect_notice_at_the_reset() {
        let (rig, device) = Rig::with_linked_pods();
        let down = Instant::now();
        rig.link_down(&device);
        let calls = rig
            .omarchy()
            .wait_calls(1, ABSENT_GRACE.saturating_add(HANG_LIMIT));
        assert_gap(
            "the notice after the pods went",
            down,
            Instant::now(),
            &not_before(ABSENT_GRACE),
        );
        assert_eq!(calls, [DISCONNECT_NOTICE]);
        rig.omarchy().none_for(RECORDED_WITHIN);
        rig.finish();
    }

    #[test]
    fn a_link_that_never_filled_sends_none() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        device.pods.fault(Fault::NeverAcknowledge);
        rig.start();
        rig.link_up(&device);
        rig.wait_logged("link: opening to handshake");
        rig.link_down(&device);
        rig.wait_logged("link: closing to idle");
        rig.omarchy().never_ran(RECORDED_WITHIN);
        rig.finish();
    }

    #[test]
    fn a_second_loss_after_a_reconnect_sends_a_second_notice() {
        let (rig, device) = Rig::with_linked_pods();
        rig.link_down(&device);
        rig.omarchy()
            .wait_calls(1, ABSENT_GRACE.saturating_add(HANG_LIMIT));
        rig.relink(&device);
        rig.link_down(&device);
        let calls = rig
            .omarchy()
            .wait_calls(2, ABSENT_GRACE.saturating_add(HANG_LIMIT));
        assert_eq!(calls, [DISCONNECT_NOTICE, DISCONNECT_NOTICE]);
        rig.omarchy().none_for(RECORDED_WITHIN);
        rig.finish();
    }

    #[test]
    fn a_user_disconnect_sends_one() {
        let (rig, _device) = Rig::with_linked_pods();
        let asked = Instant::now();
        rig.disconnect();
        let calls = rig.omarchy().wait_calls(1, HANG_LIMIT);
        assert_gap(
            "the notice after do disconnect, with no grace",
            asked,
            Instant::now(),
            &(Duration::ZERO..=REAL_TIME_SLACK),
        );
        assert_eq!(calls, [DISCONNECT_NOTICE]);
        rig.omarchy().none_for(RECORDED_WITHIN);
        rig.finish();
    }

    #[test]
    fn a_missing_omarchy_logs_once_and_changes_nothing() {
        let (absent, absent_runs, absent_lines) = two_losses(Mood::Absent);
        let (works, works_runs, works_lines) = two_losses(Mood::Works);
        assert_eq!(absent_lines, [NOT_FOUND]);
        assert_eq!(absent_runs, 0);
        assert_eq!(works_lines, [""; 0]);
        assert_eq!(works_runs, 2);
        assert_eq!(absent, works);
    }

    #[test]
    fn an_omarchy_that_cannot_run_logs_once_as_failed() {
        let (mut rig, device) = Rig::with_linked_pods();
        let program = rig.session().bin().join(PROGRAM);
        fs::set_permissions(&program, Permissions::from_mode(READ_WRITE)).unwrap();
        rig.disconnect();
        rig.wait_logged(CANNOT_RUN);
        rig.relink(&device);
        rig.disconnect();
        pause(RECORDED_WITHIN);
        assert_eq!(notifier_lines(&mut rig), [CANNOT_RUN]);
        assert_eq!(rig.omarchy().calls(), [[""; 0]; 0]);
        rig.finish();
    }

    #[test]
    fn a_failing_omarchy_is_reaped_and_the_next_notice_goes() {
        let (mut rig, device) = Rig::with_linked_pods();
        rig.omarchy().become_(Mood::Fails);
        let pid = rig.daemon().pid();
        let reaped = || (children(pid).is_empty() && reaped_children_faults(pid) > 0).then_some(());
        rig.disconnect();
        rig.omarchy().wait_calls(1, HANG_LIMIT);
        wait_for("the failed run reaped", reaped);
        rig.relink(&device);
        rig.disconnect();
        let calls = rig.omarchy().wait_calls(2, HANG_LIMIT);
        assert_eq!(calls, [DISCONNECT_NOTICE, DISCONNECT_NOTICE]);
        wait_for("both failed runs reaped", || {
            children(pid).is_empty().then_some(())
        });
        pause(RECORDED_WITHIN);
        assert_eq!(notifier_lines(&mut rig), [EXITED_ONE]);
        let log = rig.daemon().log();
        let refusals = log.lines().filter(|line| *line == REFUSAL);
        assert_eq!(refusals.count(), 2, "{log}");
        rig.finish();
    }

    #[test]
    fn no_child_is_left_after_stop() {
        let (mut rig, _device) = Rig::with_linked_pods();
        rig.omarchy().become_(Mood::Hangs);
        rig.disconnect();
        rig.omarchy().wait_calls(1, HANG_LIMIT);
        let pid = rig.daemon().pid();
        let hung = the_only_child(pid);
        // A run the daemon left unreaped comes to this process and stays a zombie, so `/proc` still shows it.
        set_child_subreaper(Some(getpid())).unwrap();
        let status = rig.stop(NOTIFIER_STOP_WAIT.saturating_add(REAL_TIME_SLACK));
        assert!(status.success(), "the daemon exited with {status}");
        let proc = format!("/proc/{hung}");
        assert!(
            !Path::new(&proc).exists(),
            "the hung run outlived the daemon or was left unreaped"
        );
        assert_eq!(notifier_lines(&mut rig), [""; 0]);
        rig.finish();
    }

    #[test]
    fn a_full_queue_drops_the_new_notice_and_says_so() {
        let (mut rig, device) = Rig::with_linked_pods();
        rig.omarchy().become_(Mood::Hangs);
        rig.disconnect();
        rig.omarchy().wait_calls(1, HANG_LIMIT);
        let started = Instant::now();
        for loss in 2..=FULL_AT_LOSS {
            rig.relink(&device);
            rig.disconnect();
            let full = notifier_lines(&mut rig).contains(&QUEUE_FULL.to_owned());
            assert_eq!(full, loss == FULL_AT_LOSS, "a full queue at loss {loss}");
        }
        assert!(
            started.elapsed() < NOTIFY_WAIT,
            "the losses outlasted the hung run, which frees a slot"
        );
        assert_eq!(
            rig.omarchy().calls().len(),
            1,
            "a second run while one hung"
        );
        assert_eq!(notifier_lines(&mut rig), [QUEUE_FULL]);
        rig.finish();
    }
}
