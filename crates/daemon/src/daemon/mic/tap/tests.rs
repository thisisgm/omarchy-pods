use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pods_mic::{Resync, STAMP_STEP};
use pods_proto::dictation::{AudioNotification, DictationPacket};
use pods_proto::doap::Handles;
use pods_proto::uplink::{AudioSdu, UplinkPacket};
use pods_pw::Fill;
use pods_samples::uplink::{audio_sdu, tone_unit};
use pods_samples::{doap, opus};

use super::{Feed, Tap};

const UNITS: u32 = 4;
const LINE: u16 = 15;
const LEVEL: u8 = 176;
/// Reads of 20 ms at the node's rate, far more than the ring holds.
const READS: usize = 2000;
const FRAMES: usize = 1280;

/// A packet of tone units whose first stamp is `place` packets in.
fn packet(place: u32) -> Vec<u8> {
    let units = vec![tone_unit(LINE, LEVEL); usize::try_from(UNITS).unwrap()];
    audio_sdu(place.wrapping_mul(UNITS * STAMP_STEP), &units)
}

fn audio(sdu: &[u8]) -> AudioSdu<'_> {
    let Ok(Some(UplinkPacket::Audio(audio))) = UplinkPacket::parse(sdu) else {
        panic!("not an audio SDU");
    };
    audio
}

/// A dictation notification of one tone frame under `counter`.
fn notification(counter: u16) -> Vec<u8> {
    let frame = opus::tone().next().unwrap();
    doap::audio(&doap::Handles::OBSERVED, 0, &[(counter, frame)])
}

fn dictated(pdu: &[u8]) -> AudioNotification<'_> {
    let Ok(Some(DictationPacket::Audio(audio))) = DictationPacket::parse(pdu, &Handles::OBSERVED)
    else {
        panic!("not a dictation audio notification");
    };
    audio
}

fn concealed(tap: &Tap) -> u32 {
    tap.counters().concealed.load(Ordering::Relaxed)
}

/// A chain the tap takes, as when its node opens.
fn open(tap: &Tap) -> (Feed, Resync) {
    let (chain, feed, resync) = tap.chain().unwrap();
    tap.open(chain, || Ok::<(), ()>(())).unwrap();
    (feed, resync)
}

/// A node whose drop records whether the tap still held a chain then.
struct Node {
    tap: Arc<Tap>,
    chain_at_drop: Arc<AtomicBool>,
}

impl Drop for Node {
    fn drop(&mut self) {
        let held = self.tap.lock().chain.is_some();
        self.chain_at_drop.store(held, Ordering::Relaxed);
    }
}

#[test]
fn audio_with_no_chain_is_counted_and_dropped() {
    let tap = Tap::new();
    for place in [0, 2] {
        assert!(!tap.audio(&audio(&packet(place))));
    }
    assert_eq!(tap.packets(), 2);
    assert_eq!(concealed(&tap), 0);
}

#[test]
fn the_units_reach_the_chain_in_one_call_each_sdu() {
    let tap = Tap::new();
    let (_feed, _resync) = open(&tap);
    tap.audio(&audio(&packet(0)));
    // A packet lost between these two is concealed unit by unit.
    tap.audio(&audio(&packet(2)));
    assert_eq!(concealed(&tap), UNITS);
    tap.close(());
    tap.audio(&audio(&packet(4)));
    assert_eq!((tap.packets(), concealed(&tap)), (3, UNITS));
}

#[test]
fn a_node_that_fails_to_open_takes_no_chain() {
    let tap = Tap::new();
    let (chain, _feed, _resync) = tap.chain().unwrap();
    assert_eq!(tap.open(chain, || Err::<(), ()>(())), Err(()));
    tap.audio(&audio(&packet(0)));
    // With a chain this lost packet would be concealed.
    tap.audio(&audio(&packet(2)));
    assert_eq!(concealed(&tap), 0);
}

#[test]
fn a_closed_node_is_dropped_after_the_tap_lets_go_of_its_chain() {
    let tap = Tap::new();
    let (_feed, _resync) = open(&tap);
    let chain_at_drop = Arc::new(AtomicBool::new(true));
    tap.close(Node {
        tap: Arc::clone(&tap),
        chain_at_drop: Arc::clone(&chain_at_drop),
    });
    assert!(!chain_at_drop.load(Ordering::Relaxed));
}

