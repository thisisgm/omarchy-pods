//! Support tests: the reading of `ManufacturerData`, the advert queue bound, the reported paths bound, one read per path and the change it keeps, the count of refused tries and the calls a cause leaves behind, as tables.

use std::collections::HashMap;
use std::future::poll_fn;
use std::task::Poll;

use pods_core::policy::{MonitorFact, Sighting};
use pods_proto::addr::BdAddr;
use pods_proto::proximity::ADVERT_LEN;
use proptest::prelude::*;
use tokio::sync::mpsc;
use tokio::task::yield_now;
use zbus::Variant;

use super::{
    ADVERT_DEVICES_MAX, ADVERT_QUEUE, APPLE, MONITOR_TRIES, Monitor, apple_advert, objects, offer,
};
use crate::daemon::link::Input;
use crate::daemon::link::tests::block_on;

const ADDRESS: &str = "02:00:00:00:00:01";
const ADAPTER: &str = "/org/bluez/hci0";
const OTHER_ADAPTER: &str = "/org/bluez/hci1";
/// A company that is not Apple.
const OTHER_COMPANY: u16 = 0x0006;
/// Room for every advert the bound lets in, and more.
const INPUT_ROOM: usize = 64;
/// Entries drawn up to this length, so lengths on both sides of `ADVERT_LEN` come up.
const ENTRY_LONGEST: usize = 64;

fn address() -> BdAddr {
    ADDRESS.parse().unwrap()
}

fn data(len: usize) -> Vec<u8> {
    (0..len).map(|at| u8::try_from(at).unwrap()).collect()
}

fn manufacturer_data(company: u16, bytes: Vec<u8>) -> Variant<'static> {
    Variant::from(HashMap::from([(company, Variant::from(bytes))]))
}

fn read_entry(len: usize) -> Option<[u8; ADVERT_LEN]> {
    apple_advert(&manufacturer_data(APPLE, data(len)))
}

#[test]
fn an_apple_entry_of_the_advert_length_or_more_is_read_by_its_first_bytes() {
    let first: [u8; ADVERT_LEN] = data(ADVERT_LEN).try_into().unwrap();
    let wanted = Some(first);
    assert_eq!(read_entry(ADVERT_LEN), wanted);
    assert_eq!(read_entry(ADVERT_LEN + 4), wanted);
    for len in 0..ADVERT_LEN {
        assert_eq!(read_entry(len), None, "{len}");
    }
}

#[test]
fn another_company_or_another_shape_is_dropped() {
    let other = manufacturer_data(OTHER_COMPANY, data(ADVERT_LEN));
    let text = Variant::from(HashMap::from([(APPLE, Variant::from("not bytes"))]));
    let words = Variant::from(HashMap::from([(
        APPLE,
        Variant::from(vec![1_u16; ADVERT_LEN]),
    )]));
    let flat = Variant::from(data(ADVERT_LEN));
    for (row, variant) in [
        ("another company", other),
        ("text", text),
        ("words", words),
        ("no dict", flat),
    ] {
        assert_eq!(apple_advert(&variant), None, "{row}");
    }
}

#[test]
fn the_one_monitor_matches_apple_proximity_adverts() {
    let objects = objects();
    assert_eq!(objects.len(), 1);
    let monitor = objects.values().next().unwrap();
    let properties = &monitor["org.bluez.AdvertisementMonitor1"];
    assert_eq!(properties["Type"], Variant::from("or_patterns"));
    let pattern = vec![(0_u8, 0xff_u8, vec![0x4c_u8, 0x00, 0x07])];
    assert_eq!(properties["Patterns"], Variant::from(pattern));
}

