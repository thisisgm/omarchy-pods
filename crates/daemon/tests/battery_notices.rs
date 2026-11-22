//! The low battery notice through the built daemon: the fake pods report their batteries, and the stand-in `omarchy` on the session's path records what the daemon ran.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use pods_fake::Profile;
    use pods_samples::{CASE, Cell, RIGHT};

    use super::support::children::children;
    use super::support::omarchy::{DISCONNECT_NOTICE, Mood, RECORDED_WITHIN, low_battery_notice};
    use super::support::rig::{Device, Rig};
    use super::support::{HANG_LIMIT, wait_for};

    /// A level above any percent, which the pods' report cannot mean.
    const PAST_FULL: u8 = 101;

    fn draining(level: u8) -> Cell {
        Cell {
            level,
            charging: false,
        }
    }

    fn charging(level: u8) -> Cell {
        Cell {
            level,
            charging: true,
        }
    }

    /// Reports `left` with the other two as the burst left them, then waits for the status to show it, which the daemon writes after its notices.
    fn report_left(rig: &Rig, device: &Device, left: Cell) {
        device.pods.set_battery(left, RIGHT, CASE);
        rig.wait_status(&format!("the left bud at {left:?}"), |status| {
            status["left"]["level"] == left.level && status["left"]["charging"] == left.charging
        });
    }

    #[test]
    fn a_bud_at_the_trigger_sends_one_notice_and_the_next_report_none() {
        let (rig, device) = Rig::with_linked_pods();
        device.pods.set_battery(draining(10), RIGHT, CASE);
        let calls = rig.omarchy().wait_calls(1, HANG_LIMIT);
        assert_eq!(calls, [low_battery_notice("Left", 10)]);
        report_left(&rig, &device, draining(9));
        rig.omarchy().none_for(RECORDED_WITHIN);
        rig.finish();
    }

    #[test]
    fn a_bud_at_eleven_sends_none() {
        let (rig, device) = Rig::with_linked_pods();
        report_left(&rig, &device, draining(11));
        rig.omarchy().never_ran(RECORDED_WITHIN);
        rig.finish();
    }

    #[test]
    fn charging_clears_and_the_next_low_report_sends_again() {
        let (rig, device) = Rig::with_linked_pods();
        device.pods.set_battery(draining(10), RIGHT, CASE);
        rig.omarchy().wait_calls(1, HANG_LIMIT);
        report_left(&rig, &device, charging(10));
        rig.omarchy().none_for(RECORDED_WITHIN);
        let low = low_battery_notice("Left", 10);
        assert_eq!(rig.omarchy().calls(), std::slice::from_ref(&low));
        device.pods.set_battery(draining(10), RIGHT, CASE);
        let calls = rig.omarchy().wait_calls(2, HANG_LIMIT);
        assert_eq!(calls, [low.clone(), low]);
        rig.finish();
    }

    #[test]
    fn rising_to_the_reset_rearms() {
        let (rig, device) = Rig::with_linked_pods();
        report_left(&rig, &device, draining(10));
        report_left(&rig, &device, draining(15));
        device.pods.set_battery(draining(10), RIGHT, CASE);
        let calls = rig.omarchy().wait_calls(2, HANG_LIMIT);
        let low = low_battery_notice("Left", 10);
        assert_eq!(calls, [low.clone(), low]);
        rig.finish();
    }

    #[test]
    fn fourteen_does_not_rearm() {
        let (rig, device) = Rig::with_linked_pods();
        device.pods.set_battery(draining(10), RIGHT, CASE);
        rig.omarchy().wait_calls(1, HANG_LIMIT);
        report_left(&rig, &device, draining(14));
        report_left(&rig, &device, draining(10));
        rig.omarchy().none_for(RECORDED_WITHIN);
        assert_eq!(rig.omarchy().calls(), [low_battery_notice("Left", 10)]);
        rig.finish();
    }

    #[test]
    fn left_right_and_case_latch_apart() {
        let (rig, device) = Rig::with_linked_pods();
        device
            .pods
            .set_battery(draining(10), draining(9), draining(8));
        let calls = rig.omarchy().wait_calls(3, HANG_LIMIT);
        let all_three = [
            low_battery_notice("Left", 10),
            low_battery_notice("Right", 9),
            low_battery_notice("Case", 8),
        ];
        assert_eq!(calls, all_three);
        device
            .pods
            .set_battery(draining(15), draining(9), draining(8));
        rig.wait_status("the left bud back at the reset", |status| {
            status["left"]["level"] == 15
        });
        device
            .pods
            .set_battery(draining(10), draining(9), draining(8));
        let calls = rig.omarchy().wait_calls(4, HANG_LIMIT);
        assert_eq!(calls[3..], [low_battery_notice("Left", 10)]);
        rig.omarchy().none_for(RECORDED_WITHIN);
        rig.finish();
    }

    #[test]
    fn a_reconnect_at_the_same_level_is_silent() {
        let (rig, device) = Rig::with_linked_pods();
        device.pods.set_battery(draining(10), RIGHT, CASE);
        rig.omarchy().wait_calls(1, HANG_LIMIT);
        rig.disconnect();
        assert_eq!(rig.status()["left"]["available"], false);
        rig.omarchy().wait_calls(2, HANG_LIMIT);
        rig.relink(&device);
        assert_eq!(rig.status()["left"]["level"], 10);
        rig.omarchy().none_for(RECORDED_WITHIN);
        let calls = rig.omarchy().calls();
        assert_eq!(
            calls,
            [
                low_battery_notice("Left", 10),
                DISCONNECT_NOTICE.map(str::to_owned).to_vec()
            ]
        );
        rig.finish();
    }

    #[test]
    fn a_level_above_100_is_unknown_and_sends_none() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile {
            left: draining(PAST_FULL),
            ..Profile::default()
        });
        rig.start();
        rig.link_up(&device);
        let status = rig.wait_filled();
        assert_eq!(status["left"]["available"], false, "{status}");
        rig.omarchy().never_ran(RECORDED_WITHIN);
        rig.finish();
    }

    #[test]
    fn notices_come_one_at_a_time() {
        let (mut rig, device) = Rig::with_linked_pods();
        rig.omarchy().become_(Mood::Slow);
        let pid = rig.daemon().pid();
        device
            .pods
            .set_battery(draining(10), draining(9), draining(8));
        let mut most_alive = 0;
        let calls = wait_for("three runs, the last one reaped", || {
            most_alive = most_alive.max(children(pid).len());
            let calls = rig.omarchy().calls();
            (calls.len() == 3 && children(pid).is_empty()).then_some(calls)
        });
        assert_eq!(most_alive, 1, "runs alive together");
        let all_three = [
            low_battery_notice("Left", 10),
            low_battery_notice("Right", 9),
            low_battery_notice("Case", 8),
        ];
        assert_eq!(calls, all_three);
        rig.finish();
    }
}
