//! A suspend through the built daemon: the fake logind sends `PrepareForSleep`, the fake BlueZ and pods play the link, and the stand-in `omarchy` records the notices.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use pods_fake::Profile;

    use super::support::bus::Listener;
    use super::support::logind::SLEEP_SIGNAL;
    use super::support::omarchy::{DISCONNECT_NOTICE, RECORDED_WITHIN, loss_is_announced};
    use super::support::parked::refused_pods;
    use super::support::rig::{
        ABSENT_GRACE, HELD_LADDER, REAL_TIME_SLACK, Rig, assert_gap, not_before,
    };
    use super::support::sleep::{
        RESUME_SETTLE, SLEEPING, SYSTEM_BUS_RETRY, WATCHING, WOKE, logged, sleep, wake,
    };
    use super::support::{HANG_LIMIT, pause, wait_within};

    /// The held dials before the wake; the wait after the last of them is `HELD_LADDER[3]`, far past the settle.
    const DIALS_BEFORE_THE_WAKE: usize = 4;

    /// Waits for sweep number `count + 1` and checks it came the settle after `woke`, not before.
    fn wait_wake_sweep(rig: &Rig, count: usize, woke: Instant) {
        let sweeps = wait_within(
            "the sweep after the settle",
            RESUME_SETTLE.saturating_add(HANG_LIMIT),
            || {
                let sweeps = rig.bluez().sweeps();
                (sweeps.len() > count).then_some(sweeps)
            },
        );
        assert_gap(
            "the sweep after the wake",
            woke,
            sweeps[count],
            &not_before(RESUME_SETTLE),
        );
    }

    /// Waits for the line `line` in the daemon's log, which may take the settle and a whole grace.
    fn wait_long_logged(rig: &mut Rig, line: &str) {
        let limit = RESUME_SETTLE
            .saturating_add(ABSENT_GRACE)
            .saturating_add(HANG_LIMIT);
        wait_within(&format!("the daemon to log `{line}`"), limit, || {
            (logged(rig, line) > 0).then_some(())
        });
    }

    /// Checks the grace begun after `down` ends past the settle begun by now, so only the window can silence its loss.
    fn assert_grace_outlasts_settle(down: Instant) {
        let since = down.elapsed();
        let settled = since
            .saturating_add(RESUME_SETTLE)
            .saturating_add(REAL_TIME_SLACK);
        assert!(
            settled < ABSENT_GRACE,
            "the wake came {since:?} after the loss, so the grace of {ABSENT_GRACE:?} could end inside the settle"
        );
    }

    #[test]
    fn a_link_lost_during_suspend_sends_no_notice_even_when_its_grace_ends_later() {
        let (mut rig, device) = Rig::with_linked_pods();
        sleep(&mut rig);
        let down = Instant::now();
        rig.link_down(&device);
        rig.wait_entered("grace", 1);
        wake(&mut rig);
        assert_grace_outlasts_settle(down);
        wait_long_logged(&mut rig, "link: grace to idle");
        rig.omarchy().never_ran(RECORDED_WITHIN);
        rig.finish();
    }

    #[test]
    fn a_link_lost_after_the_settle_sends_one() {
        let (mut rig, device) = Rig::with_linked_pods();
        sleep(&mut rig);
        let count = rig.bluez().sweeps().len();
        let woke = wake(&mut rig);
        wait_wake_sweep(&rig, count, woke);
        loss_is_announced(&rig, &device);
        rig.finish();
    }

    #[test]
    fn a_wake_in_backoff_dials_now() {
        let (mut rig, device) = refused_pods();
        rig.link_up(&device);
        let climb: Duration = HELD_LADDER.iter().take(DIALS_BEFORE_THE_WAKE).sum();
        let dials = rig.wait_dials(DIALS_BEFORE_THE_WAKE, climb.saturating_add(HANG_LIMIT));
        let last = dials.last().unwrap().at;
        sleep(&mut rig);
        let woke = wake(&mut rig);
        let dials = rig.wait_dials(
            DIALS_BEFORE_THE_WAKE.saturating_add(1),
            RESUME_SETTLE.saturating_add(HANG_LIMIT),
        );
        let next = dials.last().unwrap().at;
        assert_gap(
            "the dial after the wake",
            woke,
            next,
            &not_before(RESUME_SETTLE),
        );
        let held = HELD_LADDER[DIALS_BEFORE_THE_WAKE.saturating_sub(1)];
        assert!(
            next.duration_since(last) < held,
            "the dial waited out the held step of {held:?}"
        );
        rig.finish();
    }

    #[test]
    fn a_wake_sweeps_bluez_once() {
        let (mut rig, _device) = Rig::with_known_pods();
        sleep(&mut rig);
        let count = rig.bluez().sweeps().len();
        let woke = wake(&mut rig);
        wait_wake_sweep(&rig, count, woke);
        pause(REAL_TIME_SLACK);
        assert_eq!(rig.bluez().sweeps().len(), count.saturating_add(1));
        rig.finish();
    }

    #[test]
    fn a_wake_with_the_link_up_changes_nothing() {
        let (mut rig, _device) = Rig::with_linked_pods();
        let before = rig.status();
        let dials = rig.dials().len();
        sleep(&mut rig);
        let count = rig.bluez().sweeps().len();
        let woke = wake(&mut rig);
        wait_wake_sweep(&rig, count, woke);
        rig.omarchy().never_ran(RECORDED_WITHIN);
        assert_eq!(rig.status(), before);
        assert_eq!(rig.dials().len(), dials);
        rig.finish();
    }

    #[test]
    fn an_impostor_signal_is_ignored() {
        let (mut rig, device) = Rig::with_linked_pods();
        rig.wait_logged(WATCHING);
        let impostor = rig.impostor();
        let rule = format!("type='signal',member='{SLEEP_SIGNAL}'");
        let mut everyone = Listener::subscribe(&rig.system_bus_address(), &rule);
        impostor.prepare_for_sleep(true);
        // The bus has routed the signal; the grace wait below gives the daemon time to act on any copy.
        everyone.next();
        loss_is_announced(&rig, &device);
        assert_eq!(
            logged(&mut rig, SLEEPING),
            0,
            "the daemon took the impostor's edge"
        );
        rig.finish();
    }

    #[test]
    fn no_logind_on_the_bus_changes_nothing() {
        let mut rig = Rig::new();
        rig.logind().drop_name();
        let device = rig.add_pods(Profile::default());
        rig.start();
        rig.link_up(&device);
        rig.wait_filled();
        loss_is_announced(&rig, &device);
        rig.finish();
    }

    #[test]
    fn a_second_sleep_inside_the_grace_keeps_the_first_loss_silent() {
        let (mut rig, device) = Rig::with_linked_pods();
        sleep(&mut rig);
        rig.link_down(&device);
        rig.wait_entered("grace", 1);
        let count = rig.bluez().sweeps().len();
        let woke = wake(&mut rig);
        wait_wake_sweep(&rig, count, woke);
        // The wake's sweep leaves in the step that starts the grace again.
        let regraced = rig.bluez().sweeps()[count];
        sleep(&mut rig);
        let since = regraced.elapsed();
        assert!(
            since.saturating_add(REAL_TIME_SLACK) < ABSENT_GRACE,
            "the second sleep came {since:?} after the grace began again, past its {ABSENT_GRACE:?}"
        );
        wait_long_logged(&mut rig, "link: grace to idle");
        rig.omarchy().never_ran(RECORDED_WITHIN);
        rig.finish();
    }

    #[test]
    fn a_loss_after_the_link_outlived_a_suspend_is_announced_across_a_later_sleep() {
        let (mut rig, device) = Rig::with_linked_pods();
        sleep(&mut rig);
        let count = rig.bluez().sweeps().len();
        let woke = wake(&mut rig);
        wait_wake_sweep(&rig, count, woke);
        let down = Instant::now();
        rig.link_down(&device);
        rig.wait_entered("grace", 1);
        sleep(&mut rig);
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
        rig.finish();
    }

    #[test]
    fn a_bus_loss_while_asleep_does_not_leave_notices_off() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        rig.relay_system_bus();
        rig.start();
        rig.link_up(&device);
        rig.wait_filled();
        sleep(&mut rig);
        let sweeps = rig.bluez().sweeps().len();
        let cut = Instant::now();
        rig.cut_system_bus();
        let back = SYSTEM_BUS_RETRY.saturating_add(HANG_LIMIT);
        let watching = wait_within("the sleep watcher subscribed again", back, || {
            (logged(&mut rig, WATCHING) > 1).then(Instant::now)
        });
        let swept = wait_within("the BlueZ watcher's sweep after the cut", back, || {
            (rig.bluez().sweeps().len() > sweeps).then(Instant::now)
        });
        // Each watcher is back the retry after the cut, so the loss below comes past the settle.
        let again = not_before(SYSTEM_BUS_RETRY);
        assert_gap(
            "the sleep watcher back after the cut",
            cut,
            watching,
            &again,
        );
        assert_gap("the BlueZ watcher back after the cut", cut, swept, &again);
        rig.wait_filled();
        loss_is_announced(&rig, &device);
        assert_eq!(logged(&mut rig, WOKE), 0, "a wake edge reached the daemon");
        rig.finish();
    }

    #[test]
    fn a_loss_while_asleep_with_no_wake_sends_no_notice() {
        let (mut rig, device) = Rig::with_linked_pods();
        sleep(&mut rig);
        // No sooner than the cut test's loss, so only the cut can tell the two apart.
        pause(SYSTEM_BUS_RETRY.saturating_add(REAL_TIME_SLACK));
        rig.link_down(&device);
        wait_long_logged(&mut rig, "link: grace to idle");
        rig.omarchy().never_ran(RECORDED_WITHIN);
        rig.finish();
    }

    #[test]
    fn sleep_twice_then_wake_once_is_one_window() {
        let (mut rig, device) = Rig::with_linked_pods();
        sleep(&mut rig);
        let down = Instant::now();
        rig.link_down(&device);
        rig.wait_entered("grace", 1);
        sleep(&mut rig);
        let count = rig.bluez().sweeps().len();
        let woke = wake(&mut rig);
        assert_grace_outlasts_settle(down);
        wait_wake_sweep(&rig, count, woke);
        wait_long_logged(&mut rig, "link: grace to idle");
        rig.omarchy().never_ran(RECORDED_WITHIN);
        assert_eq!(
            rig.logind().method_calls(),
            [""; 0],
            "the daemon called logind"
        );
        rig.relink(&device);
        loss_is_announced(&rig, &device);
        rig.finish();
    }
}
