use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use pods_core::policy::{MicFact, OpenId};
use pods_pw::Event;
use tokio::runtime::Builder;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use super::{Adapter, LOST, MIC_EVENT_QUEUE, MicRequest, PAUSED, STREAMING, Turn, next, watcher};
use crate::daemon::audio::AUDIO_RETRY_MS;

fn adapter() -> Adapter {
    let (wake, _wakes) = mpsc::channel(MIC_EVENT_QUEUE);
    Adapter {
        socket: PathBuf::new(),
        wake,
        wanted: None,
        opened: None,
        misses: 0,
        retry_at: None,
    }
}

#[test]
fn each_miss_waits_the_next_step_of_the_ladder_then_stays_on_its_last() {
    let mut adapter = adapter();
    let [.., longest] = AUDIO_RETRY_MS;
    for wait in AUDIO_RETRY_MS.into_iter().chain([longest, longest]) {
        let wait = Duration::from_millis(wait);
        let before = Instant::now();
        assert_eq!(
            adapter.retry(OpenId::default()),
            MicFact::Down(OpenId::default())
        );
        let after = Instant::now();
        let at = adapter.retry_at.unwrap();
        let earliest = before.checked_add(wait).unwrap();
        let latest = after.checked_add(wait).unwrap();
        assert!(earliest <= at && at <= latest, "{wait:?}");
    }
}

#[test]
fn a_lost_source_stays_lost_and_a_full_wake_queue_never_blocks() {
    let events = Arc::new(AtomicU8::new(PAUSED));
    let (wake, mut wakes) = mpsc::channel(1);
    let mut watched = watcher(Arc::clone(&events), wake);
    let seen = |wakes: &mpsc::Receiver<()>| (events.load(Ordering::Acquire), wakes.len());
    watched(Event::Streaming);
    assert_eq!(seen(&wakes), (STREAMING, 1));
    // Into a full queue: the event is stored and the call returns.
    watched(Event::Paused);
    assert_eq!(seen(&wakes), (PAUSED, 1));
    assert_eq!(wakes.try_recv(), Ok(()));
    watched(Event::Lost);
    assert_eq!(seen(&wakes), (LOST, 1));
    watched(Event::Streaming);
    assert_eq!(seen(&wakes), (LOST, 1));
}

#[test]
fn a_broken_library_ends_the_want_so_no_retry_follows() {
    let mut adapter = adapter();
    adapter.wanted = Some((
        OpenId::default(),
        "node".to_owned(),
        "description".to_owned(),
    ));
    assert_eq!(adapter.broken(&"the library failed"), MicFact::Broken);
    assert_eq!(adapter.wanted, None);
    // The call a retry turn makes: with nothing wanted it tries nothing, so no miss and no retry.
    let runtime = Builder::new_current_thread().build().unwrap();
    assert_eq!(runtime.block_on(adapter.open()), None);
    assert_eq!((adapter.misses, adapter.retry_at), (0, None));
}

#[test]
fn a_request_is_read_before_a_wake_already_waiting() {
    let runtime = Builder::new_current_thread().enable_time().build().unwrap();
    let (requests_in, mut requests) = watch::channel(MicRequest::Close);
    let (wake, mut wakes) = mpsc::channel(1);
    wake.try_send(()).unwrap();
    let open = MicRequest::Open {
        id: OpenId::default(),
        name: "node".to_owned(),
        description: "description".to_owned(),
    };
    requests_in.send(MicRequest::Close).unwrap();
    requests_in.send(open.clone()).unwrap();
    let turn = runtime.block_on(next(&mut requests, &mut wakes, None));
    assert!(matches!(turn, Turn::Request));
    assert_eq!(*requests.borrow_and_update(), open);
    let turn = runtime.block_on(next(&mut requests, &mut wakes, None));
    assert!(matches!(turn, Turn::Woken));
    drop(requests_in);
    let turn = runtime.block_on(next(&mut requests, &mut wakes, None));
    assert!(matches!(turn, Turn::Ended));
}

#[test]
fn the_outage_suite_waits_the_ladders_first_step() {
    let suite = include_str!("../../../tests/source.rs");
    let [first, ..] = AUDIO_RETRY_MS;
    let copy = format!("const FIRST_RETRY: Duration = Duration::from_millis({first});");
    let live = suite.lines().any(|line| line.trim() == copy);
    assert!(live, "tests/source.rs FIRST_RETRY is not {first} ms");
}
