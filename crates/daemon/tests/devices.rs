//! `connect`, `disconnect` and `forget` through the built daemon: the BlueZ call each makes, and what the user then reads.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::process::Output;
    use std::time::{Duration, Instant};

    use pods_fake::Profile;
    use pods_trap::Answer;
    use rustix::io::Errno;
    use tokio::runtime::Builder as RuntimeBuilder;
    use tokio::time::timeout;
    use zbus::connection::Builder;

    use super::support::bluez::{Call, DeviceMethod};
    use super::support::bus::{BUS_NAME, INTERFACE, OBJECT_PATH};
    use super::support::rig::{
        CALLS_IN_FLIGHT_MAX, DIAL_WAIT, Device, Rig, assert_gap, connected, total,
    };
    use super::support::{HANG_LIMIT, Proto1, do_request, outcome, pause, wait_for};

    const NOT_CONNECTED: &str = "The AirPods are not connected.";
    const NO_DEVICE: &str = "There are no AirPods to act on.";

    /// Long enough for a call the daemon wrongly makes to reach the fake BlueZ.
    const QUIET: Duration = Duration::from_millis(300);

    /// Refused dials before `do connect`, each ending in a backoff the next held step is seconds away from.
    const REFUSED_DIALS: usize = 4;

    const STATUS_CALLS: usize = 10;
    const STATUS_APART: Duration = Duration::from_millis(200);

    /// The daemon with one pair of pods, their link up and their name in the status.
    fn up() -> (Rig, Device) {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        rig.start();
        rig.link_up(&device);
        rig.wait_filled();
        (rig, device)
    }

    /// Fails unless `output` is an exit 1 with `message` on stderr and nothing on stdout.
    fn assert_refused(output: &Output, message: &str) {
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(output.stdout, b"", "{output:?}");
        assert_eq!(
            String::from_utf8(output.stderr.clone()).unwrap(),
            format!("{message}\n")
        );
    }

    /// Fails unless `output` is an exit 0 with nothing on stdout or stderr.
    fn assert_taken(output: &Output) {
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"", "{output:?}");
        assert_eq!(output.stderr, b"", "{output:?}");
    }

    /// The error name and message the daemon's D-Bus interface refuses `method` with, failing if it takes it.
    fn dbus_refusal(rig: &Rig, method: &str) -> (String, Option<String>) {
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let client = timeout(HANG_LIMIT, Builder::address(rig.session_bus()).build())
                .await
                .expect("the session bus within the hang limit")
                .unwrap();
            let call =
                client.call_method(Some(BUS_NAME), OBJECT_PATH, Some(INTERFACE), method, &());
            match timeout(HANG_LIMIT, call).await.expect("a D-Bus reply") {
                Err(zbus::Error::MethodError(name, message, _)) => {
                    (name.as_str().to_owned(), message)
                }
                other => panic!("{method} was not refused: {other:?}"),
            }
        })
    }

    #[test]
    fn do_disconnect_calls_bluez_and_resets_without_the_grace() {
        let (mut rig, device) = up();
        assert_taken(&rig.client(&["do", "disconnect"]));
        let cleared = rig.wait_status("the name cleared", |status| status["device_name"] == "");
        assert_eq!(
            rig.entered("grace"),
            0,
            "the disconnect went through the grace"
        );
        assert!(!connected(&cleared));
        assert_eq!(rig.bluez().calls(), [device.call(DeviceMethod::Disconnect)]);
        assert_eq!(total(&cleared, "disconnect_calls_total"), 1);
        assert_refused(&rig.client(&["do", "disconnect"]), NOT_CONNECTED);
        pause(QUIET);
        assert_eq!(rig.bluez().calls().len(), 1);
        rig.finish();
    }

    #[test]
    fn do_connect_with_the_link_up_is_confirmed_without_a_call() {
        let (rig, _device) = up();
        let mut client = Proto1::connect(rig.session());
        assert_eq!(outcome(&client.ask(&do_request(1, "connect"))), "confirmed");
        pause(QUIET);
        assert_eq!(rig.bluez().calls(), []);
        assert_eq!(rig.dials().len(), 1);
        rig.finish();
    }

    #[test]
    fn do_connect_on_a_refused_device_dials_at_once() {
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
        rig.link_up(&device);
        rig.wait_dials(REFUSED_DIALS, HANG_LIMIT);
        rig.wait_entered("backoff", REFUSED_DIALS);
        rig.accept(&device);
        let mut client = Proto1::connect(rig.session());
        let asked = Instant::now();
        assert_eq!(outcome(&client.ask(&do_request(1, "connect"))), "sent");
        let dials = rig.wait_dials(5, HANG_LIMIT);
        assert_gap(
            "the dial after do connect",
            asked,
            dials[4].at,
            &(Duration::ZERO..=DIAL_WAIT),
        );
        assert_eq!(rig.bluez().calls(), []);
        rig.wait_filled();
        rig.finish();
    }

    #[test]
    fn do_connect_with_no_link_calls_bluez_and_the_state_fills() {
        let (rig, device) = Rig::with_known_pods();
        assert_taken(&rig.client(&["do", "connect"]));
        let filled = rig.wait_filled();
        assert_eq!(rig.bluez().calls(), [device.call(DeviceMethod::Connect)]);
        assert_eq!(rig.dials().len(), 1);
        assert!(connected(&filled));
        assert_eq!(total(&filled, "connect_calls_total"), 1);
        rig.finish();
    }

    #[test]
    fn do_forget_removes_the_device_through_its_adapter() {
        let (rig, device) = up();
        assert_taken(&rig.client(&["do", "forget"]));
        device
            .pods
            .wait_closed(1, HANG_LIMIT)
            .expect("the channel closed");
        let cleared = rig.wait_status("the name cleared", |status| status["device_name"] == "");
        let removed = Call {
            object: rig.adapter().0.clone(),
            method: DeviceMethod::RemoveDevice,
            argument: Some(device.path.0.clone()),
        };
        assert_eq!(rig.bluez().calls(), [removed]);
        assert_eq!(total(&cleared, "forget_calls_total"), 1);
        assert_refused(&rig.client(&["do", "connect"]), NO_DEVICE);
        rig.finish();
    }

    #[test]
    fn no_device_reads_the_same_on_the_cli_the_socket_and_dbus() {
        let mut rig = Rig::new();
        rig.start();
        assert_refused(&rig.client(&["do", "connect"]), NO_DEVICE);
        let reply = Proto1::connect(rig.session()).ask(&do_request(1, "connect"));
        assert_eq!(outcome(&reply), "no_device");
        assert_eq!(reply["error"]["message"], NO_DEVICE);
        assert_eq!(
            dbus_refusal(&rig, "Connect"),
            (
                format!("{INTERFACE}.Error.NoDevice"),
                Some(NO_DEVICE.to_owned())
            )
        );
        assert_eq!(rig.bluez().calls(), []);
        rig.finish();
    }

    #[test]
    fn do_connect_with_two_known_and_no_history_is_no_device() {
        let (mut rig, _first) = Rig::with_known_pods();
        rig.add_pods(Profile::default());
        rig.wait_disconnect_answers("no_device");
        let mut client = Proto1::connect(rig.session());
        assert_eq!(outcome(&client.ask(&do_request(2, "connect"))), "no_device");
        pause(QUIET);
        assert_eq!(rig.bluez().calls(), []);
        rig.finish();
    }

    #[test]
    fn a_call_bluez_refuses_raises_the_failure_counter_only() {
        let (rig, device) = Rig::with_known_pods();
        rig.bluez().fail_calls(true);
        let before = rig.status();
        let mut client = Proto1::connect(rig.session());
        assert_eq!(outcome(&client.ask(&do_request(1, "connect"))), "sent");
        let failed = rig.wait_status("the failed connect counted", |status| {
            total(status, "connect_failures_total") == 1
        });
        assert_eq!(total(&failed, "connect_calls_total"), 1);
        rig.no_dial_for(QUIET);
        let mut after = rig.status();
        for counter in ["connect_calls_total", "connect_failures_total"] {
            after[counter] = before[counter].clone();
        }
        assert_eq!(after, before);
        assert_eq!(rig.bluez().calls(), [device.call(DeviceMethod::Connect)]);
        rig.finish();
    }

    #[test]
    fn a_call_bluez_never_answers_does_not_block_status() {
        let (rig, device) = Rig::with_known_pods();
        rig.bluez().hold_replies(true);
        let mut client = Proto1::connect(rig.session());
        assert_eq!(outcome(&client.ask(&do_request(1, "connect"))), "sent");
        wait_for("the connect call at the fake BlueZ", || {
            (rig.bluez().calls() == [device.call(DeviceMethod::Connect)]).then_some(())
        });
        // No failure counted, so the reply and each answer came while the call hung.
        for _ in 0..STATUS_CALLS {
            let status = rig.status();
            assert_eq!(total(&status, "connect_calls_total"), 1, "{status}");
            assert_eq!(total(&status, "connect_failures_total"), 0, "{status}");
            pause(STATUS_APART);
        }
        rig.finish();
    }

    #[test]
    fn a_fifth_unanswered_call_fails_at_once() {
        let (rig, _device) = Rig::with_known_pods();
        rig.bluez().hold_replies(true);
        let mut client = Proto1::connect(rig.session());
        for id in 1..=CALLS_IN_FLIGHT_MAX {
            assert_eq!(outcome(&client.ask(&do_request(id, "connect"))), "sent");
        }
        wait_for("four calls at the fake BlueZ", || {
            (rig.bluez().calls().len() == CALLS_IN_FLIGHT_MAX).then_some(())
        });
        let fifth = CALLS_IN_FLIGHT_MAX + 1;
        // A fresh client: the daemon closes one left idle past its stall deadline.
        let reply = Proto1::connect(rig.session()).ask(&do_request(fifth, "connect"));
        assert_eq!(outcome(&reply), "sent");
        // Exactly one failure, so the fifth failed while the four held calls still hung.
        let failed = rig.wait_status("the fifth call failed", |status| {
            total(status, "connect_failures_total") == 1
        });
        assert_eq!(total(&failed, "connect_calls_total"), 5);
        pause(QUIET);
        assert_eq!(rig.bluez().calls().len(), CALLS_IN_FLIGHT_MAX);
        rig.finish();
    }

    #[test]
    fn a_call_is_never_made_for_a_device_bluez_does_not_hold() {
        let (rig, device) = up();
        rig.bluez().remove(&device.path);
        rig.wait_status("the name cleared", |status| status["device_name"] == "");
        let mut client = Proto1::connect(rig.session());
        for (id, command) in ["connect", "disconnect", "forget"].into_iter().enumerate() {
            assert_eq!(
                outcome(&client.ask(&do_request(id + 1, command))),
                "no_device"
            );
        }
        pause(QUIET);
        assert_eq!(rig.bluez().calls(), []);
        rig.finish();
    }
}
