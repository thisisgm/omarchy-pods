//! When the channel fails, the next dial waits at least the step of the ladder the failure puts it on; the exact step is the paused driver tests' to prove.

use std::time::{Duration, Instant};

use pods_fake::{Fault, Profile};
use pods_trap::Answer;
use rustix::io::Errno;

use crate::support::HANG_LIMIT;
use crate::support::rig::{DIAL_WAIT, FIRST_RETRY, HELD_LADDER, Rig, assert_gap, connected};

/// How long the daemon waits for each acknowledgement of the opening before it closes the channel.
const HANDSHAKE_STEP: Duration = Duration::from_millis(2000);

/// From a dial the pods never acknowledge to the next: the handshake step, then the first held delay.
const HANDSHAKE_THEN_HELD: Duration = HANDSHAKE_STEP.saturating_add(HELD_LADDER[0]);

/// How soon a closed link shows as disconnected.
const DOWN_WITHIN: Duration = Duration::from_secs(1);

#[test]
fn a_hang_up_mid_burst_is_followed_by_a_dial_after_750_ms() {
    let mut rig = Rig::new();
    let device = rig.add_pods(Profile::default());
    device.pods.fault(Fault::CloseAfter(10));
    rig.start();
    let watch = rig.watch();
    let first = watch
        .next(HANG_LIMIT)
        .expect("the state watch prints first");
    rig.link_up(&device);
    let closed = device.pods.wait_closed(1, HANG_LIMIT).unwrap()[0];
    device.pods.fault(Fault::None);
    let down = rig.wait_status("connected false", |status| !connected(status));
    assert_gap(
        "connected false after the close",
        closed,
        Instant::now(),
        &(Duration::ZERO..=DOWN_WITHIN),
    );
    let mut lines = vec![first.state];
    lines.extend(watch.lines_until(&down));
    assert!(
        lines.iter().any(connected),
        "the link was never up: {lines:#?}"
    );
    let dials = rig.wait_dials(2, HANG_LIMIT);
    assert_gap(
        "the second dial after the close",
        closed,
        dials[1].at,
        &(FIRST_RETRY..),
    );
    rig.wait_filled();
    rig.finish();
}

#[test]
fn pods_that_never_acknowledge_are_closed_after_2_s_and_dialled_again() {
    let mut rig = Rig::new();
    let device = rig.add_pods(Profile::default());
    device.pods.fault(Fault::NeverAcknowledge);
    rig.start();
    let watch = rig.watch();
    let first = watch
        .next(HANG_LIMIT)
        .expect("the state watch prints first");
    rig.link_up(&device);
    let dials = rig.wait_dials(1, HANG_LIMIT);
    let closed = device.pods.wait_closed(1, HANG_LIMIT).unwrap()[0];
    assert_gap(
        "the close after the dial",
        dials[0].at,
        closed,
        &(HANDSHAKE_STEP..),
    );
    let dials = rig.wait_dials(2, HANG_LIMIT);
    assert_gap(
        "the next dial after the first",
        dials[0].at,
        dials[1].at,
        &(HANDSHAKE_THEN_HELD..),
    );
    let mut lines = vec![first.state];
    lines.extend(watch.lines_until(&rig.status()));
    assert!(lines.iter().all(|line| !connected(line)), "{lines:#?}");
    rig.finish();
}

#[test]
fn two_refusals_then_an_accept_dial_at_0_1_and_3_s() {
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
    let up = Instant::now();
    rig.link_up(&device);
    rig.wait_dials(2, HANG_LIMIT);
    rig.accept(&device);
    let dials = rig.wait_dials(3, HANG_LIMIT);
    assert_gap(
        "the first dial after the link came up",
        up,
        dials[0].at,
        &(Duration::ZERO..=DIAL_WAIT),
    );
    assert_gap(
        "the second dial after the first",
        dials[0].at,
        dials[1].at,
        &(HELD_LADDER[0]..),
    );
    assert_gap(
        "the third dial after the second",
        dials[1].at,
        dials[2].at,
        &(HELD_LADDER[1]..),
    );
    rig.wait_filled();
    rig.finish();
}

#[test]
fn pods_that_close_at_once_climb_the_refused_ladder() {
    let mut rig = Rig::new();
    let device = rig.add_pods(Profile::default());
    device.pods.fault(Fault::CloseAfter(0));
    rig.start();
    rig.link_up(&device);
    let dials = rig.wait_dials(3, HANG_LIMIT);
    let closes = device.pods.wait_closed(2, HANG_LIMIT).unwrap();
    assert_gap(
        "the second dial after the first close",
        closes[0],
        dials[1].at,
        &(HELD_LADDER[0]..),
    );
    assert_gap(
        "the third dial after the second close",
        closes[1],
        dials[2].at,
        &(HELD_LADDER[1]..),
    );
    rig.finish();
}
