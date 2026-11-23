//! What BlueZ says about the pods decides when they are dialled, kept, cleared or left alone.

use std::time::{Duration, Instant};

use pods_fake::Profile;
use pods_trap::Answer;
use rustix::io::Errno;
use serde_json::Value;
use zbus::OwnedVariant;

use crate::support::rig::{
    ABSENT_GRACE, DIAL_WAIT, Device, Rig, SIGNAL_SLACK, assert_gap, connected, named, serve,
};
use crate::support::{HANG_LIMIT, pause};

/// Gone for a while, well inside the grace.
const AWAY: Duration = Duration::from_secs(3);

const FLAPS: usize = 20;
const FLAP_APART: Duration = Duration::from_millis(10);

/// Longer than all the flaps, so every flap lands while the dial waits for its answer.
const DIAL_ANSWERED_AFTER: Duration = Duration::from_millis(500);

/// Past the 750 ms first retry, so a dial the daemon already queued comes within it.
const QUIET_FOR: Duration = Duration::from_secs(1);

const SECOND_NAME: &str = "Test\u{2019}s Second AirPods";

#[test]
fn pods_back_within_the_grace_keep_their_state() {
    let mut rig = Rig::new();
    let device = rig.add_pods(Profile::default());
    rig.start();
    let watch = rig.watch();
    let mut lines = vec![
        watch
            .next(HANG_LIMIT)
            .expect("the state watch prints first")
            .state,
    ];
    rig.link_up(&device);
    lines.extend(watch.lines_until(&rig.wait_filled()));
    rig.link_down(&device);
    pause(AWAY);
    let back = Instant::now();
    rig.link_up(&device);
    let dials = rig.wait_dials(2, HANG_LIMIT);
    assert_gap(
        "the dial after the pods came back",
        back,
        dials[1].at,
        &(Duration::ZERO..=DIAL_WAIT),
    );
    lines.extend(watch.lines_until(&rig.wait_status("connected again", connected)));
    // A reset clears the name and the batteries together; neither may go while the pods are away.
    let kept = |line: &Value| named(line) && line["left"]["available"] == true;
    let first_kept = lines.iter().position(kept).unwrap();
    assert!(lines[first_kept..].iter().all(kept), "{lines:#?}");
    rig.finish();
}

#[test]
fn pods_gone_for_good_are_cleared_after_the_grace() {
    let mut rig = Rig::new();
    let device = rig.add_pods(Profile::default());
    rig.start();
    let watch = rig.watch();
    rig.link_up(&device);
    let filled = rig.wait_filled();
    watch.lines_until(&filled);
    let failures = filled["reconnect_failures_total"].as_u64().unwrap();
    let down = Instant::now();
    rig.link_down(&device);
    let cleared = loop {
        let printed = watch
            .next(HANG_LIMIT)
            .expect("a line with the name cleared");
        if printed.state["device_name"] == "" {
            break printed;
        }
    };
    assert_gap(
        "the cleared line after the pods went",
        down,
        cleared.at,
        &(ABSENT_GRACE..),
    );
    assert_eq!(cleared.state["reconnect_failures_total"], failures + 1);
    rig.finish();
}

#[test]
fn a_second_pair_is_dialled_only_after_the_first_is_reset() {
    let mut rig = Rig::new();
    let first = rig.add_pods(Profile::default());
    let second = rig.add_pods(Profile {
        name: SECOND_NAME.to_owned(),
        ..Profile::default()
    });
    rig.start();
    rig.link_up(&first);
    rig.wait_filled();
    rig.link_up(&second);
    rig.no_dial_for(QUIET_FOR);
    let down = Instant::now();
    rig.link_down(&first);
    let dials = rig.wait_dials(2, HANG_LIMIT);
    assert_eq!(dials[1].device, second.address);
    assert_gap(
        "the dial to the second pair after the first went",
        down,
        dials[1].at,
        &(ABSENT_GRACE..),
    );
    rig.wait_status("the second pair's name", |status| {
        status["device_name"] == SECOND_NAME
    });
    rig.finish();
}

#[test]
fn a_refused_device_that_disconnects_is_not_dialled_again() {
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
    rig.wait_dials(2, HANG_LIMIT);
    rig.bluez()
        .set(&device.path, "Connected", OwnedVariant::from(false));
    pause(SIGNAL_SLACK);
    rig.no_dial_for(Duration::from_secs(5));
    let dials = rig.dials();
    assert_eq!(dials.len(), 2, "{dials:?}");
    rig.finish();
}

#[test]
fn a_flapping_connected_property_never_opens_two_channels() {
    let mut rig = Rig::new();
    let device = rig.add_pods(Profile::default());
    rig.start();
    // The first of the twenty sets false, which the pods already are.
    flap(&rig, &device, true);
    let status = rig.wait_filled();
    assert_eq!(status["device_name"], Profile::default().name);
    rig.no_dial_for(QUIET_FOR);
    assert_eq!(device.pods.most_open_at_once(), 1);
    rig.finish();
}

/// Sets Connected `FLAPS` times, `FLAP_APART` apart, alternating and ending on `last`; returns when the last was set.
fn flap(rig: &Rig, device: &Device, last: bool) -> Instant {
    let mut last_set = Instant::now();
    for left in (0..FLAPS).rev() {
        let value = last == left.is_multiple_of(2);
        rig.bluez()
            .set(&device.path, "Connected", OwnedVariant::from(value));
        last_set = Instant::now();
        pause(FLAP_APART);
    }
    last_set
}

/// Fails unless the last flap came while the only dial was in flight, after it was made and before it was answered.
fn assert_in_flight(rig: &Rig, last_flap: Instant) {
    let dials = rig.dials();
    assert_eq!(dials.len(), 1, "{dials:?}");
    let since_dial = last_flap.checked_duration_since(dials[0].at);
    assert!(
        since_dial.is_some_and(|since| !since.is_zero() && since < DIAL_ANSWERED_AFTER),
        "the last flap came {since_dial:?} after the dial, which was answered after {DIAL_ANSWERED_AFTER:?}"
    );
}

#[test]
fn flaps_ending_false_while_a_dial_is_in_flight_leave_no_link() {
    let mut rig = Rig::new();
    let device = rig.add_pods(Profile::default());
    rig.start();
    rig.answer(&device, serve(&device.pods, DIAL_ANSWERED_AFTER));
    let last_flap = flap(&rig, &device, false);
    device
        .pods
        .wait_closed(1, HANG_LIMIT)
        .unwrap_or_else(|deadline| panic!("the channel stayed open: {deadline:?}"));
    rig.no_dial_for(QUIET_FOR);
    assert_in_flight(&rig, last_flap);
    assert!(!connected(&rig.status()));
    rig.finish();
}

#[test]
fn flaps_ending_true_while_a_dial_is_in_flight_leave_one_link() {
    let mut rig = Rig::new();
    let device = rig.add_pods(Profile::default());
    rig.start();
    rig.answer(&device, serve(&device.pods, DIAL_ANSWERED_AFTER));
    let last_flap = flap(&rig, &device, true);
    let status = rig.wait_filled();
    assert_eq!(status["device_name"], Profile::default().name);
    rig.no_dial_for(QUIET_FOR);
    assert_in_flight(&rig, last_flap);
    assert_eq!(device.pods.most_open_at_once(), 1);
    rig.finish();
}
