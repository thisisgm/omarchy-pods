//! A wake through the built daemon where real time makes it slow: pods parked at the end of the held ladder, minutes up it.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use super::support::HANG_LIMIT;
    use super::support::parked::{climb_to_parked, refused_pods};
    use super::support::rig::{HELD_LADDER, assert_gap, not_before};
    use super::support::sleep::{RESUME_SETTLE, sleep, wake};

    /// The first dial and one per held step; the next comes only from the parked wait or a wake.
    const HELD_DIALS: usize = HELD_LADDER.len().saturating_add(1);

    #[test]
    #[ignore = "minutes at real time; the slow job runs it"]
    fn a_wake_retries_a_parked_device_within_the_settle_plus_slack() {
        let (mut rig, device) = refused_pods();
        rig.link_up(&device);
        climb_to_parked(&rig);
        sleep(&mut rig);
        let woke = wake(&mut rig);
        let dials = rig.wait_dials(
            HELD_DIALS.saturating_add(1),
            RESUME_SETTLE.saturating_add(HANG_LIMIT),
        );
        let retry = dials.last().unwrap().at;
        assert_gap(
            "the parked dial after the wake",
            woke,
            retry,
            &not_before(RESUME_SETTLE),
        );
        rig.finish();
    }
}