#[test]
fn an_advert_is_queued_only_while_fewer_than_the_bound_wait() {
    let sighting = Sighting {
        address: address(),
        data: read_entry(ADVERT_LEN).unwrap(),
    };
    let (inputs, mut taken) = mpsc::channel(INPUT_ROOM);
    for _ in 0..INPUT_ROOM {
        offer(&inputs, sighting);
    }
    let mut queued = 0_usize;
    while let Ok(Input::Advert(_)) = taken.try_recv() {
        queued = queued.saturating_add(1);
    }
    assert_eq!(queued, ADVERT_QUEUE);
    for _ in 0..ADVERT_QUEUE {
        inputs
            .try_send(Input::Monitor(MonitorFact::Registered))
            .unwrap();
    }
    offer(&inputs, sighting);
    let mut adverts = 0_usize;
    while let Ok(input) = taken.try_recv() {
        if matches!(input, Input::Advert(_)) {
            adverts = adverts.saturating_add(1);
        }
    }
    assert_eq!(adverts, 0);
}

#[test]
fn the_reported_paths_keep_the_newest_up_to_the_bound_and_each_once() {
    let mut monitor = Monitor::default();
    let path = |at: usize| format!("{ADAPTER}/dev_{at}");
    let read = |at: usize| Some((path(at), Some((address(), None))));
    assert_eq!(monitor.read_ended(read(0)), None);
    assert_eq!(monitor.address_of(&path(0)), None, "not wanted");
    monitor.set_wanted(true);
    for at in 0..=ADVERT_DEVICES_MAX {
        assert_eq!(monitor.read_ended(read(at)), None);
        assert_eq!(monitor.read_ended(read(at)), None);
    }
    assert_eq!(monitor.reported.len(), ADVERT_DEVICES_MAX);
    assert_eq!(monitor.address_of(&path(0)), None);
    assert_eq!(monitor.address_of(&path(1)), Some(address()));
    monitor.forget_device(&path(1));
    assert_eq!(monitor.address_of(&path(1)), None);
    let advert = read_entry(ADVERT_LEN);
    let found = Some((path(0), Some((address(), advert))));
    let sighting = advert.map(|data| Sighting {
        address: address(),
        data,
    });
    assert_eq!(monitor.read_ended(found), sighting);
}

#[test]
fn one_read_per_path_at_most_the_queue_bound_and_none_unwanted() {
    let mut monitor = Monitor::default();
    let path = |at: usize| format!("{ADAPTER}/dev_{at}");
    assert!(!monitor.begin_read(&path(0)), "not wanted");
    monitor.set_wanted(true);
    assert!(monitor.begin_read(&path(0)));
    assert!(!monitor.begin_read(&path(0)), "already under read");
    for at in 1..ADVERT_QUEUE {
        assert!(monitor.begin_read(&path(at)), "{at}");
    }
    assert!(!monitor.begin_read(&path(ADVERT_QUEUE)), "the bound");
    assert_eq!(
        monitor.read_ended(Some((path(0), None))),
        None,
        "a failed read"
    );
    assert!(monitor.begin_read(&path(0)), "read again after it failed");
    assert!(!monitor.begin_read(&path(ADVERT_QUEUE)), "the bound again");
}

#[test]
fn a_change_during_the_first_read_wins_over_its_reply_and_later_ones_go_at_once() {
    let mut monitor = Monitor::default();
    monitor.set_wanted(true);
    let path = |at: usize| format!("{ADAPTER}/dev_{at}");
    let advert = |first: u8| {
        let mut bytes = data(ADVERT_LEN);
        bytes[0] = first;
        (
            manufacturer_data(APPLE, bytes.clone()),
            <[u8; ADVERT_LEN]>::try_from(bytes).unwrap(),
        )
    };
    let ((older, older_bytes), (newer, newer_bytes), (latest, latest_bytes)) =
        (advert(1), advert(2), advert(3));
    assert!(monitor.begin_read(&path(0)));
    assert_eq!(
        monitor.changed(&path(0), &newer),
        None,
        "kept while under read"
    );
    assert_eq!(
        monitor.changed(&path(1), &older),
        None,
        "neither reported nor under read"
    );
    assert_eq!(monitor.reading.len(), 1);
    let read = Some((path(0), Some((address(), Some(older_bytes)))));
    let kept = Sighting {
        address: address(),
        data: newer_bytes,
    };
    assert_eq!(monitor.read_ended(read), Some(kept));
    assert_eq!(monitor.reading.len(), 0);
    let at_once = Sighting {
        address: address(),
        data: latest_bytes,
    };
    assert_eq!(monitor.changed(&path(0), &latest), Some(at_once));
    assert!(monitor.begin_read(&path(1)));
    let unchanged = Some((path(1), Some((address(), Some(older_bytes)))));
    let replied = Sighting {
        address: address(),
        data: older_bytes,
    };
    assert_eq!(
        monitor.read_ended(unchanged),
        Some(replied),
        "no change kept: the reply"
    );
}

