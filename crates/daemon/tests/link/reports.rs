//! What the pods report reaches the status, the state file, D-Bus and `watch`, and no report stops the link.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use pods_fake::{Fault, Profile};
use pods_samples::{Cell, MODEL_NUMBER, NOISE, host, metadata_of};
use pods_trap::Outcome;
use proptest::prelude::RngExt;
use proptest::test_runner::{RngAlgorithm, TestRng};
use serde_json::Value;
use tokio::runtime::Builder as RuntimeBuilder;
use tokio::time::timeout;
use zbus::OwnedVariant;
use zbus::connection::Builder;

use crate::support::bus::{BUS_NAME, INTERFACE, OBJECT_PATH};
use crate::support::rig::{ANC, Rig, TRANSPARENCY, assert_gap, connected, not_before};
use crate::support::uplink::rig_with;
use crate::support::{HANG_LIMIT, Proto1, read_to_close, wait_for, wait_within};

/// The control PSM AirPods listen on.
const CONTROL_PSM: u16 = 0x1001;

/// The C++ `parseData` repeats the notification request once, this long after the first, while no battery report came.
const NOTIFY_REREQUEST: Duration = Duration::from_millis(2000);

/// How soon reports sent before the acknowledgement show in the status.
const SHOWN_WITHIN: Duration = Duration::from_secs(1);

/// How soon the daemon answers the features acknowledgement with the notification request.
const REQUEST_WITHIN: Duration = Duration::from_secs(1);

/// Past the 2000 ms re-request wait, so a third request would come within it.
const NO_THIRD_FOR: Duration = Duration::from_secs(3);

/// Changes the never reading subscriber is sent, past the socket's buffer and the hub's bound together.
const CHANGES: usize = 2000;

const STATUS_CALLS: usize = 10;

/// Past the 4096 byte receive buffer of the daemon's reader.
const OVERSIZED_SDU: usize = 5000;

/// The name an oversized metadata packet carries, which only a truncated parse could put in the status.
const OVERSIZED_NAME: &str = "Oversized AirPods";

/// Bytes after the oversized packet's fields, none of them a field's end.
const FILLER: u8 = 0xa5;

const RANDOM_SDUS: usize = 500;
const RANDOM_SDU_MAX: usize = 1024;

/// The accessory header and the opcodes a parser reads: battery, ear, control and metadata, so half the random bytes reach one.
const ACCESSORY_HEADER: [u8; 4] = [0x04, 0x00, 0x04, 0x00];
const PARSED_OPCODES: [u8; 4] = [0x04, 0x06, 0x09, 0x1d];

/// A battery level no profile here starts with.
const NEW_LEVEL: Cell = Cell {
    level: 42,
    charging: false,
};

const GET: &str = r#"{"v":1,"id":1,"op":"get"}"#;
const SUBSCRIBE: &[u8] = b"{\"v\":1,\"id\":1,\"op\":\"subscribe\"}\n";

const LINK: &str = "the link";
const BATTERIES: &str = "the batteries";
const EARS: &str = "the ears";
const NAME_AND_MODEL: &str = "the name and model";
const NOISE_MODE: &str = "the noise mode";

/// The group of status keys `key` belongs to, in the flat form `left.level`.
fn group(key: &str) -> &'static str {
    let (object, field) = key.split_once('.').unwrap_or(("", key));
    match (object, field) {
        ("", "connected" | "audio_server") => LINK,
        ("left" | "right", "in_ear") => EARS,
        ("left" | "right" | "case" | "headset", _) => BATTERIES,
        (
            "",
            "device_name" | "model_name" | "model_int" | "model_number" | "is_pro_series"
            | "is_headset",
        ) => NAME_AND_MODEL,
        ("", field) if field.starts_with("supports_") => NAME_AND_MODEL,
        ("", "noise_mode") => NOISE_MODE,
        _ => panic!("{key} changed, a key of no group the burst may change"),
    }
}

