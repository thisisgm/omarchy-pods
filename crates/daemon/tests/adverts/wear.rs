//! Connect-when-worn through the daemon and fake BlueZ.

use std::fmt::Write as _;
use std::fs;
use std::time::Duration;

use pods_fake::Profile;
use pods_samples::Placed;
use pods_samples::advert::{Advert, HEADER_LEN};
use zbus::OwnedVariant;

use super::support::advert::{HEADER, PAYLOAD, keyed, ours};
use super::support::bluez::monitor::registration;
use super::support::bluez::{Call, DeviceMethod};
use super::support::rig::{Device, Rig};
use super::support::sleep::logged;
use super::support::{HANG_LIMIT, pause, wait_for};
use super::tests::{CASE_AT, LOW_CASE_HEADER, QUIET, filled, picked, wait_intake};

const BOTH_OUT_HEADER: [u8; HEADER_LEN] = [
    0x07, 0x19, 0x01, 0x27, 0x20, 0x20, 0x88, 0x47, 0x00, 0x00, 0x04,
];
const IN_CASE_HEADER: [u8; HEADER_LEN] = [
    0x07, 0x19, 0x01, 0x27, 0x20, 0x60, 0x88, 0x47, 0x08, 0x00, 0x04,
];
const CONNECTION_AT: usize = 10;

#[test]
fn a_wear_edge_connects_the_paired_device_through_bluez() {
    let mut rig = keyed();
    let device = wear_pods(&mut rig);
    rig.start();
    wait_intake(&rig, "listening");
    let advert = ours(disconnected_worn_header(), PAYLOAD);
    let advertiser = rig.bluez().advertise(&advert.address, &advert.data);
    rig.wait_status("the advert in the status", |status| {
        picked(status) == filled()
    });
    rig.no_dial_for(QUIET);
    assert_eq!(rig.bluez().calls(), registration(rig.adapter()));
    advertiser.update(&ours(BOTH_OUT_HEADER, PAYLOAD).data);
    rig.wait_status("both buds out of ear", |status| {
        status["right"]["in_ear"] == false
    });
    advertiser.update(&advert.data);
    let connect = wait_for("BlueZ Connect after the wear edge", || wear_call(&rig));
    assert_eq!(connect.object, device.path.0);
    rig.wait_filled();
    assert_eq!(logged(&mut rig, "link: connect when worn"), 1);
    rig.finish();
}

#[test]
fn wear_does_not_connect_a_different_paired_device_after_restart() {
    let mut rig = Rig::new();
    rig.bluez().add_monitor_manager(rig.adapter());
    let first = rig.add_pods(Profile {
        keys: Some((pods_samples::IDENTITY_KEY, pods_samples::PAYLOAD_KEY)),
        ..Profile::default()
    });
    let _second = rig.add_pods(Profile::default());
    rig.start();
    rig.link_up(&first);
    rig.wait_filled();
    let bound = format!("\"device\":\"{}\"", hex(&first.address.octets()));
    wait_for("the first pair's keys bound to it", || {
        let text = fs::read_to_string(rig.session().keys_file()).ok()?;
        text.contains(&bound).then_some(())
    });
    assert!(rig.stop(HANG_LIMIT).success());
    rig.bluez().remove(&first.path);
    rig.start();
    wait_intake(&rig, "listening");

    let out = marked_out();
    let advertiser = rig.bluez().advertise(&out.address, &out.data);
    rig.wait_status("the first pair's out-of-ear advert", |status| {
        status["case"]["level"] == 10
    });
    advertiser.update(&ours(disconnected_worn_header(), PAYLOAD).data);
    rig.wait_status("the first pair's worn advert", |status| {
        status["right"]["in_ear"] == true
    });
    pause(QUIET);
    assert!(
        wear_call(&rig).is_none(),
        "wear must not connect a different paired device"
    );
    assert!(rig.stop(HANG_LIMIT).success());
    rig.finish();
}

#[test]
fn legacy_keys_read_adverts_without_connecting() {
    let mut rig = keyed();
    let _device = rig.add_pods(Profile::default());
    rig.start();
    wait_intake(&rig, "listening");

    let out = marked_out();
    let advertiser = rig.bluez().advertise(&out.address, &out.data);
    rig.wait_status("the legacy keys resolve an out-of-ear advert", |status| {
        status["case"]["level"] == 10
    });
    advertiser.update(&ours(disconnected_worn_header(), PAYLOAD).data);
    rig.wait_status("the legacy keys resolve a worn advert", |status| {
        status["right"]["in_ear"] == true
    });
    pause(QUIET);
    assert!(
        wear_call(&rig).is_none(),
        "legacy keys have no saved device"
    );
    rig.finish();
}

fn disconnected_worn_header() -> [u8; HEADER_LEN] {
    let mut header = HEADER;
    header[CONNECTION_AT] = 0x00;
    header
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::new();
    for byte in bytes {
        write!(text, "{byte:02x}").unwrap();
    }
    text
}