fn refusal() -> (String, Result<(), String>) {
    (ADAPTER.to_owned(), Err("refused".to_owned()))
}

fn acceptance() -> (String, Result<(), String>) {
    (OTHER_ADAPTER.to_owned(), Ok(()))
}

/// The count of refused tries, each ending with its one call: what `ended` returns and whether a retry waits.
fn refused_tries(accepted_first: bool) -> Vec<(Option<MonitorFact>, bool)> {
    block_on(async {
        let mut monitor = Monitor::default();
        monitor.set_wanted(true);
        if accepted_first {
            assert_eq!(
                monitor.ended(Some(acceptance())),
                Some(MonitorFact::Registered)
            );
        }
        let mut seen = Vec::new();
        for _ in 0..MONITOR_TRIES {
            let fact = monitor.ended(Some(refusal()));
            let waiting = poll_fn(|context| {
                let pending = matches!(monitor.poll(context), Poll::Pending);
                Poll::Ready(pending && monitor.retry.is_some())
            })
            .await;
            seen.push((fact, waiting));
            // As if the wait ran out and the retry's one call is the next to end.
            monitor.retry = None;
        }
        seen
    })
}

#[test]
fn the_fifth_refused_try_reads_unavailable_unless_an_adapter_accepted() {
    let before_last = usize::try_from(MONITOR_TRIES.saturating_sub(1)).unwrap();
    let mut retries = vec![(None, true); before_last];
    retries.push((Some(MonitorFact::Unavailable), false));
    assert_eq!(refused_tries(false), retries);
    let mut kept = vec![(None, true); before_last];
    kept.push((None, false));
    assert_eq!(refused_tries(true), kept);
}

#[test]
fn an_acceptance_ending_the_last_try_still_reads_registered() {
    block_on(async {
        let mut monitor = Monitor::default();
        monitor.set_wanted(true);
        for _ in 1..MONITOR_TRIES {
            assert_eq!(monitor.ended(Some(refusal())), None);
            monitor.retry = None;
        }
        // `ADAPTER` refused this try too; the other adapter's acceptance is the last call to end.
        assert!(monitor.refused.contains(ADAPTER));
        assert_eq!(
            monitor.ended(Some(acceptance())),
            Some(MonitorFact::Registered)
        );
    });
}

#[test]
fn a_call_or_read_ended_before_bluez_is_forgotten_is_never_joined() {
    block_on(async {
        let mut monitor = Monitor::default();
        monitor.set_wanted(true);
        let call = monitor.calls.spawn(async { Some(acceptance()) });
        let path = format!("{ADAPTER}/dev_0");
        let read = monitor
            .reads
            .spawn(async move { (path, Some((address(), None))) });
        while !call.is_finished() || !read.is_finished() {
            yield_now().await;
        }
        assert_eq!(monitor.forget_bluez(), None);
        let pending = poll_fn(|context| Poll::Ready(monitor.poll(context).is_pending())).await;
        assert!(pending);
    });
}

proptest! {
    #[test]
    fn arbitrary_entries_never_panic(company in prop_oneof![Just(APPLE), any::<u16>()], bytes in proptest::collection::vec(any::<u8>(), 0..ENTRY_LONGEST)) {
        let read = apple_advert(&manufacturer_data(company, bytes.clone()));
        let wanted = (company == APPLE && bytes.len() >= ADVERT_LEN)
            .then(|| <[u8; ADVERT_LEN]>::try_from(&bytes[..ADVERT_LEN]).unwrap());
        prop_assert_eq!(read, wanted);
    }
}
