//! Support tests: which sink is the device's and how its name starts; no user path yet for the sink it reports.

mod restore;
mod volume;
mod waits;

use pods_core::policy::audio::{Percent, SinkFact, SinkState, VolumeStamp, sink_prefix};
use pods_proto::addr::BdAddr;

use super::volume::device_sink;
use super::wire::tests::{FIRST_INDEX, sink_list};

const DEVICE: &str = "02:00:00:00:00:0A";
const SPEAKERS: &[u8] = b"alsa_output.speakers";
const OUTPUT: &[u8] = b"bluez_output.02_00_00_00_00_0A.1";
const HEADSET: &[u8] = b"bluez_output.02_00_00_00_00_0A.2";
const OTHER_PODS: &[u8] = b"bluez_output.02_00_00_00_00_01.1";
/// Half of the protocol's full volume.
const HALF: u32 = 0x8000;
/// The protocol's full volume.
const NORM: u32 = 0x10000;
const OUTPUT_INDEX: u32 = FIRST_INDEX + 2;
const HEADSET_INDEX: u32 = FIRST_INDEX + 3;

fn prefix() -> Vec<u8> {
    sink_prefix(DEVICE.parse::<BdAddr>().unwrap()).into_bytes()
}

fn found(index: u32, is_default: bool, percent: u8) -> SinkFact {
    let volume = Percent::new(percent).unwrap();
    let state = SinkState::Idle;
    SinkFact {
        index,
        is_default,
        volume,
        state,
        volume_stamp: VolumeStamp::default(),
    }
}

fn device_sink_of(default: Option<&[u8]>) -> Option<SinkFact> {
    let list = sink_list(&[
        (c"alsa_output.speakers", &[NORM]),
        (c"bluez_output.02_00_00_00_00_01.1", &[NORM]),
        (c"bluez_output.02_00_00_00_00_0A.1", &[HALF]),
        (c"bluez_output.02_00_00_00_00_0A.2", &[NORM]),
    ]);
    device_sink(&list, &prefix(), default)
        .unwrap()
        .map(|reading| reading.fact)
}

#[test]
fn the_default_is_the_devices_sink_when_its_name_starts_with_the_prefix() {
    assert_eq!(
        device_sink_of(Some(OUTPUT)),
        Some(found(OUTPUT_INDEX, true, 50))
    );
    assert_eq!(
        device_sink_of(Some(HEADSET)),
        Some(found(HEADSET_INDEX, true, 100))
    );
}

#[test]
fn another_default_leaves_the_devices_first_sink() {
    assert_eq!(
        device_sink_of(Some(SPEAKERS)),
        Some(found(OUTPUT_INDEX, false, 50))
    );
    assert_eq!(
        device_sink_of(Some(OTHER_PODS)),
        Some(found(OUTPUT_INDEX, false, 50))
    );
    assert_eq!(device_sink_of(None), Some(found(OUTPUT_INDEX, false, 50)));
}

#[test]
fn the_devices_sink_keeps_its_index_for_the_restore() {
    let found = device_sink_of(Some(OUTPUT)).unwrap();
    assert_eq!(found.index, OUTPUT_INDEX);
}

#[test]
fn a_list_without_the_devices_sink_has_none() {
    let list = sink_list(&[(c"alsa_output.speakers", &[NORM])]);
    assert_eq!(device_sink(&list, &prefix(), Some(SPEAKERS)), Ok(None));
    assert_eq!(device_sink(&[], &prefix(), None), Ok(None));
}
