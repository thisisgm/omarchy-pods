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
    use zbus::OwnedVariant;

    use super::support::rig::{
        DIAL_WAIT, Device, HELD_LADDER, Rig, assert_gap, connected, not_before,
    };
    use super::support::{HANG_LIMIT, pause};

    /// The wait before the next dial once the held ladder is spent and the pods are parked.
    const PARKED_RETRY: Duration = Duration::from_secs(300);

    /// The wait in the parked phase before BlueZ reports the pods again.
    const PARKED_FOR: Duration = Duration::from_secs(5);

    /// Where `cminflt`, the minor faults of the children a process waited for, sits among the `/proc/<pid>/stat` fields after the name.
    const REAPED_FAULTS_AFTER_NAME: usize = 8;

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
}
