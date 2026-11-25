//! The signal a service manager sends ends the daemon within a second, the link up or a dial still waiting.

use std::time::{Duration, Instant};

use pods_fake::Profile;
use pods_trap::{Answer, Outcome};
use rustix::io::Errno;

use crate::support::bluez::{Call, DeviceMethod};
use crate::support::rig::{Rig, assert_gap};
use crate::support::uplink::keep_off;
use crate::support::{HANG_LIMIT, wait_for};

/// How soon the daemon exits after SIGTERM.
const STOP_WITHIN: Duration = Duration::from_secs(1);

/// A connect the kernel answers only after this long, past the stop.
const SLOW_CONNECT: Duration = Duration::from_secs(4);

/// Sends SIGTERM and fails unless the daemon exits 0 within a second, its socket and state file gone.
fn stop_within_a_second(rig: &mut Rig) {
    let signalled = Instant::now();
    let status = rig.stop(STOP_WITHIN);
    assert_gap(
        "the exit after SIGTERM",
        signalled,
        Instant::now(),
        &(Duration::ZERO..=STOP_WITHIN),
    );
    assert!(status.success(), "{status}: {}", rig.daemon().log());
    assert!(!rig.session().socket().exists());
    assert!(!rig.session().state_file().exists());
}

#[test]
fn sigterm_closes_the_channel_and_leaves_the_device_connected() {
    let mut rig = Rig::new();
    // An uplink over a sink not known quiet holds the exit for its own bound, which the uplink suites time.
    keep_off(rig.session());
    let device = rig.add_pods(Profile::default());
    rig.start();
    rig.link_up(&device);
    rig.wait_filled();
    stop_within_a_second(&mut rig);
    device.pods.wait_closed(1, STOP_WITHIN).unwrap();
    assert_eq!(rig.bluez().calls(), []);
    // The record above is empty because nothing called, not because nothing is recorded.
    rig.bluez().call_device(&device.path, "Disconnect");
    let disconnect = Call {
        object: device.path.0.clone(),
        method: DeviceMethod::Disconnect,
        argument: None,
    };
    assert_eq!(rig.bluez().calls(), [disconnect]);
    rig.finish();
}

#[test]
fn sigterm_during_a_slow_connect_still_exits_within_a_second() {
    let mut rig = Rig::new();
    let device = rig.add_pods(Profile::default());
    rig.start();
    rig.answer(
        &device,
        Answer::Refuse {
            after: SLOW_CONNECT,
            errno: Errno::TIMEDOUT,
        },
    );
    rig.link_up(&device);
    let dials = rig.wait_dials(1, HANG_LIMIT);
    assert_eq!(dials[0].outcome, Outcome::Pending);
    stop_within_a_second(&mut rig);
    wait_for("the dial abandoned", || {
        (rig.dials()[0].outcome == Outcome::Abandoned).then_some(())
    });
    rig.finish();
}
