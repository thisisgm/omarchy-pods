//! Proximity readings belong to the pair whose keys decrypted them.

use std::time::Duration;

use pods_fake::Profile;
use pods_trap::Answer;
use rustix::io::Errno;

use crate::support::HANG_LIMIT;
use crate::support::advert::{HEADER, PAYLOAD, keyed, ours};
use crate::support::held::held;
use crate::support::rig::{Device, HELD_LADDER, Rig};
use crate::tests::{QUIET, filled, picked, wait_intake};

fn two_pairs() -> (Rig, Device) {
    let mut rig = keyed();
    let first = rig.add_pods(Profile::default());
    let second = rig.add_pods(Profile {
        model_number: "A3048".to_owned(),
        ..Profile::default()
    });
    rig.session().store_sample_keys(Some(first.address));
    rig.start();
    wait_intake(&rig, "listening");
    (rig, second)
}

#[test]
fn idle_readings_are_cleared_when_a_different_pair_connects() {
    let (rig, second) = two_pairs();
    let empty = picked(&rig.status());
    let advert = ours(HEADER, PAYLOAD);
    let _advertiser = rig.bluez().advertise(&advert.address, &advert.data);
    rig.wait_status("the first pair's model, battery, lid and wear", |status| {
        picked(status) == filled()
    });
    let (answer, release) = held(&second.pods);
    rig.answer(&second, answer);
    rig.link_up(&second);
    assert_eq!(rig.wait_dials(1, HANG_LIMIT)[0].device, second.address);
    assert_eq!(picked(&rig.status()), empty);
    drop(release);
    let filled = rig.wait_filled();
    assert_eq!(filled["model_name"], "AirPods Pro 2 (USB-C)");
    assert_eq!(filled["lid_state"], empty["lid_state"]);
    rig.finish();
}

fn ignores_first_pair(rig: &Rig) {
    let before = picked(&rig.status());
    let advert = ours(HEADER, PAYLOAD);
    let _advertiser = rig.bluez().advertise(&advert.address, &advert.data);
    rig.holds_for(QUIET, "no readings from the first pair", |status| {
        picked(status) == before
    });
}

#[test]
fn a_different_pairs_advert_is_ignored_while_opening() {
    let (rig, second) = two_pairs();
    let (answer, _release) = held(&second.pods);
    rig.answer(&second, answer);
    rig.link_up(&second);
    assert_eq!(rig.wait_dials(1, HANG_LIMIT)[0].device, second.address);
    ignores_first_pair(&rig);
    rig.finish();
}

fn refuse(rig: &Rig, second: &Device) {
    rig.answer(
        second,
        Answer::Refuse {
            after: Duration::ZERO,
            errno: Errno::CONNREFUSED,
        },
    );
}

#[test]
fn a_different_pairs_advert_is_ignored_in_backoff() {
    let (mut rig, second) = two_pairs();
    refuse(&rig, &second);
    rig.link_up(&second);
    rig.wait_entered("backoff", 1);
    ignores_first_pair(&rig);
    rig.finish();
}

#[test]
#[ignore = "waits for the full held retry ladder"]
fn a_different_pairs_advert_is_ignored_while_parked() {
    let (mut rig, second) = two_pairs();
    refuse(&rig, &second);
    rig.link_up(&second);
    let within = HELD_LADDER
        .iter()
        .sum::<Duration>()
        .saturating_add(HANG_LIMIT);
    assert_eq!(
        rig.wait_dials(HELD_LADDER.len().saturating_add(1), within)
            .last()
            .unwrap()
            .device,
        second.address
    );
    rig.wait_entered("parked", 1);
    ignores_first_pair(&rig);
    rig.finish();
}
