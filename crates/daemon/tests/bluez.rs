//! The BlueZ watcher through the built daemon: the fake BlueZ on a private system bus reports the pods, the trap answers the dial and the fake pods serve it.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "these tests drive the daemon through the rig, so the socket helpers the other suites share go unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs::OpenOptions;
    use std::future::poll_fn;
    use std::io::Write;
    use std::pin::Pin;
    use std::time::{Duration, Instant};

    use pods_core::link::ladder::HELD_DELAYS_MS;
    use pods_fake::Profile;
    use pods_proto::packets::CONTROL_PSM;
    use pods_trap::Answer;
    use rustix::io::Errno;
    use serde_json::Value;
    use tokio::runtime::{Builder as RuntimeBuilder, Runtime};
    use tokio::time::timeout;
    use zbus::connection::Builder;
    use zbus::export::futures_core::Stream;
    use zbus::message::Type;
    use zbus::{Connection, MessageStream, OwnedObjectPath, OwnedVariant, Variant};

    use super::support::bluez::{AAP_UUID, DEVICE_INTERFACE, FakeBluez};
    use super::support::bus::start_bus;
    use super::support::rig::{DIAL_WAIT, REAL_TIME_SLACK, Rig, SIGNAL_SLACK, test_address};
    use super::support::{HANG_LIMIT, Session, pause, wait_for, wait_within};

    /// `test_address(1)` as BlueZ writes it.
    const FIRST_ADDRESS: &str = "02:00:00:00:00:01";
    const HEADSET_UUID: &str = "0000111e-0000-1000-8000-00805f9b34fb";

    /// `SWEEP_TIMEOUT_MS` of `src/daemon/bluez.rs`.
    const SWEEP_TIMEOUT: Duration = Duration::from_millis(2000);

    /// `SYSTEM_BUS_RETRY_MS` of `src/daemon/bluez.rs`.
    const SYSTEM_BUS_RETRY: Duration = Duration::from_millis(5000);

    /// `SWEEP_RETRY_MS` of `src/daemon/bluez.rs`.
    const SWEEP_RETRY: Duration = Duration::from_millis(1000);

    /// A bus monitor that saw no message for this long has seen every one sent before it.
    const MONITOR_QUIET: Duration = Duration::from_millis(100);

    const SWEEP_CALLS: &str = "type='method_call',interface='org.freedesktop.DBus.ObjectManager',member='GetManagedObjects'";

    /// How the daemon's line about a sweep that failed starts, from `src/daemon/bluez.rs`.
    const SWEEP_FAILED: &str = "bluez: cannot read BlueZ's devices:";

    /// How that line goes on when BlueZ never answered, from `reply` in `src/daemon/bluez.rs`.
    const SWEEP_HELD: &str = "no answer within";

    /// The daemon's line about a signal it could not read, from `body` in `src/daemon/bluez.rs`.
    const SIGNAL_UNREAD: &str = "bluez: cannot read a PropertiesChanged signal of signature (su)";

    /// Words every line of the daemon about the system bus holds.
    const SYSTEM_BUS_LINE: &str = "system bus";

    /// The most a `status` may take while BlueZ answers nothing.
    const STATUS_LIMIT: Duration = Duration::from_millis(200);

    /// Asks of `status` a `STATUS_LIMIT` apart, spanning two sweep timeouts.
    const STATUS_ASKS: u32 = 20;

    /// Longer than a sweep that times out and the dial that would follow it.
    const QUIET: Duration = SWEEP_TIMEOUT.saturating_add(REAL_TIME_SLACK);

    /// The most the daemon may take to close the channel after BlueZ reports the link down.
    const CLOSE_WAIT: Duration = Duration::from_secs(1);

    /// The most a dial may come after a `Connected` true edge.
    const EDGE_TO_DIAL: Duration = Duration::from_millis(500);

    /// When the system bus starts, inside the daemon's first wait to reconnect.
    const SYSTEM_BUS_LATE: Duration = Duration::from_secs(2);

    const DEVICE_CHANGES: &str = "type='signal',sender='org.bluez',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',arg0='org.bluez.Device1'";

    type Managed = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedVariant>>>;

    async fn managed_objects(client: &Connection) -> Managed {
        client
            .call_method(
                Some("org.bluez"),
                "/",
                Some("org.freedesktop.DBus.ObjectManager"),
                "GetManagedObjects",
                &(),
            )
            .await
            .unwrap()
            .body()
            .deserialize()
            .unwrap()
    }

    fn in_time<T>(runtime: &Runtime, work: impl Future<Output = T>) -> T {
        let finished = runtime.block_on(async { timeout(HANG_LIMIT, work).await });
        finished.unwrap_or_else(|_| panic!("the bus client still waiting after {HANG_LIMIT:?}"))
    }

    /// A monitor of the bus at `address` that keeps every message matching `rule`.
    fn monitor(runtime: &Runtime, address: &str, rule: &str) -> MessageStream {
        in_time(runtime, async {
            let monitor = Builder::address(address).build().await.unwrap();
            monitor
                .call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus.Monitoring"),
                    "BecomeMonitor",
                    &(vec![rule], 0_u32),
                )
                .await
                .unwrap();
            MessageStream::from(monitor)
        })
    }

    /// The method calls the monitor kept so far; the bus also sends it the loss of its own name.
    fn monitored(runtime: &Runtime, messages: &mut MessageStream) -> usize {
        let mut count: usize = 0;
        loop {
            let next = poll_fn(|context| Pin::new(&mut *messages).poll_next(context));
            match runtime.block_on(async { timeout(MONITOR_QUIET, next).await }) {
                Ok(Some(message)) => {
                    if message.unwrap().message_type() == Type::MethodCall {
                        count = count.saturating_add(1);
                    }
                }
                Ok(None) | Err(_) => return count,
            }
        }
    }

    #[test]
    fn the_fake_bluez_serves_its_objects_to_a_client() {
        let session = Session::new();
        let bus = start_bus(&session, "system");
        let bluez = FakeBluez::start(&bus.address, true);
        let adapter = bluez.add_adapter();
        let path = bluez.add_device(&adapter, test_address(1), &[AAP_UUID], false);
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (client, mut changes) = in_time(&runtime, async {
            let client = Builder::address(bus.address.as_str())
                .build()
                .await
                .unwrap();
            let objects = managed_objects(&client).await;
            let adapter_path = OwnedObjectPath::try_from(adapter.0.as_str()).unwrap();
            assert!(objects[&adapter_path].contains_key("org.bluez.Adapter1"));
            let device_path = OwnedObjectPath::try_from(path.0.as_str()).unwrap();
            let device = &objects[&device_path][DEVICE_INTERFACE];
            let address = String::try_from(device["Address"].try_clone().unwrap()).unwrap();
            assert_eq!(address, FIRST_ADDRESS);
            assert!(!bool::try_from(&device["Connected"]).unwrap());
            let uuids = Vec::<String>::try_from(device["UUIDs"].try_clone().unwrap()).unwrap();
            assert_eq!(uuids, [AAP_UUID]);
            let changes = MessageStream::for_match_rule(DEVICE_CHANGES, &client, None)
                .await
                .unwrap();
            (client, changes)
        });
        bluez.set(&path, "Connected", OwnedVariant::from(true));
        let next = poll_fn(|context| Pin::new(&mut changes).poll_next(context));
        let message = in_time(&runtime, next).unwrap().unwrap();
        assert_eq!(message.header().path().unwrap().as_str(), path.0);
        let (interface, values, _): (String, HashMap<String, OwnedVariant>, Vec<String>) =
            message.body().deserialize().unwrap();
        assert_eq!(interface, DEVICE_INTERFACE);
        assert!(bool::try_from(&values["Connected"]).unwrap());
        let objects = in_time(&runtime, managed_objects(&client));
        let device_path = OwnedObjectPath::try_from(path.0.as_str()).unwrap();
        assert!(bool::try_from(&objects[&device_path][DEVICE_INTERFACE]["Connected"]).unwrap());
        bluez.finish();
    }

    #[test]
    #[should_panic(expected = "the fake BlueZ stopped serving before the end of the test")]
    fn a_fake_whose_bus_closed_fails_its_test() {
        let session = Session::new();
        let bus = start_bus(&session, "system");
        let bluez = FakeBluez::start(&bus.address, true);
        drop(bus);
        wait_within("the fake to notice its bus closed", HANG_LIMIT, || {
            bluez.stopped_serving().then_some(())
        });
        bluez.finish();
    }

    fn connected(status: &Value) -> bool {
        status["connected"].as_bool().unwrap()
    }

    fn uuids(list: &[&str]) -> OwnedVariant {
        let list: Vec<String> = list.iter().map(|uuid| (*uuid).to_owned()).collect();
        OwnedVariant::try_from(Variant::from(list)).unwrap()
    }

    /// Asks `librepods status` for `duration`, failing at the first answer where `check` does not hold.
    fn holds_for(rig: &Rig, duration: Duration, what: &str, check: impl Fn(&Value) -> bool) {
        let started = Instant::now();
        while started.elapsed() < duration {
            let status = rig.status();
            assert!(
                check(&status),
                "{what} ended after {:?}: {status}",
                started.elapsed()
            );
        }
    }

    fn up(rig: &Rig) {
        rig.wait_status("connected true", connected);
    }

    /// BlueZ leaves the bus and comes back, which starts a sweep.
    fn bluez_restarts(rig: &Rig) {
        rig.bluez().drop_name();
        rig.bluez().take_name();
    }

    #[test]
    #[should_panic(expected = "the daemon logged a device address")]
    fn the_finish_check_catches_an_address_in_the_log() {
        let mut rig = Rig::new();
        rig.start();
        let mut log = OpenOptions::new()
            .append(true)
            .open(&rig.daemon().log)
            .unwrap();
        writeln!(log, "{FIRST_ADDRESS}").unwrap();
        rig.finish();
    }

    #[test]
    fn a_connected_candidate_is_dialled() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        rig.start();
        rig.link_up(&device);
        let dials = rig.wait_dials(1, DIAL_WAIT);
        assert_eq!(dials.len(), 1, "{dials:?}");
        assert_eq!(
            (dials[0].device, dials[0].psm),
            (device.address, CONTROL_PSM)
        );
        up(&rig);
        rig.finish();
    }

    #[test]
    fn a_connected_device_without_the_uuid_opens_no_bluetooth_socket() {
        let mut rig = Rig::new();
        let pods = rig.add_pods(Profile::default());
        rig.bluez()
            .add_device(rig.adapter(), test_address(2), &[HEADSET_UUID], true);
        rig.start();
        rig.no_dial_for(QUIET);
        assert_eq!(rig.sockets(), 0);
        assert!(!connected(&rig.status()));
        // Only the sweep made the pods a candidate, so their dial shows it read the device list.
        rig.link_up(&pods);
        let dials = rig.wait_dials(1, DIAL_WAIT);
        up(&rig);
        assert!(
            dials.iter().all(|dial| dial.device == pods.address),
            "{dials:?}"
        );
        rig.finish();
    }

    #[test]
    fn a_device_already_connected_at_start_is_adopted() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        rig.link_up(&device);
        rig.start();
        rig.wait_dials(1, DIAL_WAIT);
        let name = Profile::default().name;
        rig.wait_status("the profile's name", |status| {
            status["device_name"] == name.as_str()
        });
        rig.finish();
    }

    #[test]
    fn freshly_paired_pods_are_dialled_when_their_uuids_arrive() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        rig.bluez().set(&device.path, "UUIDs", uuids(&[]));
        rig.start();
        rig.link_up(&device);
        rig.no_dial_for(QUIET);
        rig.bluez().set(&device.path, "UUIDs", uuids(&[AAP_UUID]));
        rig.wait_dials(1, DIAL_WAIT);
        up(&rig);
        rig.finish();
    }

    #[test]
    fn daemon_started_before_bluez_adopts_the_device_when_it_appears() {
        let mut rig = Rig::before_bluez();
        let device = rig.add_pods(Profile::default());
        rig.link_up(&device);
        rig.start();
        rig.no_dial_for(QUIET);
        rig.bluez().take_name();
        rig.wait_dials(1, DIAL_WAIT);
        up(&rig);
        rig.finish();
    }

    #[test]
    fn bluez_restart_with_the_link_up_keeps_it() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        rig.start();
        rig.link_up(&device);
        rig.wait_dials(1, DIAL_WAIT);
        up(&rig);
        rig.bluez().drop_name();
        holds_for(&rig, QUIET, "connected", connected);
        rig.bluez().take_name();
        rig.no_dial_for(QUIET);
        assert_eq!(rig.dials().len(), 1);
        let closed = device.pods.wait_closed(1, SIGNAL_SLACK);
        assert!(closed.is_err(), "the channel closed: {closed:?}");
        // The watcher still follows BlueZ after its return: the link falling closes the channel.
        rig.bluez()
            .set(&device.path, "Connected", OwnedVariant::from(false));
        device.pods.wait_closed(1, CLOSE_WAIT).unwrap();
        rig.finish();
    }

    #[test]
    fn bluez_restart_while_refused_stops_the_dials_until_it_returns() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        rig.start();
        let refuse = Answer::Refuse {
            after: Duration::ZERO,
            errno: Errno::CONNREFUSED,
        };
        rig.answer(&device, refuse);
        rig.link_up(&device);
        let first_retry = Duration::from_millis(HELD_DELAYS_MS[0]);
        rig.wait_dials(2, DIAL_WAIT + first_retry);
        rig.bluez().drop_name();
        pause(SIGNAL_SLACK);
        let next_retry = Duration::from_millis(HELD_DELAYS_MS[1]);
        rig.no_dial_for(next_retry + QUIET);
        rig.accept(&device);
        let returned = Instant::now();
        rig.bluez().take_name();
        let dials = rig.wait_dials(3, DIAL_WAIT + SWEEP_TIMEOUT);
        assert!(
            dials[2].at >= returned,
            "the third dial came before BlueZ returned: {dials:?}"
        );
        up(&rig);
        rig.finish();
    }

    #[test]
    fn a_connected_edge_brings_a_new_dial_within_500_ms() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        rig.start();
        rig.link_up(&device);
        rig.wait_dials(1, DIAL_WAIT);
        up(&rig);
        rig.bluez()
            .set(&device.path, "Connected", OwnedVariant::from(false));
        device.pods.wait_closed(1, CLOSE_WAIT).unwrap();
        rig.no_dial_for(QUIET);
        let edge = Instant::now();
        rig.bluez()
            .set(&device.path, "Connected", OwnedVariant::from(true));
        let dials = rig.wait_dials(2, EDGE_TO_DIAL);
        let took = dials[1]
            .at
            .checked_duration_since(edge)
            .unwrap_or_else(|| panic!("the second dial came before the edge: {dials:?}"));
        assert!(
            took <= EDGE_TO_DIAL,
            "the second dial came {took:?} after the edge, wanted 0 to {EDGE_TO_DIAL:?}"
        );
        up(&rig);
        rig.finish();
    }

    #[test]
    fn pods_known_to_two_adapters_stay_up_while_either_is_connected() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        let second = rig.bluez().add_adapter();
        let other = rig
            .bluez()
            .add_device(&second, device.address, &[AAP_UUID], false);
        rig.start();
        rig.link_up(&device);
        rig.wait_dials(1, DIAL_WAIT);
        up(&rig);
        rig.bluez()
            .set(&other, "Connected", OwnedVariant::from(true));
        rig.bluez()
            .set(&other, "Connected", OwnedVariant::from(false));
        holds_for(&rig, QUIET, "connected", connected);
        let closed = device.pods.wait_closed(1, Duration::ZERO);
        assert!(closed.is_err(), "the channel closed: {closed:?}");
        rig.bluez()
            .set(&other, "Connected", OwnedVariant::from(true));
        rig.bluez()
            .set(&device.path, "Connected", OwnedVariant::from(false));
        holds_for(&rig, QUIET, "connected", connected);
        let closed = device.pods.wait_closed(1, Duration::ZERO);
        assert!(closed.is_err(), "the channel closed: {closed:?}");
        rig.bluez()
            .set(&other, "Connected", OwnedVariant::from(false));
        device.pods.wait_closed(1, CLOSE_WAIT).unwrap();
        rig.finish();
    }

    #[test]
    fn no_system_bus_leaves_the_socket_serving() {
        let mut rig = Rig::before_system_bus();
        rig.start();
        assert!(!connected(&rig.status()));
        pause(SYSTEM_BUS_RETRY + REAL_TIME_SLACK);
        assert!(!connected(&rig.status()));
        let log = rig.daemon().log();
        let about_the_bus = log.lines().filter(|line| line.contains(SYSTEM_BUS_LINE));
        assert_eq!(about_the_bus.count(), 1, "{log}");
        rig.finish();
    }

    #[test]
    fn the_system_bus_is_retried_after_5_s() {
        let mut rig = Rig::before_system_bus();
        let started = Instant::now();
        rig.start();
        pause(SYSTEM_BUS_LATE.saturating_sub(started.elapsed()));
        rig.start_system_bus();
        let device = rig.add_pods(Profile::default());
        rig.link_up(&device);
        let latest = SYSTEM_BUS_RETRY + REAL_TIME_SLACK + DIAL_WAIT;
        let dials = rig.wait_dials(1, latest);
        let after = dials[0].at.saturating_duration_since(started);
        assert!(
            after >= SYSTEM_BUS_RETRY && after <= latest,
            "the dial came {after:?} after the start, wanted {SYSTEM_BUS_RETRY:?} to {latest:?}"
        );
        rig.finish();
    }

    #[test]
    fn a_wedged_bluez_does_not_hold_the_daemon() {
        let mut rig = Rig::new();
        rig.bluez().hold_replies(true);
        rig.start();
        let mut slowest = Duration::ZERO;
        for _ in 0..STATUS_ASKS {
            let asked = Instant::now();
            rig.status();
            slowest = slowest.max(asked.elapsed());
            pause(STATUS_LIMIT);
        }
        assert!(
            slowest <= STATUS_LIMIT,
            "the slowest status took {slowest:?}, wanted 0 to {STATUS_LIMIT:?}"
        );
        let log = rig.daemon().log();
        assert!(log.contains(SWEEP_HELD), "no sweep was held: {log}");
        rig.bluez().hold_replies(false);
        let device = rig.add_pods(Profile::default());
        rig.link_up(&device);
        rig.wait_dials(1, SWEEP_TIMEOUT + DIAL_WAIT);
        rig.finish();
    }

    #[test]
    fn a_sweep_error_is_logged_once_until_it_changes() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        rig.link_up(&device);
        rig.bluez().fail_calls(true);
        rig.start();
        wait_for("the sweep error", || {
            rig.daemon().log().contains(SWEEP_FAILED).then_some(())
        });
        let swept = rig.bluez().sweeps().len();
        bluez_restarts(&rig);
        // The repeat must reach the fake before the hold, or it is held instead of failing again.
        wait_for("the repeated sweep", || {
            (rig.bluez().sweeps().len() > swept).then_some(())
        });
        rig.bluez().hold_replies(true);
        bluez_restarts(&rig);
        wait_for("the held sweep's error", || {
            rig.daemon().log().contains(SWEEP_HELD).then_some(())
        });
        rig.bluez().hold_replies(false);
        rig.bluez().fail_calls(false);
        bluez_restarts(&rig);
        // Only a sweep adopts pods connected before the start, and sweeps run in order, so every one before it has run.
        rig.wait_dials(1, SWEEP_TIMEOUT + DIAL_WAIT);
        let log = rig.daemon().log();
        assert_eq!(log.matches(SWEEP_FAILED).count(), 2, "{log}");
        up(&rig);
        rig.finish();
    }

    #[test]
    fn a_signal_the_watcher_cannot_read_is_logged_and_the_link_stays() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        rig.start();
        rig.link_up(&device);
        up(&rig);
        rig.bluez().emit_unreadable(&device.path);
        wait_for("the unreadable signal's line", || {
            rig.daemon().log().contains(SIGNAL_UNREAD).then_some(())
        });
        up(&rig);
        rig.finish();
    }

    #[test]
    fn pods_added_just_before_bluez_leaves_are_not_left_up() {
        let mut rig = Rig::new();
        let pods = rig.add_pods(Profile::default());
        rig.bluez().remove(&pods.path);
        rig.start();
        wait_for("the first sweep", || {
            (!rig.bluez().sweeps().is_empty()).then_some(())
        });
        rig.bluez().hold_replies(true);
        bluez_restarts(&rig);
        wait_for("the held sweep", || {
            (rig.bluez().sweeps().len() > 1).then_some(())
        });
        rig.bluez()
            .add_device(rig.adapter(), pods.address, &[AAP_UUID], true);
        rig.bluez().drop_name();
        rig.bluez().hold_replies(false);
        wait_for("the held sweep's error", || {
            rig.daemon().log().contains(SWEEP_HELD).then_some(())
        });
        holds_for(&rig, QUIET, "not connected", |status| !connected(status));
        rig.finish();
    }

    #[test]
    fn a_failed_sweep_is_tried_again_until_it_finds_pods_already_connected() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        rig.link_up(&device);
        rig.bluez().fail_calls(true);
        rig.start();
        // A third sweep is sent only after the second one's failure was handled.
        let sweeps = wait_for("a third failed sweep", || {
            let sweeps = rig.bluez().sweeps();
            (sweeps.len() > 2).then_some(sweeps)
        });
        let gap = sweeps[1].duration_since(sweeps[0]);
        let latest = SWEEP_RETRY + REAL_TIME_SLACK;
        assert!(
            gap >= SWEEP_RETRY && gap <= latest,
            "the second sweep came {gap:?} after the first, wanted {SWEEP_RETRY:?} to {latest:?}"
        );
        let log = rig.daemon().log();
        assert_eq!(log.matches(SWEEP_FAILED).count(), 1, "{log}");
        rig.bluez().fail_calls(false);
        rig.wait_dials(1, SWEEP_RETRY + REAL_TIME_SLACK + DIAL_WAIT);
        up(&rig);
        rig.finish();
    }

    #[test]
    fn a_sweep_failed_with_bluez_off_the_bus_is_not_tried_again() {
        let mut rig = Rig::before_bluez();
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut sweeps = monitor(&runtime, &rig.system_bus_address(), SWEEP_CALLS);
        rig.start();
        wait_for("the failed sweep", || {
            rig.daemon().log().contains(SWEEP_FAILED).then_some(())
        });
        pause(SWEEP_RETRY + REAL_TIME_SLACK);
        assert_eq!(monitored(&runtime, &mut sweeps), 1);
        rig.finish();
    }

    #[test]
    fn removed_pods_close_and_the_sweep_adopts_the_next_connected_pods() {
        let mut rig = Rig::new();
        let first = rig.add_pods(Profile::default());
        let second = rig.add_pods(Profile::default());
        rig.start();
        rig.link_up(&first);
        rig.wait_dials(1, DIAL_WAIT);
        up(&rig);
        rig.link_up(&second);
        rig.no_dial_for(SIGNAL_SLACK);
        rig.bluez().remove(&first.path);
        first.pods.wait_closed(1, CLOSE_WAIT).unwrap();
        let dials = rig.wait_dials(2, DIAL_WAIT);
        assert_eq!(dials[1].device, second.address);
        up(&rig);
        rig.finish();
    }
}