/// The status as flat keys: `left` with `level` inside is `left.level`.
fn flat(status: &Value) -> BTreeMap<String, Value> {
    let mut keys = BTreeMap::new();
    for (key, value) in status.as_object().unwrap() {
        match value.as_object() {
            Some(inner) => {
                for (field, value) in inner {
                    keys.insert(format!("{key}.{field}"), value.clone());
                }
            }
            None => {
                keys.insert(key.clone(), value.clone());
            }
        }
    }
    keys
}

/// The one group of keys that differs between two neighbouring lines.
fn changed_group(before: &Value, after: &Value) -> &'static str {
    let after_keys = flat(after);
    let groups: BTreeSet<&str> = flat(before)
        .iter()
        .filter(|(key, value)| after_keys.get(*key) != Some(value))
        .map(|(key, _)| group(key))
        .collect();
    let mut only = groups.iter();
    match (only.next(), only.next()) {
        (Some(group), None) => group,
        _ => panic!("neighbouring lines differ in {groups:?}:\n{before}\n{after}"),
    }
}

/// Waits for `StatusJson` of the daemon's D-Bus interface to equal `wanted`.
fn wait_dbus_status(bus_address: &str, wanted: &Value) {
    let runtime = RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let connect = async { timeout(HANG_LIMIT, Builder::address(bus_address).build()).await };
    let client = runtime
        .block_on(connect)
        .expect("the session bus within the hang limit")
        .unwrap();
    wait_for("the D-Bus status to match", || {
        let call = async {
            let get = client.call_method(
                Some(BUS_NAME),
                OBJECT_PATH,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &(INTERFACE, "StatusJson"),
            );
            timeout(HANG_LIMIT, get).await
        };
        let reply = runtime.block_on(call).ok()?.ok()?;
        let value: OwnedVariant = reply.body().deserialize().ok()?;
        let document: Value = serde_json::from_str(&String::try_from(value).ok()?).ok()?;
        (document == *wanted).then_some(())
    });
}

#[test]
fn adoption_fills_status_file_and_dbus_alike() {
    let (rig, device) = rig_with("off", &[], Profile::default());
    rig.link_up(&device);
    let status = rig.wait_filled();
    assert_eq!(status["device_name"], Profile::default().name);
    assert_eq!(status["model_number"], "A3063");
    let pods = [
        (
            status["left"]["level"].as_u64(),
            status["left"]["charging"].as_bool(),
        ),
        (
            status["right"]["level"].as_u64(),
            status["right"]["charging"].as_bool(),
        ),
        (
            status["case"]["level"].as_u64(),
            status["case"]["charging"].as_bool(),
        ),
    ];
    assert_eq!(
        pods,
        [
            (Some(99), Some(true)),
            (Some(100), Some(false)),
            (Some(50), Some(false))
        ]
    );
    wait_for("the state file to match the status", || {
        let text = fs::read_to_string(rig.session().state_file()).ok()?;
        (serde_json::from_str::<Value>(&text).ok()? == status).then_some(())
    });
    wait_dbus_status(rig.session_bus(), &status);
    let dials = rig.dials();
    assert_eq!(dials.len(), 1, "{dials:?}");
    assert_eq!(
        (dials[0].psm, &dials[0].outcome),
        (CONTROL_PSM, &Outcome::Accepted)
    );
    // The key request leaves after the publish of the metadata, so it may come after the status filled.
    device
        .pods
        .wait_heard(&host::request_proximity_keys(), 1, HANG_LIMIT)
        .unwrap_or_else(|deadline| panic!("no key request heard: {deadline:?}"));
    let heard: Vec<Vec<u8>> = device
        .pods
        .heard()
        .into_iter()
        .map(|heard| heard.sdu)
        .collect();
    assert_eq!(
        heard,
        [
            host::handshake(),
            host::set_specific_features(),
            host::request_notifications(),
            host::request_proximity_keys()
        ]
    );
    rig.finish();
}

