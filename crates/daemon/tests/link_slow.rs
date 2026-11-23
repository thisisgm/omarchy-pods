//! The held ladder and the parked wait at real time through the built daemon, minutes long, so the `slow` job runs them.

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
    use serde_json::Value;
    use zbus::OwnedVariant;

    use super::support::bluez::DeviceMethod;
    use super::support::children::{children, reaped_children_faults};
    use super::support::parked::{climb_to_parked, refused_pods};
    use super::support::rig::{CALLS_IN_FLIGHT_MAX, DIAL_WAIT, Rig, assert_gap, connected, total};
    use super::support::{HANG_LIMIT, Proto1, do_request, outcome, pause, wait_for, wait_within};

    /// The wait before the next dial once the held ladder is spent and the pods are parked.
    const PARKED_RETRY: Duration = Duration::from_secs(300);

    /// The wait in the parked phase before BlueZ reports the pods again.
    const PARKED_FOR: Duration = Duration::from_secs(5);

    /// The daemon's limit on one BlueZ method call.
    const CALL_TIMEOUT: Duration = Duration::from_secs(30);

    /// Keeps the second disconnect waiting for BlueZ long after the first one timed out.
    const DISCONNECTS_APART: Duration = Duration::from_secs(5);

    fn failures(status: &Value) -> usize {
        usize::try_from(total(status, "connect_failures_total")).unwrap()
    }

    #[test]
    #[ignore = "minutes at real time; the slow job runs it"]
    fn a_refused_device_is_parked_and_tried_again_after_five_minutes() {
        let (mut rig, device) = refused_pods();
        let watch = rig.watch();
        let mut lines = vec![
            watch
                .next(HANG_LIMIT)
                .expect("the state watch prints first")
                .state,
        ];
        rig.link_up(&device);
        climb_to_parked(&rig);
        let dials = rig.wait_dials(12, PARKED_RETRY.saturating_add(HANG_LIMIT));
        assert_gap(
            "the parked dial after the eleventh",
            dials[10].at,
            dials[11].at,
            &(PARKED_RETRY..),
        );
        let last = rig.wait_status("two failures after the twelfth dial", |status| {
            status["reconnect_failures_total"] == 2
        });
        lines.extend(watch.lines_until(&last));
        assert!(lines.iter().all(|line| !connected(line)), "{lines:#?}");
        let pid = rig.daemon().pid();
        let running = children(pid);
        assert!(running.is_empty(), "the daemon has children {running:?}");
        // A child already reaped is in no children file, but its page faults stay counted in the parent.
        assert_eq!(
            reaped_children_faults(pid),
            0,
            "the daemon ran and reaped a process"
        );
        rig.finish();
    }

    #[test]
    #[ignore = "minutes at real time; the slow job runs it"]
    fn a_parked_device_is_dialled_at_once_on_a_connected_edge() {
        let (rig, device) = refused_pods();
        rig.link_up(&device);
        climb_to_parked(&rig);
        pause(PARKED_FOR);
        rig.accept(&device);
        rig.bluez()
            .set(&device.path, "Connected", OwnedVariant::from(false));
        let edge = Instant::now();
        rig.link_up(&device);
        let dials = rig.wait_dials(12, HANG_LIMIT);
        assert_gap(
            "the dial after the connected edge",
            edge,
            dials[11].at,
            &(Duration::ZERO..=DIAL_WAIT),
        );
        rig.wait_filled();
        rig.finish();
    }

    #[test]
    #[ignore = "half a minute at real time; the slow job runs it"]
    fn held_bluez_calls_fail_after_their_timeout_and_free_their_slots() {
        let (rig, device) = Rig::with_known_pods();
        rig.bluez().hold_replies(true);
        let mut client = Proto1::connect(rig.session());
        let asked = Instant::now();
        for id in 1..=CALLS_IN_FLIGHT_MAX {
            assert_eq!(outcome(&client.ask(&do_request(id, "connect"))), "sent");
        }
        let held = vec![device.call(DeviceMethod::Connect); CALLS_IN_FLIGHT_MAX];
        wait_for("four calls at the fake BlueZ", || {
            (rig.bluez().calls() == held).then_some(())
        });
        // No call counted failed yet, so this answer came while all four hung.
        let during = rig.status();
        assert_eq!(
            failures(&during),
            0,
            "status answered while the calls hang: {during}"
        );
        let failed = wait_within(
            "the held connects counted as failed",
            CALL_TIMEOUT.saturating_add(HANG_LIMIT),
            || {
                let status = rig.status();
                (failures(&status) >= CALLS_IN_FLIGHT_MAX).then_some((Instant::now(), status))
            },
        );
        assert_gap(
            "the failures after do connect",
            asked,
            failed.0,
            &(CALL_TIMEOUT..),
        );
        assert_eq!(failures(&failed.1), CALLS_IN_FLIGHT_MAX);
        let fifth = CALLS_IN_FLIGHT_MAX + 1;
        let reply = Proto1::connect(rig.session()).ask(&do_request(fifth, "connect"));
        assert_eq!(outcome(&reply), "sent");
        let reached = vec![device.call(DeviceMethod::Connect); fifth];
        wait_for("a fifth call at the fake BlueZ", || {
            (rig.bluez().calls() == reached).then_some(())
        });
        let status = rig.status();
        assert_eq!(failures(&status), CALLS_IN_FLIGHT_MAX);
        assert!(!connected(&status));
        rig.finish();
    }

    #[test]
    #[ignore = "half a minute at real time; the slow job runs it"]
    fn a_disconnect_that_times_out_after_the_pods_came_back_leaves_a_newer_one_holding() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        rig.start();
        rig.link_up(&device);
        rig.wait_filled();
        let mut held = Vec::new();
        for id in 1..=2 {
            rig.bluez().hold_replies(true);
            let reply = Proto1::connect(rig.session()).ask(&do_request(id, "disconnect"));
            assert_eq!(outcome(&reply), "sent");
            held.push(device.call(DeviceMethod::Disconnect));
            wait_for("the held disconnect at the fake BlueZ", || {
                (rig.bluez().calls() == held).then_some(())
            });
            rig.bluez().hold_replies(false);
            if id == 1 {
                rig.link_down(&device);
                rig.wait_status("the name cleared", |status| status["device_name"] == "");
                rig.link_up(&device);
                rig.wait_filled();
                pause(DISCONNECTS_APART);
            }
        }
        wait_within(
            "the first disconnect counted as failed",
            CALL_TIMEOUT.saturating_add(HANG_LIMIT),
            || (total(&rig.status(), "disconnect_failures_total") == 1).then_some(()),
        );
        let graces = rig.entered("grace");
        rig.link_down(&device);
        let cleared = rig.wait_status("the name cleared", |status| status["device_name"] == "");
        assert_eq!(
            rig.entered("grace"),
            graces,
            "the pods left through the grace"
        );
        assert!(!connected(&cleared));
        assert_eq!(total(&cleared, "disconnect_failures_total"), 1);
        rig.finish();
    }
}
