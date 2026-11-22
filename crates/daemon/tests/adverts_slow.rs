//! The advert monitor registered again through the built daemon after each way BlueZ drops it, timed over several rounds at real time.

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

    use super::support::advert::keyed;
    use super::support::bluez::Call;
    use super::support::bluez::monitor::registration;
    use super::support::rig::Rig;
    use super::support::sleep::{RESUME_SETTLE, sleep, wake};
    use super::support::{HANG_LIMIT, pause, wait_within};

    /// The most BlueZ may go without the monitor after it dropped it.
    const REGAINED_WITHIN: Duration = Duration::from_secs(5);
    /// Rounds per way, for a range rather than one figure.
    const ROUNDS: usize = 5;
    /// A wake costs the settle each round, so it gets fewer.
    const WAKE_ROUNDS: usize = 3;
    /// Long enough for a monitor call the daemon wrongly makes at power off to reach the fake.
    const OFF_HOLD: Duration = Duration::from_millis(500);

    /// Started with the keys stored and the manager served on its adapter, the monitor registered once.
    fn listening() -> Rig {
        let mut rig = keyed();
        rig.start();
        wait_registered(&rig, 1);
        rig
    }

    /// Waits for `count` registrations in all and the monitor read, giving the moment both held; every call must be one of them.
    fn wait_registered(rig: &Rig, count: usize) -> Instant {
        let wanted: Vec<Call> = (0..count)
            .flat_map(|_| registration(rig.adapter()))
            .collect();
        let regained = wait_within(&format!("{count} registrations"), HANG_LIMIT, || {
            let made = rig.bluez().calls().len() >= wanted.len();
            (made && rig.bluez().monitors().len() == 1).then(Instant::now)
        });
        assert_eq!(rig.bluez().calls(), wanted);
        regained
    }

    /// Runs `drop_it` each round and times the registration that follows, failing past `REGAINED_WITHIN` after `from`.
    #[expect(
        clippy::print_stderr,
        reason = "the range of the rounds goes into the test's output"
    )]
    fn timed(
        what: &str,
        rig: &mut Rig,
        rounds: usize,
        mut drop_it: impl FnMut(&mut Rig) -> Instant,
        from: Duration,
    ) {
        let mut gaps = Vec::new();
        for round in 0..rounds {
            let dropped = drop_it(rig);
            let regained = wait_registered(rig, round.saturating_add(2));
            gaps.push(regained.saturating_duration_since(dropped));
        }
        gaps.sort();
        eprintln!(
            "{what}, {rounds} rounds: {:?} to {:?}",
            gaps.first().unwrap(),
            gaps.last().unwrap()
        );
        let limit = from.saturating_add(REGAINED_WITHIN);
        assert!(
            gaps.iter().all(|gap| *gap >= from && *gap <= limit),
            "{what}: {gaps:?}"
        );
        assert_eq!(rig.status()["advert_intake"], "listening");
    }

    #[test]
    #[ignore = "rounds at real time; the slow job runs it"]
    fn a_restarted_bluetoothd_gets_the_monitor_again_within_5_s() {
        let mut rig = listening();
        let restart = |rig: &mut Rig| {
            rig.bluez().drop_name();
            let restarted = Instant::now();
            rig.bluez().take_name();
            restarted
        };
        timed(
            "bluetoothd back to the monitor",
            &mut rig,
            ROUNDS,
            restart,
            Duration::ZERO,
        );
        rig.finish();
    }

    #[test]
    #[ignore = "rounds at real time; the slow job runs it"]
    fn an_adapter_power_cycle_gets_it_again_within_5_s() {
        let mut rig = listening();
        let cycle = |rig: &mut Rig| {
            let before = rig.bluez().calls();
            rig.bluez().power(rig.adapter(), false);
            pause(OFF_HOLD);
            assert_eq!(rig.bluez().calls(), before, "a monitor call at power off");
            let powered = Instant::now();
            rig.bluez().power(rig.adapter(), true);
            powered
        };
        timed(
            "power on to the monitor",
            &mut rig,
            ROUNDS,
            cycle,
            Duration::ZERO,
        );
        rig.finish();
    }

    #[test]
    #[ignore = "seconds of settle per round at real time; the slow job runs it"]
    fn a_wake_gets_it_again_within_5_s_of_the_settle() {
        let mut rig = listening();
        let night = |rig: &mut Rig| {
            sleep(rig);
            wake(rig)
        };
        timed(
            "a wake to the monitor",
            &mut rig,
            WAKE_ROUNDS,
            night,
            RESUME_SETTLE,
        );
        rig.finish();
    }

    #[test]
    #[ignore = "rounds at real time; the slow job runs it"]
    fn an_unsolicited_release_gets_it_again_within_5_s() {
        let mut rig = listening();
        let release = |rig: &mut Rig| {
            let released = Instant::now();
            rig.bluez().release_monitors();
            released
        };
        timed(
            "a release to the monitor",
            &mut rig,
            ROUNDS,
            release,
            Duration::ZERO,
        );
        rig.finish();
    }
}