fn wear_call(rig: &Rig) -> Option<Call> {
    rig.bluez()
        .calls()
        .into_iter()
        .find(|call| call.method == DeviceMethod::Connect)
}

#[test]
fn a_wear_edge_does_not_connect_an_unpaired_device() {
    let mut rig = keyed();
    let device = wear_pods(&mut rig);
    rig.bluez()
        .set(&device.path, "Paired", OwnedVariant::from(false));
    rig.start();
    wait_intake(&rig, "listening");
    let out = ours(BOTH_OUT_HEADER, PAYLOAD);
    let advertiser = rig.bluez().advertise(&out.address, &out.data);
    rig.wait_status("both buds out of ear", |status| {
        status["right"]["available"] == true && status["right"]["in_ear"] == false
    });
    advertiser.update(&ours(disconnected_worn_header(), PAYLOAD).data);
    rig.wait_logged("bluez: Connect failed: BlueZ did not confirm pairing");
    assert!(wear_call(&rig).is_none());
    assert_eq!(logged(&mut rig, "link: connect when worn"), 0);
    rig.finish();
}

#[test]
fn a_wear_edge_reports_a_failed_paired_get() {
    let mut rig = keyed();
    let _device = wear_pods(&mut rig);
    rig.start();
    wait_intake(&rig, "listening");
    let out = marked_out();
    let advertiser = rig.bluez().advertise(&out.address, &out.data);
    rig.wait_status("the out-of-ear advert", |status| {
        picked(status)["case"]["level"] == 10
    });
    rig.bluez().fail_calls(true);
    advertiser.update(&ours(disconnected_worn_header(), PAYLOAD).data);
    let error = wait_for("the failed pairing check", || {
        rig.daemon()
            .log()
            .lines()
            .find_map(|line| line.strip_prefix("bluez: Connect failed: "))
            .map(str::to_owned)
    });
    rig.bluez().fail_calls(false);
    assert_eq!(error, "org.bluez.Error.Failed");
    assert!(wear_call(&rig).is_none());
    rig.finish();
}

#[test]
fn disconnect_sources_hold_auto_connect_until_the_case() {
    use DisconnectSource::{BluezFirst, ChannelFirst, Command};
    #[derive(Clone, Copy, Debug)]
    enum DisconnectSource {
        Command,
        BluezFirst,
        ChannelFirst,
    }

    for source in [Command, BluezFirst, ChannelFirst] {
        let mut rig = keyed();
        let device = wear_pods(&mut rig);
        rig.start();
        rig.link_up(&device);
        rig.wait_filled();
        assert!(rig.status()["left"]["in_ear"] == true || rig.status()["right"]["in_ear"] == true);
        if matches!(source, Command) {
            rig.disconnect();
        } else {
            if matches!(source, ChannelFirst) {
                rig.close_channel(&device);
                rig.wait_logged("link: up to backoff");
            }
            rig.bluez()
                .set(&device.path, "Connected", OwnedVariant::from(false));
            if matches!(source, ChannelFirst) {
                rig.wait_logged("link: backoff to grace");
            }
        }
        wait_intake(&rig, "listening");
        let out = ours(BOTH_OUT_HEADER, PAYLOAD);
        let advertiser = rig.bluez().advertise(&out.address, &out.data);
        rig.wait_status("the out-of-ear advert", |status| {
            picked(status)["case"]["level"] == 70 && status["right"]["in_ear"] == false
        });
        let worn = ours(disconnected_worn_header(), PAYLOAD);
        advertiser.update(&worn.data);
        rig.wait_status("the worn advert", |status| {
            status["right"]["in_ear"] == true
        });
        if matches!(source, ChannelFirst) {
            pause(QUIET);
        } else {
            rig.no_dial_for(QUIET);
        }
        assert!(
            wear_call(&rig).is_none(),
            "{source:?} disconnect must hold the wear edge"
        );
        advertiser.update(&ours(IN_CASE_HEADER, PAYLOAD).data);
        rig.wait_status("the case advert", |status| status["lid_state"] == 1);
        advertiser.update(&worn.data);
        wait_for("the wear edge after the case", || wear_call(&rig));
        rig.finish();
    }
}

/// Both buds out with the case at 10 percent, a level the linked pods never report.
fn marked_out() -> Advert {
    let mut header = BOTH_OUT_HEADER;
    header[CASE_AT] = LOW_CASE_HEADER[CASE_AT];
    ours(header, PAYLOAD)
}

fn wear_pods(rig: &mut Rig) -> Device {
    let device = rig.add_pods(Profile::default());
    rig.session().store_sample_keys(Some(device.address));
    device
}

fn unworn_link(rig: &mut Rig) -> Device {
    let device = wear_pods(rig);
    rig.start();
    rig.link_up(&device);
    rig.wait_filled();
    device.pods.set_ear(Placed::OutOfEar, Placed::OutOfEar);
    rig.wait_status("both buds out", |status| {
        status["left"]["in_ear"] == false && status["right"]["in_ear"] == false
    });
    device
}