#[test]
fn an_arm_starts_a_new_run_at_the_next_audio_and_reports_it_once() {
    let tap = Tap::new();
    let (_feed, _resync) = open(&tap);
    assert!(!tap.audio(&audio(&packet(0))));
    tap.arm();
    // With no new run this stamp would stand one packet past the one expected.
    assert!(tap.audio(&audio(&packet(2))));
    assert!(!tap.audio(&audio(&packet(3))));
    assert_eq!(concealed(&tap), 0);
}

#[test]
fn a_dictation_start_begins_a_dictation_run_at_the_next_audio_and_reports_it_once() {
    let tap = Tap::new();
    let (_feed, _resync) = open(&tap);
    // On the uplink's run a dictation frame is a stale packet: the one lost before it is not concealed.
    assert!(!tap.dictation(&dictated(&notification(0))));
    assert!(!tap.dictation(&dictated(&notification(2))));
    assert_eq!(concealed(&tap), 0);
    tap.arm_dictation();
    assert!(tap.dictation(&dictated(&notification(10))));
    assert!(!tap.dictation(&dictated(&notification(12))));
    assert_eq!(concealed(&tap), 1);
}

#[test]
fn a_node_opened_after_the_first_dictation_audio_still_gets_its_run() {
    let tap = Tap::new();
    tap.arm_dictation();
    assert!(tap.dictation(&dictated(&notification(0))));
    let (_feed, _resync) = open(&tap);
    assert!(!tap.dictation(&dictated(&notification(1))));
    assert!(!tap.dictation(&dictated(&notification(3))));
    assert_eq!(
        concealed(&tap),
        1,
        "the chain took the frames as a dictation run"
    );
}

#[test]
fn a_reopened_node_keeps_dictation_until_an_uplink_start() {
    let tap = Tap::new();
    let (feed, _resync) = open(&tap);
    tap.arm_dictation();
    assert!(tap.dictation(&dictated(&notification(0))));
    assert!(!feed.reader.read_all());
    tap.close(feed);
    assert!(!tap.dictation(&dictated(&notification(1))));

    let (feed, _resync) = open(&tap);
    assert!(!tap.audio(&audio(&packet(1))));
    assert!(
        feed.reader.read_all(),
        "uplink plays while dictation is selected"
    );
    assert!(!tap.dictation(&dictated(&notification(2))));
    assert!(
        !feed.reader.read_all(),
        "the reopened node has audio to play"
    );

    tap.arm();
    assert!(tap.audio(&audio(&packet(0))));
    assert!(!tap.audio(&audio(&packet(2))));
    assert_eq!(concealed(&tap), UNITS);
    tap.close(feed);

    let (feed, _resync) = open(&tap);
    assert!(!tap.dictation(&dictated(&notification(3))));
    assert!(feed.reader.read_all(), "late dictation left the node empty");
    assert!(!tap.audio(&audio(&packet(3))));
    assert!(!feed.reader.read_all(), "uplink plays on the reopened node");
}

#[test]
fn dictation_audio_is_counted_apart_from_the_uplink() {
    let tap = Tap::new();
    tap.dictation(&dictated(&notification(0)));
    tap.dictation(&dictated(&notification(1)));
    tap.audio(&audio(&packet(0)));
    assert_eq!((tap.dictation_packets(), tap.packets()), (2, 1));
}

#[test]
fn an_uplink_start_and_a_dictation_start_each_wait_for_their_own_audio() {
    let tap = Tap::new();
    tap.arm();
    tap.arm_dictation();
    assert!(tap.dictation(&dictated(&notification(0))));
    assert!(tap.audio(&audio(&packet(0))));
}

#[test]
fn the_feed_never_allocates() {
    let tap = Tap::new();
    let (mut feed, resync) = open(&tap);
    for place in 0..8 {
        tap.audio(&audio(&packet(place)));
    }
    resync.request();
    let mut out = [0.0_f32; FRAMES];
    let reading = allocation_counter::measure(|| {
        for _ in 0..READS {
            black_box(feed.fill(&mut out));
        }
    });
    assert_eq!(reading.count_total, 0);
    // The counter does count in this binary: one vector is one allocation.
    let control = allocation_counter::measure(|| drop(black_box(vec![0_u8; FRAMES])));
    assert_eq!(control.count_total, 1);
}
