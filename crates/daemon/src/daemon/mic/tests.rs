use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use pods_core::policy::{MicFact, OpenId};
use pods_mic::{NODE_LATENCY_MS, RATE_HZ, SAMPLES_PER_UNIT, STAMP_STEP};
use pods_pw::Event;
use pods_pwrig::Server;
use pods_samples::uplink::tone_unit;
use tokio::runtime::Builder;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use super::{
    Adapter, LOST, MIC_EVENT_QUEUE, MicRequest, PAUSED, Request, STREAMING, Tap, Turn, Wanted,
    next, watcher,
};
use crate::daemon::audio::AUDIO_RETRY_MS;

fn adapter() -> Adapter {
    let (wake, _wakes) = mpsc::channel(MIC_EVENT_QUEUE);
    Adapter {
        socket: PathBuf::new(),
        tap: Tap::new(),
        reader: watch::channel(None).0,
        wake,
        wanted: None,
        opened: None,
        before_card_update: || {},
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
    let (_, _, resync) = pods_mic::chain(Arc::default()).unwrap();
    let mut watched = watcher(Arc::clone(&events), wake, resync);
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

/// The tone's line and level: any unit that decodes to sound will do.
const TONE_LINE: u16 = 10;
const TONE_LEVEL: u8 = 180;

/// Whether a reader reaches the end of three units landed at zero in two reads a unit apart, after the watcher sees `event`.
fn reads_to_the_end(event: Event) -> bool {
    let (wake, _wakes) = mpsc::channel(1);
    let (mut chain, mut reader, resync) = pods_mic::chain(Arc::default()).unwrap();
    let mut watched = watcher(Arc::new(AtomicU8::new(PAUSED)), wake, resync);
    let tone = tone_unit(TONE_LINE, TONE_LEVEL);
    let stamps = [0, STAMP_STEP, 2 * STAMP_STEP];
    chain.sdu(
        Duration::ZERO,
        stamps.into_iter().map(|stamp| (stamp, tone.as_slice())),
    );
    watched(event);
    let latency = Duration::from_millis(u64::from(NODE_LATENCY_MS));
    let unit_time = Duration::from_secs(u64::from(SAMPLES_PER_UNIT))
        .checked_div(RATE_HZ)
        .unwrap();
    let mut heard = vec![0.0_f32; usize::from(SAMPLES_PER_UNIT)];
    // A reader left where it stood is a unit short at the end; one moved to its target is not.
    reader.read(latency.checked_sub(unit_time).unwrap(), &mut heard);
    reader.read(latency, &mut heard);
    reader.read_all()
}

#[test]
fn only_a_stream_start_moves_the_reader_to_its_target() {
    assert!(reads_to_the_end(Event::Streaming));
    for event in [Event::Paused, Event::Lost] {
        assert!(!reads_to_the_end(event), "{event:?}");
    }
}

#[test]
fn a_broken_library_ends_the_want_so_no_retry_follows() {
    let mut adapter = adapter();
    adapter.wanted = Some(Wanted {
        id: OpenId::default(),
        name: "node".to_owned(),
        description: "description".to_owned(),
        device: None,
    });
    assert_eq!(adapter.broken(&"the library failed"), MicFact::Broken);
    assert!(adapter.wanted.is_none());
    // The call a retry turn makes: with nothing wanted it tries nothing, so no miss and no retry.
    let runtime = Builder::new_current_thread().build().unwrap();
    assert_eq!(runtime.block_on(adapter.open()), None);
    assert_eq!((adapter.misses, adapter.retry_at), (0, None));
}

#[test]
fn a_panicked_card_update_clears_the_reader() {
    const CARD_ID: u32 = 86;
    let directory = tempfile::tempdir().unwrap();
    let server = Server::start(directory.path());
    let runtime = Builder::new_current_thread().enable_time().build().unwrap();
    let mut adapter = adapter();
    adapter.socket = server.socket();
    let reader = adapter.reader.subscribe();
    let device = "02:00:00:00:00:01".parse().unwrap();
    let id = OpenId::default();

    runtime.block_on(async {
        let request = Request {
            node: MicRequest::Open {
                id,
                name: "test-mic".to_owned(),
                description: "Test microphone".to_owned(),
            },
            device: Some(device),
        };
        assert_eq!(adapter.serve(request).await, Some(MicFact::Up(id)));
        assert!(reader.borrow().is_some());

        adapter.before_card_update = || panic!("injected card update panic");
        assert_eq!(
            adapter.update_card(Some((device, CARD_ID))).await,
            Some(MicFact::Down(id))
        );
        assert!(
            reader.borrow().is_none(),
            "the reader still names the dropped source"
        );
    });
}

#[test]
fn a_request_is_read_before_a_wake_already_waiting() {
    let runtime = Builder::new_current_thread().enable_time().build().unwrap();
    let (requests_in, mut requests) = watch::channel(Request::CLOSE);
    let (_card_ids, mut cards) = watch::channel(None);
    let (wake, mut wakes) = mpsc::channel(1);
    wake.try_send(()).unwrap();
    let open = Request {
        node: MicRequest::Open {
            id: OpenId::default(),
            name: "node".to_owned(),
            description: "description".to_owned(),
        },
        device: None,
    };
    requests_in.send(Request::CLOSE).unwrap();
    requests_in.send(open.clone()).unwrap();
    let turn = runtime.block_on(next(&mut requests, &mut wakes, &mut cards, None));
    assert!(matches!(turn, Turn::Request));
    assert_eq!(*requests.borrow_and_update(), open);
    let turn = runtime.block_on(next(&mut requests, &mut wakes, &mut cards, None));
    assert!(matches!(turn, Turn::Woken));
    drop(requests_in);
    let turn = runtime.block_on(next(&mut requests, &mut wakes, &mut cards, None));
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

#[test]
fn a_retry_wakes_after_the_audio_adapter_ends() {
    let runtime = Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap();
    let (_requests_in, mut requests) = watch::channel(Request::CLOSE);
    let (card_ids, mut cards) = watch::channel(None);
    drop(card_ids);
    let (_wake, mut wakes) = mpsc::channel(1);
    let turn = runtime.block_on(async {
        let [first, ..] = AUDIO_RETRY_MS;
        let retry_at = Instant::now()
            .checked_add(Duration::from_millis(first))
            .unwrap();
        // The retry wakes the same pending turn after the card watch has closed.
        next(&mut requests, &mut wakes, &mut cards, Some(retry_at)).await
    });
    assert!(matches!(turn, Turn::Retry));
}