#[test]
fn watch_prints_every_change_of_the_burst_in_order() {
    let (rig, device) = rig_with(
        "off",
        &[],
        Profile {
            noise: ANC.wire,
            ..Profile::default()
        },
    );
    let watch = rig.watch();
    let first = watch
        .next(HANG_LIMIT)
        .expect("the state watch prints first");
    rig.link_up(&device);
    let last = rig.wait_status("the burst's noise mode", |status| {
        status["noise_mode"] == ANC.status
    });
    let mut lines = vec![first.state];
    lines.extend(watch.lines_until(&last));
    let groups: Vec<&str> = lines
        .iter()
        .zip(lines.iter().skip(1))
        .map(|(before, after)| changed_group(before, after))
        .collect();
    assert_eq!(
        groups,
        [LINK, BATTERIES, EARS, NAME_AND_MODEL, NOISE_MODE],
        "{lines:#?}"
    );
    assert_eq!(lines.last(), Some(&rig.status()));
    rig.finish();
}

#[test]
fn a_subscriber_that_never_reads_is_dropped_and_status_still_answers() {
    let mut rig = Rig::new();
    let device = rig.add_pods(Profile::default());
    rig.start();
    rig.link_up(&device);
    rig.wait_filled();
    let mut silent = UnixStream::connect(rig.session().socket()).unwrap();
    silent.write_all(SUBSCRIBE).unwrap();
    silent.set_read_timeout(Some(HANG_LIMIT)).unwrap();
    // Read byte by byte up to the newline, so the reply is taken and no event behind it; sample input `{"id":1,"ok":true,"result":{}}`.
    let mut reply = Vec::new();
    while reply.last() != Some(&b'\n') {
        let mut byte = [0_u8; 1];
        silent.read_exact(&mut byte).unwrap();
        reply.extend(byte);
    }
    let subscribed: Value = serde_json::from_slice(&reply)
        .unwrap_or_else(|error| panic!("the subscribe reply: {error}: {reply:?}"));
    // The daemon follows the hub before it replies, so every change from here on finds the subscriber.
    assert_eq!(subscribed["ok"], true, "{subscribed}");
    for change in 0..CHANGES {
        let mode = if change.is_multiple_of(2) {
            ANC
        } else {
            TRANSPARENCY
        };
        device.pods.set_setting(NOISE, mode.wire);
    }
    // The last report on the channel, so its level in the status means every change before it was taken.
    device.pods.set_battery(NEW_LEVEL, NEW_LEVEL, NEW_LEVEL);
    for call in 0..STATUS_CALLS {
        let reply = Proto1::connect(rig.session()).ask(GET);
        assert_eq!(reply["ok"], true, "{reply}");
        // The burst ends with the new level, so the daemon answered this get while it took the changes.
        if call == 0 {
            assert_ne!(reply["state"]["left"]["level"], NEW_LEVEL.level, "{reply}");
        }
    }
    let taken = rig.wait_status("every change taken", |status| {
        status["left"]["level"] == NEW_LEVEL.level
    });
    assert_eq!(taken["noise_mode"], TRANSPARENCY.status);
    let read = String::from_utf8(read_to_close(&mut silent)).unwrap();
    let events = read
        .lines()
        .filter(|line| line.starts_with(r#"{"event""#))
        .count();
    assert!(events > 0, "the subscriber got no event");
    assert!(events < CHANGES, "the subscriber got all {events} events");
    rig.finish();
}

#[test]
fn an_oversized_sdu_is_dropped_and_the_link_stays_up() {
    let mut rig = Rig::new();
    let device = rig.add_pods(Profile::default());
    rig.start();
    rig.link_up(&device);
    rig.wait_filled();
    let mut oversized = metadata_of(OVERSIZED_NAME, MODEL_NUMBER);
    oversized.resize(OVERSIZED_SDU, FILLER);
    device.pods.send(&oversized);
    device.pods.set_battery(NEW_LEVEL, NEW_LEVEL, NEW_LEVEL);
    let status = rig.wait_status("the new battery level", |status| {
        status["left"]["level"] == NEW_LEVEL.level
    });
    assert_eq!(status["device_name"], Profile::default().name);
    assert_eq!(rig.dials().len(), 1, "{:?}", rig.dials());
    rig.finish();
}

#[test]
fn random_bytes_from_the_pods_never_stop_the_link() {
    let mut rng = TestRng::deterministic_rng(RngAlgorithm::ChaCha);
    let mut rig = Rig::new();
    let device = rig.add_pods(Profile::default());
    rig.start();
    rig.link_up(&device);
    rig.wait_filled();
    for count in 0..RANDOM_SDUS {
        // At least one byte: a zero byte SDU reads as the close on a sequenced socket.
        let mut sdu = vec![0; rng.random_range(1..=RANDOM_SDU_MAX)];
        rng.fill(&mut sdu[..]);
        if count.is_multiple_of(2) {
            let opcode = PARSED_OPCODES[rng.random_range(0..PARSED_OPCODES.len())];
            let mut header = ACCESSORY_HEADER.to_vec();
            header.extend([opcode, 0x00]);
            sdu.splice(0..0, header);
            sdu.truncate(RANDOM_SDU_MAX);
        }
        device.pods.send(&sdu);
    }
    device.pods.set_battery(NEW_LEVEL, NEW_LEVEL, NEW_LEVEL);
    rig.wait_status("the new battery level", |status| {
        status["left"]["level"] == NEW_LEVEL.level
    });
    assert_eq!(rig.dials().len(), 1, "{:?}", rig.dials());
    rig.finish();
}

#[test]
fn reports_before_the_acknowledgement_show_while_disconnected() {
    let mut rig = Rig::new();
    let device = rig.add_pods(Profile::default());
    device.pods.fault(Fault::ReportsBeforeAck);
    rig.start();
    let up = Instant::now();
    rig.link_up(&device);
    let left = Profile::default().left;
    let status = wait_within("the left battery", SHOWN_WITHIN, || {
        let status = rig.status();
        (status["left"]["level"] == left.level).then_some(status)
    });
    assert_gap(
        "the left battery after the link came up",
        up,
        Instant::now(),
        &(Duration::ZERO..=SHOWN_WITHIN),
    );
    assert_eq!(status["left"]["charging"], left.charging);
    assert!(!connected(&status), "{status}");
    rig.finish();
}

#[test]
fn the_notification_request_is_repeated_once_without_a_battery_report() {
    let mut rig = Rig::new();
    let device = rig.add_pods(Profile::default());
    device.pods.fault(Fault::WithholdBattery);
    rig.start();
    rig.link_up(&device);
    let request = host::request_notifications();
    let heard = device
        .pods
        .wait_heard(&request, 2, HANG_LIMIT)
        .unwrap_or_else(|deadline| panic!("a second request not heard: {deadline:?}"));
    // The daemon starts the wait on the features acknowledgement, which the pods send after hearing this.
    let features = device
        .pods
        .wait_heard(&host::set_specific_features(), 1, Duration::ZERO)
        .unwrap();
    assert_gap(
        "the second notification request after the features",
        features[0].at,
        heard[1].at,
        &(NOTIFY_REREQUEST..),
    );
    let third = device.pods.wait_heard(&request, 3, NO_THIRD_FOR);
    assert!(third.is_err(), "a third request: {third:?}");
    assert!(connected(&rig.status()));
    rig.finish();
}

#[test]
fn the_notification_request_is_not_repeated_after_a_battery_report() {
    let mut rig = Rig::new();
    let device = rig.add_pods(Profile::default());
    rig.start();
    rig.link_up(&device);
    let request = host::request_notifications();
    let first = device
        .pods
        .wait_heard(&request, 1, HANG_LIMIT)
        .unwrap_or_else(|deadline| panic!("no notification request heard: {deadline:?}"));
    let features = device
        .pods
        .wait_heard(&host::set_specific_features(), 1, Duration::ZERO)
        .unwrap();
    // Well before a re-request could be due, so this is the first request and not its repeat.
    assert_gap(
        "the notification request after the features",
        features[0].at,
        first[0].at,
        &(Duration::ZERO..=REQUEST_WITHIN),
    );
    rig.wait_filled();
    let second = device
        .pods
        .wait_heard(&request, 2, *not_before(NOTIFY_REREQUEST).end());
    assert!(second.is_err(), "a second request: {second:?}");
    rig.finish();
}
