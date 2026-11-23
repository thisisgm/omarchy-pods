//! A `set` on the socket reaches the pods at once, and one racing the end of the link is still answered.

use std::time::{Duration, Instant};

use pods_fake::{Fault, Profile};
use pods_samples::{NOISE, setting};
use proptest::prelude::RngExt;
use proptest::test_runner::{RngAlgorithm, TestRng};

use crate::support::rig::{ANC, Rig, TRANSPARENCY, assert_gap, connected};
use crate::support::{HANG_LIMIT, Proto1, outcome, pause, set_request};

const RACE_ROUNDS: usize = 20;

/// How far before or after the request the hang up may come, in milliseconds.
const RACE_SPREAD_MS: i64 = 20;

const SETS: usize = 200;

/// The mode the `n`th request asks for, so each one is a change.
fn alternate(request: usize) -> &'static str {
    if request.is_multiple_of(2) {
        "transparency"
    } else {
        "anc"
    }
}

#[test]
fn set_racing_a_hang_up_always_gets_an_answer() {
    let mut rng = TestRng::deterministic_rng(RngAlgorithm::ChaCha);
    let mut rig = Rig::new();
    let device = rig.add_pods(Profile::default());
    // Pods that never report a setting back, so only the write and the end of the link answer a set.
    device.pods.fault(Fault::NeverConfirm);
    rig.start();
    rig.link_up(&device);
    rig.wait_filled();
    for round in 0..RACE_ROUNDS {
        // A client per round: the daemon closes one left idle through the wait for the next link.
        let mut client = Proto1::connect(rig.session());
        let offset = rng.random_range(-RACE_SPREAD_MS..=RACE_SPREAD_MS);
        let apart = Duration::from_millis(offset.unsigned_abs());
        if offset < 0 {
            device.pods.hang_up();
            pause(apart);
        }
        client.send(&set_request(round, "noise", alternate(round)));
        if offset >= 0 {
            pause(apart);
            device.pods.hang_up();
        }
        let reply = client.reply();
        assert!(
            ["sent", "not_connected"].contains(&outcome(&reply)),
            "round {round}, hang up at {offset} ms: {reply}"
        );
        rig.wait_dials(round + 2, HANG_LIMIT);
        rig.wait_status("connected again", connected);
    }
    rig.finish();
}

/// The driver's test on a held clock proves no wait before the write; the gaps printed here are the 20 ms budget, measured on the box.
#[test]
fn every_set_reaches_the_pods_after_its_request() {
    let mut rig = Rig::new();
    let device = rig.add_pods(Profile::default());
    rig.start();
    rig.link_up(&device);
    rig.wait_filled();
    let mut client = Proto1::connect(rig.session());
    let mut written = Vec::new();
    for request in 0..SETS {
        written.push(Instant::now());
        let reply = client.ask(&set_request(request, "noise", alternate(request)));
        assert_eq!(outcome(&reply), "sent", "request {request}: {reply}");
    }
    let frames = [setting(NOISE, TRANSPARENCY.wire), setting(NOISE, ANC.wire)];
    for frame in &frames {
        device.pods.wait_heard(frame, SETS / 2, HANG_LIMIT).unwrap();
    }
    let heard: Vec<Instant> = device
        .pods
        .heard()
        .into_iter()
        .filter(|heard| frames.contains(&heard.sdu))
        .map(|heard| heard.at)
        .collect();
    assert_eq!(heard.len(), SETS);
    let mut gaps: Vec<(Instant, Instant)> = written.into_iter().zip(heard).collect();
    gaps.sort_by_key(|(written, heard)| heard.saturating_duration_since(*written));
    for (what, (written, heard)) in [
        ("the fastest set", gaps[0]),
        ("the slowest set", gaps[SETS - 1]),
    ] {
        assert_gap(what, written, heard, &(Duration::ZERO..));
    }
    rig.finish();
}
