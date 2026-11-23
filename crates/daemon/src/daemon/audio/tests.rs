//! Support tests: which sink is the device's and how its name starts; no user path yet for the sink it reports.

mod restore;
mod waits;

use pods_core::policy::audio::{Percent, SinkFact, sink_prefix};
use pods_proto::addr::BdAddr;

use super::wire::tests::{FIRST_INDEX, sink_list};
use super::{Followed, device_sink};

const DEVICE: &str = "02:00:00:00:00:0A";
const SPEAKERS: &[u8] = b"alsa_output.speakers";
const OUTPUT: &[u8] = b"bluez_output.02_00_00_00_00_0A.1";
const HEADSET: &[u8] = b"bluez_output.02_00_00_00_00_0A.2";
const OTHER_PODS: &[u8] = b"bluez_output.02_00_00_00_00_01.1";
/// Half of the protocol's full volume.
const HALF: u32 = 0x8000;
/// The protocol's full volume.
const NORM: u32 = 0x10000;

fn prefix() -> Vec<u8> {
    sink_prefix(DEVICE.parse::<BdAddr>().unwrap()).into_bytes()
}

fn found(is_default: bool, percent: u8) -> SinkFact {
    let volume = Percent::new(percent).unwrap();
    SinkFact { is_default, volume }
}

fn device_sink_of(default: Option<&[u8]>) -> Option<SinkFact> {
    device_sink_of_list(default).map(|sink| sink.fact)
}

fn device_sink_of_list(default: Option<&[u8]>) -> Option<Followed> {
    let list = sink_list(&[
        (c"alsa_output.speakers", &[NORM]),
        (c"bluez_output.02_00_00_00_00_01.1", &[NORM]),
        (c"bluez_output.02_00_00_00_00_0A.1", &[HALF]),
        (c"bluez_output.02_00_00_00_00_0A.2", &[NORM]),
    ]);
    device_sink(&list, &prefix(), default).unwrap()
}

#[test]
fn the_default_is_the_devices_sink_when_its_name_starts_with_the_prefix() {
    assert_eq!(device_sink_of(Some(OUTPUT)), Some(found(true, 50)));
    assert_eq!(device_sink_of(Some(HEADSET)), Some(found(true, 100)));
}

#[test]
fn another_default_leaves_the_devices_first_sink() {
    assert_eq!(device_sink_of(Some(SPEAKERS)), Some(found(false, 50)));
    assert_eq!(device_sink_of(Some(OTHER_PODS)), Some(found(false, 50)));
    assert_eq!(device_sink_of(None), Some(found(false, 50)));
}

#[test]
fn the_devices_sink_keeps_its_index_for_the_restore() {
    let found = device_sink_of_list(Some(OUTPUT)).unwrap();
    assert_eq!(found.index, FIRST_INDEX + 2);
}

#[test]
fn a_list_without_the_devices_sink_has_none() {
    let list = sink_list(&[(c"alsa_output.speakers", &[NORM])]);
    assert_eq!(device_sink(&list, &prefix(), Some(SPEAKERS)), Ok(None));
    assert_eq!(device_sink(&[], &prefix(), None), Ok(None));
}
