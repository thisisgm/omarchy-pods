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
    use std::fs;
    use std::time::{Duration, Instant};

    use pods_fake::Profile;
    use pods_trap::Answer;
    use rustix::io::Errno;
    use serde_json::Value;
    use zbus::OwnedVariant;

    use super::support::bluez::DeviceMethod;
    use super::support::rig::{
        ANSWERED_WITHIN, CALLS_IN_FLIGHT_MAX, DIAL_WAIT, Device, HELD_LADDER, REAL_TIME_SLACK, Rig,
        assert_gap, connected, not_before, total,
    };
    use super::support::{HANG_LIMIT, Proto1, do_request, outcome, pause, wait_for, wait_within};

    /// The wait before the next dial once the held ladder is spent and the pods are parked.
    const PARKED_RETRY: Duration = Duration::from_secs(300);

    /// The wait in the parked phase before BlueZ reports the pods again.
    const PARKED_FOR: Duration = Duration::from_secs(5);

    /// Where `cminflt`, the minor faults of the children a process waited for, sits among the `/proc/<pid>/stat` fields after the name.
    const REAPED_FAULTS_AFTER_NAME: usize = 8;

    /// The daemon's limit on one BlueZ method call.
    const CALL_TIMEOUT: Duration = Duration::from_secs(30);

    /// The most a user disconnect may take to clear the state once the pods left, far inside the 7750 ms grace.
    const CLEARED_WITHIN: Duration = Duration::from_secs(1);

    /// Keeps the second disconnect waiting for BlueZ long after the first one timed out.
    const DISCONNECTS_APART: Duration = Duration::from_secs(5);

    /// Pods that refuse every dial, the daemon started and BlueZ not yet reporting them connected.
    fn refused_pods() -> (Rig, Device) {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        rig.start();
        rig.answer(
            &device,
            Answer::Refuse {
                after: Duration::ZERO,
                errno: Errno::CONNREFUSED,
            },
        );
        (rig, device)
    }

    fn failures(status: &Value) -> usize {
        usize::try_from(total(status, "connect_failures_total")).unwrap()
    }

    /// Waits for the eleven dials of the held ladder, each gap not before its step, then for the failure that parks the pods.
    fn climb_to_parked(rig: &Rig) {
        let mut dials = rig.wait_dials(1, HANG_LIMIT);
        for delay in HELD_LADDER {
            let before = dials.last().unwrap().at;
            dials = rig.wait_dials(
                dials.len().saturating_add(1),
                delay.saturating_add(HANG_LIMIT),
            );
            let what = format!("held dial {} after the one before", dials.len());
            assert_gap(&what, before, dials.last().unwrap().at, &not_before(delay));
        }
        rig.wait_status("one failure after the eleventh dial", |status| {
            status["reconnect_failures_total"] == 1
        });
    }

    /// The minor faults of every child `pid` waited for, above zero once it ran any program; sample input `4242 (librepods) S 4200 4242 4200 0 -1 4194560 1534 0 0 0 3 1 0 0 20 0 9 0 81234`.
    fn reaped_children_faults(pid: i32) -> u64 {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        // The name may hold spaces and parentheses, so the fields are counted from its last closing parenthesis.
        let (_, after_name) = stat
            .rsplit_once(')')
            .unwrap_or_else(|| panic!("no name in {stat:?}"));
        let field = after_name
            .split_whitespace()
            .nth(REAPED_FAULTS_AFTER_NAME)
            .unwrap_or_else(|| panic!("no cminflt in {stat:?}"));
        field
            .parse()
            .unwrap_or_else(|error| panic!("cminflt {field:?} in {stat:?}: {error}"))
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
            &not_before(PARKED_RETRY),
        );
        let last = rig.wait_status("two failures after the twelfth dial", |status| {
            status["reconnect_failures_total"] == 2
        });
        lines.extend(watch.lines_until(&last));
        assert!(lines.iter().all(|line| !connected(line)), "{lines:#?}");
        let pid = rig.daemon().pid().as_raw_nonzero();
        // Each thread lists only the processes it started, so every thread is read.
        for task in fs::read_dir(format!("/proc/{pid}/task")).unwrap() {
            let path = task.unwrap().path().join("children");
            // Sample input: `4242 4243 `, the pids of the thread's children, each followed by a space.
            let children = match fs::read_to_string(&path) {
                Ok(children) => children,
                // A thread that ended after the listing has nothing left to list.
                Err(error)
                    if matches!(
                        Errno::from_io_error(&error),
                        Some(Errno::NOENT | Errno::SRCH)
                    ) =>
                {
                    continue;
                }
                Err(error) => panic!("{}: {error}", path.display()),
            };
            assert_eq!(children.trim(), "", "{} lists processes", path.display());
        }
        // A child already reaped is in no children file, but its page faults stay counted in the parent.
        assert_eq!(
            reaped_children_faults(pid.get()),
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
        let failed = wait_within(
            "the held connects counted as failed",
            CALL_TIMEOUT.saturating_add(HANG_LIMIT),
            || {
                let started = Instant::now();
                let status = rig.status();
                assert_gap(
                    "librepods status while the calls hang",
                    started,
                    Instant::now(),
                    &(Duration::ZERO..=ANSWERED_WITHIN),
                );
                (failures(&status) >= CALLS_IN_FLIGHT_MAX).then_some((Instant::now(), status))
            },
        );
        assert_gap(
            "the failures after do connect",
            asked,
            failed.0,
            &(CALL_TIMEOUT..=CALL_TIMEOUT.saturating_add(REAL_TIME_SLACK)),
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
        let left = Instant::now();
        rig.link_down(&device);
        let cleared = rig.wait_status("the name cleared", |status| status["device_name"] == "");
        assert_gap(
            "the name cleared after the pods left",
            left,
            Instant::now(),
            &(Duration::ZERO..=CLEARED_WITHIN),
        );
        assert!(!connected(&cleared));
        assert_eq!(total(&cleared, "disconnect_failures_total"), 1);
        rig.finish();
    }
}