fn connects(rig: &Rig) -> usize {
    rig.bluez()
        .calls()
        .into_iter()
        .filter(|call| call.method == DeviceMethod::Connect)
        .count()
}

#[test]
fn an_unworn_disconnect_holds_only_from_the_command() {
    for from_command in [true, false] {
        let mut rig = keyed();
        let device = unworn_link(&mut rig);
        if from_command {
            rig.disconnect();
        } else {
            rig.bluez()
                .set(&device.path, "Connected", OwnedVariant::from(false));
        }
        wait_intake(&rig, "listening");
        let out = marked_out();
        let advertiser = rig.bluez().advertise(&out.address, &out.data);
        rig.wait_status("the out-of-ear advert", |status| {
            picked(status)["case"]["level"] == 10
        });
        advertiser.update(&ours(disconnected_worn_header(), PAYLOAD).data);
        if from_command {
            rig.wait_status("the worn advert", |status| {
                status["right"]["in_ear"] == true
            });
            rig.no_dial_for(QUIET);
            assert!(wear_call(&rig).is_none(), "the command holds");
        } else {
            wait_for("the wear edge after an unworn drop", || wear_call(&rig));
        }
        rig.finish();
    }
}

#[test]
fn a_monitor_release_keeps_the_manual_disconnect_hold() {
    /// Past core's 10 s `MANUAL_HOLD_CLEAR_MS`, so a kept out-of-ear time would clear the hold.
    const PAST_HOLD: Duration = Duration::from_secs(11);

    let mut rig = keyed();
    let _device = unworn_link(&mut rig);
    rig.disconnect();
    wait_intake(&rig, "listening");
    let out = marked_out();
    let advertiser = rig.bluez().advertise(&out.address, &out.data);
    rig.wait_status("the out-of-ear advert", |status| {
        status["case"]["level"] == 10
    });

    rig.bluez().stall_monitor_registration(true);
    rig.bluez().release_monitors();
    rig.bluez().wait_monitors(0, HANG_LIMIT);
    pause(PAST_HOLD);
    rig.bluez().stall_monitor_registration(false);
    rig.bluez().wait_monitors(1, HANG_LIMIT);
    wait_intake(&rig, "listening");

    let worn = ours(disconnected_worn_header(), PAYLOAD);
    advertiser.update(&worn.data);
    wait_for("the worn advert or a Connect after release", || {
        (rig.status()["right"]["in_ear"] == true || wear_call(&rig).is_some()).then_some(())
    });
    pause(QUIET);
    assert!(
        wear_call(&rig).is_none(),
        "the monitor gap released the hold"
    );

    advertiser.update(&out.data);
    rig.wait_status("the next out-of-ear advert", |status| {
        status["right"]["in_ear"] == false
    });
    advertiser.update(&worn.data);
    rig.wait_status("the next worn advert", |status| {
        status["right"]["in_ear"] == true
    });
    pause(QUIET);
    assert!(
        wear_call(&rig).is_none(),
        "the first worn advert released the hold"
    );

    advertiser.update(&ours(IN_CASE_HEADER, PAYLOAD).data);
    rig.wait_status("the case advert", |status| status["lid_state"] == 1);
    advertiser.update(&worn.data);
    wait_for("the wear edge after the case", || wear_call(&rig));
    rig.finish();
}

#[test]
fn a_second_wear_edge_inside_the_cooldown_does_not_connect() {
    let mut rig = keyed();
    let device = wear_pods(&mut rig);
    rig.start();
    wait_intake(&rig, "listening");
    let out = marked_out();
    let advertiser = rig.bluez().advertise(&out.address, &out.data);
    rig.wait_status("the out-of-ear advert", |status| {
        picked(status)["case"]["level"] == 10
    });
    let worn = ours(disconnected_worn_header(), PAYLOAD);
    advertiser.update(&worn.data);
    wait_for("the first wear edge", || wear_call(&rig));
    rig.wait_filled();
    drop(advertiser);
    device.pods.set_ear(Placed::OutOfEar, Placed::OutOfEar);
    rig.wait_status("both buds out", |status| {
        status["left"]["in_ear"] == false && status["right"]["in_ear"] == false
    });
    rig.bluez()
        .set(&device.path, "Connected", OwnedVariant::from(false));
    wait_intake(&rig, "listening");
    let advertiser = rig.bluez().advertise(&out.address, &out.data);
    rig.wait_status("the second out-of-ear advert", |status| {
        picked(status)["case"]["level"] == 10
    });
    advertiser.update(&worn.data);
    wait_for("the second worn advert or a second Connect", || {
        (rig.status()["right"]["in_ear"] == true || connects(&rig) > 1).then_some(())
    });
    rig.no_dial_for(QUIET);
    assert_eq!(connects(&rig), 1, "the second edge is inside the cooldown");
    rig.finish();
}
