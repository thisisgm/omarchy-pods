//! The disconnect notice where real time makes it slow: a parked device, minutes up the held ladder, and a hung `omarchy` the daemon must kill.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::support::children::{children, the_only_child};
    use super::support::omarchy::{DISCONNECT_NOTICE, Mood, NOTIFY_WAIT, RECORDED_WITHIN};
    use super::support::parked::{climb_to_parked, refused_pods};
    use super::support::rig::{Rig, assert_gap};
    use super::support::{HANG_LIMIT, wait_within};

    #[test]
    #[ignore = "minutes at real time; the slow job runs it"]
    fn a_parked_device_sends_none() {
        let (mut rig, device) = refused_pods();
        rig.link_up(&device);
        climb_to_parked(&rig);
        rig.link_down(&device);
        rig.wait_logged("link: parked to idle");
        rig.omarchy().never_ran(RECORDED_WITHIN);
        rig.finish();
    }

    #[test]
    #[ignore = "seconds of a hung run at real time; the slow job runs it"]
    fn a_hung_omarchy_is_killed_after_the_wait() {
        let (mut rig, device) = Rig::with_linked_pods();
        rig.omarchy().become_(Mood::Hangs);
        let lost = Instant::now();
        rig.disconnect();
        rig.omarchy().wait_calls(1, HANG_LIMIT);
        let pid = rig.daemon().pid();
        let hung = the_only_child(pid);
        rig.omarchy().become_(Mood::Works);
        rig.relink(&device);
        rig.disconnect();
        wait_within(
            "the hung run killed and reaped",
            NOTIFY_WAIT.saturating_add(HANG_LIMIT),
            || {
                // Read before the children, so a kill and the next run between the two reads cannot count as overlap.
                let calls = rig.omarchy().calls();
                if !children(pid).contains(&hung) {
                    return Some(());
                }
                assert_eq!(calls.len(), 1, "a second run while the first hung");
                None
            },
        );
        assert_gap(
            "the kill after the loss",
            lost,
            Instant::now(),
            &(NOTIFY_WAIT..),
        );
        let calls = rig.omarchy().wait_calls(2, HANG_LIMIT);
        assert_eq!(calls, [DISCONNECT_NOTICE, DISCONNECT_NOTICE]);
        rig.wait_logged("notify: omarchy hung and was killed");
        rig.finish();
    }
}
