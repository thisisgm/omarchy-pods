//! Adapter and BlueZ recovery through the built daemon, the trap, the fake BlueZ and the fake pods.

#[cfg(test)]
#[expect(
    dead_code,
    reason = "this suite uses only part of the shared test support"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use pods_trap::Outcome;
    use rustix::io::Errno;
    use zbus::OwnedVariant;

    use super::support::bluez::AAP_UUID;
    use super::support::parked::{climb_to_parked, refused_pods};
    use super::support::rig::{
        ABSENT_GRACE, DIAL_WAIT, HELD_LADDER, REAL_TIME_SLACK, Rig, SIGNAL_SLACK, assert_gap,
        connected,
    };
    use super::support::{HANG_LIMIT, pause, wait_for};

    fn wait_for_sweep(rig: &Rig, before: usize) {
        wait_for("the next sweep at the fake BlueZ", || {
            (rig.bluez().sweeps().len() > before).then_some(())
        });
        rig.bluez().wait_signals_taken();
        rig.wait_queued_inputs_applied();
    }

    #[test]
    fn a_device_down_then_up_in_backoff_dials_at_once() {
        let (mut rig, device) = Rig::with_linked_pods();
        // A populated link enters grace on DeviceDown, so DeviceUp can cut the wait short.
        rig.close_channel(&device);
        rig.wait_entered("backoff", 2);
        let refused = wait_for("the refused redial in the trap's record", || {
            let dials = rig.dials();
            let redial = dials.get(1)?;
            (redial.outcome == Outcome::Refused(Errno::CONNREFUSED)).then_some(redial.at)
        });

        rig.accept(&device);
        rig.link_down(&device);
        rig.wait_entered("grace", 1);
        let up = Instant::now();
        rig.link_up(&device);
        let next = rig.wait_dials(3, DIAL_WAIT)[2].at;
        assert_gap(
            "the dial after DeviceUp",
            up,
            next,
            &(Duration::ZERO..DIAL_WAIT),
        );
        assert_gap(
            "the dial beat the held ladder",
            refused,
            next,
            &(..HELD_LADDER[0]),
        );
        rig.wait_filled();
        rig.finish();
    }

    #[test]
    #[ignore = "minutes at real time; the slow job runs it"]
    fn a_device_removed_and_added_in_parked_dials_at_once() {
        let (rig, device) = refused_pods();
        rig.link_up(&device);
        climb_to_parked(&rig);
        let parked_dials = rig.dials().len();
        let sweeps = rig.bluez().sweeps().len();
        rig.bluez().remove(&device.path);
        wait_for_sweep(&rig, sweeps);

        rig.accept(&device);
        let added = Instant::now();
        let path = rig
            .bluez()
            .add_device(rig.adapter(), device.address, &[AAP_UUID], true);
        assert_eq!(path, device.path);
        let next = rig.wait_dials(parked_dials.saturating_add(1), DIAL_WAIT)[parked_dials].at;
        assert_gap(
            "the dial after InterfacesAdded",
            added,
            next,
            &(Duration::ZERO..DIAL_WAIT),
        );
        rig.wait_filled();
        rig.finish();
    }

    #[test]
    fn an_adapter_power_cycle_with_device_edges_ends_up() {
        let (mut rig, device) = Rig::with_linked_pods();
        rig.bluez().power(rig.adapter(), false);
        rig.bluez()
            .set(&device.path, "Connected", OwnedVariant::from(false));
        device.pods.wait_closed(1, HANG_LIMIT).unwrap();
        rig.wait_entered("grace", 1);
        assert!(!connected(&rig.status()));

        rig.bluez().power(rig.adapter(), true);
        rig.link_up(&device);
        rig.wait_filled();
        assert_eq!(rig.dials().len(), 2);
        rig.finish();
    }

    #[test]
    fn bluez_leaving_and_returning_with_the_device_connected_keeps_the_channel() {
        let (rig, device) = Rig::with_linked_pods();
        let sweeps = rig.bluez().sweeps().len();
        rig.bluez().drop_name();
        rig.holds_for(SIGNAL_SLACK, "connected while BlueZ was away", connected);
        rig.bluez().take_name();
        wait_for_sweep(&rig, sweeps);

        let closed = device.pods.wait_closed(1, SIGNAL_SLACK);
        assert!(closed.is_err(), "the channel closed: {closed:?}");
        assert!(connected(&rig.status()));
        assert_eq!(rig.dials().len(), 1);
        assert_eq!(device.pods.most_open_at_once(), 1);
        rig.finish();
    }

    #[test]
    fn bluez_returning_with_the_device_disconnected_dials_nothing_and_sends_no_notice_inside_the_grace()
     {
        let (mut rig, device) = Rig::with_linked_pods();
        rig.bluez().drop_name();
        rig.bluez()
            .set(&device.path, "Connected", OwnedVariant::from(false));
        let returned = Instant::now();
        rig.bluez().take_name();
        let closed = device.pods.wait_closed(1, HANG_LIMIT).unwrap();
        assert_gap(
            "the channel closed after BlueZ returned",
            returned,
            closed[0],
            &(Duration::ZERO..),
        );
        rig.wait_entered("grace", 1);

        let window = ABSENT_GRACE.saturating_sub(REAL_TIME_SLACK);
        let left = window.saturating_sub(returned.elapsed());
        assert!(
            !left.is_zero(),
            "BlueZ took the whole grace observation window to return"
        );
        rig.omarchy().never_ran(left);
        rig.assert_not_logged(
            "the observation ended inside the grace",
            "link: grace to idle",
        );
        assert_eq!(rig.dials().len(), 1);
        assert!(!connected(&rig.status()));
        rig.finish();
    }

    #[test]
    fn an_adapter_power_edge_alone_sweeps_nothing() {
        let (rig, _device) = Rig::with_linked_pods();
        let sweeps = rig.bluez().sweeps();
        rig.bluez().power(rig.adapter(), false);
        rig.bluez().power(rig.adapter(), true);
        rig.bluez().wait_signals_taken();
        rig.wait_queued_inputs_applied();
        pause(SIGNAL_SLACK);
        assert_eq!(rig.bluez().sweeps(), sweeps);
        rig.finish();
    }
}
