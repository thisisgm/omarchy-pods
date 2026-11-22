//! Support tests: the pure classification under the watcher, whose proof is the end to end suite.

use super::*;

fn address(last: u8) -> BdAddr {
    BdAddr::from([0x02, 0x00, 0x00, 0x00, 0x00, last])
}

fn pods(last: u8, connected: bool) -> Properties {
    Properties {
        address: Some(address(last)),
        lists_aap: Some(true),
        connected: Some(connected),
    }
}

fn device(path: &str, properties: Properties) -> Change {
    Change::Device {
        path: path.to_owned(),
        properties,
    }
}

fn connected(value: bool) -> Properties {
    Properties {
        connected: Some(value),
        ..Properties::default()
    }
}

/// The events as text, since `DeviceEvent` carries addresses that print redacted.
fn named(events: &[DeviceEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| match event {
            DeviceEvent::Up(device) => format!("up {:?}", device.octets()[5]),
            DeviceEvent::Down(device) => format!("down {:?}", device.octets()[5]),
            DeviceEvent::Gone(device) => format!("gone {:?}", device.octets()[5]),
            DeviceEvent::Lost => "lost".to_owned(),
            DeviceEvent::CallFailed(call) => format!("call failed {:?}", call.action),
        })
        .collect()
}

const FIRST: &str = "/org/bluez/hci0/dev_02_00_00_00_00_01";
const SECOND_ADAPTER: &str = "/org/bluez/hci1/dev_02_00_00_00_00_01";

#[test]
fn a_device_without_the_uuid_is_not_a_candidate() {
    let mut candidates = Candidates::default();
    let headset = Properties {
        lists_aap: Some(false),
        ..pods(1, true)
    };
    assert!(candidates.apply(device(FIRST, headset)).is_empty());
    assert!(!candidates.holds(FIRST));
    assert!(candidates.apply(device(FIRST, connected(false))).is_empty());
    assert!(
        candidates
            .apply(Change::Swept(vec![(FIRST.to_owned(), headset)]))
            .is_empty()
    );
    assert!(!candidates.holds(FIRST));
}

#[test]
fn uuids_arriving_after_connected_make_it_up() {
    let mut candidates = Candidates::default();
    let paired = Properties {
        lists_aap: Some(false),
        ..pods(1, true)
    };
    assert!(candidates.apply(device(FIRST, paired)).is_empty());
    assert_eq!(
        named(&candidates.apply(device(FIRST, pods(1, true)))),
        ["up 1"]
    );
    assert!(candidates.apply(device(FIRST, connected(true))).is_empty());
    assert_eq!(
        named(&candidates.apply(device(FIRST, connected(false)))),
        ["down 1"]
    );
}

#[test]
fn a_removed_candidate_is_gone() {
    let mut candidates = Candidates::default();
    assert_eq!(
        named(&candidates.apply(device(FIRST, pods(1, false)))),
        ["down 1"]
    );
    let removed = Change::Removed {
        path: FIRST.to_owned(),
    };
    assert_eq!(named(&candidates.apply(removed.clone())), ["gone 1"]);
    assert!(candidates.apply(removed).is_empty());
}

#[test]
fn a_sweep_that_drops_a_candidate_reports_it_gone() {
    let mut candidates = Candidates::default();
    let second = "/org/bluez/hci0/dev_02_00_00_00_00_02";
    candidates.apply(device(FIRST, pods(1, true)));
    candidates.apply(device(second, pods(2, false)));
    let swept = Change::Swept(vec![(second.to_owned(), pods(2, false))]);
    assert_eq!(named(&candidates.apply(swept)), ["gone 1", "down 2"]);
    assert!(!candidates.holds(FIRST));
}

#[test]
fn a_seventeenth_candidate_is_not_kept() {
    let mut candidates = Candidates::default();
    let paths: Vec<String> = (1..=17)
        .map(|last| format!("/org/bluez/hci0/dev_{last}"))
        .collect();
    for (last, path) in (1..).zip(&paths[..KNOWN_MAX]) {
        assert_eq!(candidates.apply(device(path, pods(last, false))).len(), 1);
    }
    assert!(
        candidates
            .apply(device(&paths[KNOWN_MAX], pods(17, true)))
            .is_empty()
    );
    assert!(!candidates.holds(&paths[KNOWN_MAX]));
    let swept = (1..)
        .zip(&paths)
        .map(|(last, path)| (path.clone(), pods(last, false)));
    assert_eq!(
        candidates.apply(Change::Swept(swept.collect())).len(),
        KNOWN_MAX
    );
}

#[test]
fn an_address_that_does_not_parse_is_skipped() {
    let properties = HashMap::from([
        (
            "Address".to_owned(),
            OwnedVariant::try_from(Variant::from("not an address")).unwrap(),
        ),
        (
            "UUIDs".to_owned(),
            OwnedVariant::try_from(Variant::from(vec![AAP_UUID])).unwrap(),
        ),
        ("Connected".to_owned(), OwnedVariant::from(true)),
    ]);
    let read = Properties::read(&properties);
    assert_eq!(
        read,
        Properties {
            address: None,
            lists_aap: Some(true),
            connected: Some(true)
        }
    );
    let mut candidates = Candidates::default();
    assert!(candidates.apply(device(FIRST, read)).is_empty());
    assert!(!candidates.holds(FIRST));
}

#[test]
fn one_address_under_two_paths_is_down_only_when_neither_is_connected() {
    let mut candidates = Candidates::default();
    assert_eq!(
        named(&candidates.apply(device(FIRST, pods(1, true)))),
        ["up 1"]
    );
    assert!(
        candidates
            .apply(device(SECOND_ADAPTER, pods(1, false)))
            .is_empty()
    );
    assert!(
        candidates
            .apply(device(SECOND_ADAPTER, connected(true)))
            .is_empty()
    );
    assert!(candidates.apply(device(FIRST, connected(false))).is_empty());
    assert_eq!(
        named(&candidates.apply(device(SECOND_ADAPTER, connected(false)))),
        ["down 1"]
    );
    let swept = vec![
        (FIRST.to_owned(), pods(1, false)),
        (SECOND_ADAPTER.to_owned(), pods(1, true)),
    ];
    assert_eq!(named(&candidates.apply(Change::Swept(swept))), ["up 1"]);
    let first = Change::Removed {
        path: FIRST.to_owned(),
    };
    assert!(candidates.apply(first).is_empty());
    let second = Change::Removed {
        path: SECOND_ADAPTER.to_owned(),
    };
    assert_eq!(named(&candidates.apply(second)), ["gone 1"]);
    assert_eq!(named(&candidates.apply(Change::Lost)), ["lost"]);
}
