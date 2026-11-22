//! The figures of generated sessions against their plans, the two bounds of thin data, and bytes no parser reads.

use pods_proto::sdulog::{Direction, Record};
use proptest::prelude::*;

use super::super::input::MarkKind;
use super::super::lead::{Arrival, Lead};
use super::super::load::Window;
use super::super::stats::to_f64;
use super::super::support::{close, mark};
use super::session::{
    AfterStop, FLOOR, FRAME_MS, Heard, Tail, after_burst, bitrate, frame_clock, lead_of,
};
use super::synth::{
    AUDIO_AFTER_MS, FLAGGED, NS_PER_MS, NS_PER_SECOND, Plan, REPLY_AFTER_MS, STOP_AFTER_MS,
    STOP_REPLY_AFTER_MS, synth,
};
use super::*;

const START_NS: u64 = 2 * NS_PER_SECOND;
/// The longest packet the property test feeds, one receive buffer.
const LONGEST_PACKET: usize = 4096;
/// The most frames of the session the property test damages, three seconds of them.
const DAMAGED_FRAMES: u32 = 150;
/// The frame bytes every hand placed packet brings, and the bits they come to.
const PACKET_BYTES: u64 = 81;
const PACKET_BITS: u64 = PACKET_BYTES * 8;

fn logged_at(instants: impl IntoIterator<Item = u64>) -> Vec<Heard> {
    let numbered = (1_u64..).zip(instants);
    let heard = numbered.map(|(frames_done, at_ns)| Heard {
        arrival: Arrival { at_ns, frames_done },
        bytes: PACKET_BYTES,
    });
    heard.collect()
}

fn evenly(packets: u64, span_ns: u64) -> Vec<Heard> {
    let gaps = packets.saturating_sub(1);
    logged_at((0..packets).map(|k| span_ns.saturating_mul(k).checked_div(gaps).unwrap_or(0)))
}

/// The windows a capture would mark around a generated session: its start write to its stop write, and from there on.
fn marked(made: &synth::Session) -> (Window, Window) {
    let window = Window {
        start_ns: made.start_ns,
        end_ns: Some(made.stop_ns),
    };
    let tail = Window {
        start_ns: made.stop_ns,
        end_ns: None,
    };
    (window, tail)
}

/// How many packets of `heard` end no later than the last frame of the burst `plan` opens with.
fn burst_packets(plan: Plan, heard: &[Heard]) -> usize {
    let last = u64::from(plan.burst_frames);
    let burst = heard
        .iter()
        .filter(|packet| packet.arrival.frames_done <= last);
    burst.count()
}

#[test]
fn a_generated_session_is_cut_and_timed_as_its_plan_says() {
    let plan = Plan {
        late: (1, 2),
        ..Plan::seen(START_NS, 400)
    };
    let made = synth(plan);
    let (window, tail) = marked(&made);
    let session = Session::cut(&made.records, window, Some(tail));
    assert_eq!(session.reply_after_ns(), Some(REPLY_AFTER_MS * NS_PER_MS));
    assert_eq!(session.audio_after_ns(), Some(AUDIO_AFTER_MS * NS_PER_MS));
    // The three late packets of the plan count on from the last frame, so the session takes them.
    assert_eq!(session.stream.frames().count(), 403);
    assert_eq!(session.tail, Tail { taken: 3, left: 0 });
    assert_eq!(
        (session.packet_skips(), session.frame_skips()),
        (vec![], vec![])
    );
    // The counter is one byte.
    let packets = u8::try_from(session.stream.audio.len() % 256).unwrap();
    assert_eq!(made.next_packet, plan.first_packet.wrapping_add(packets));
    let flagged = session
        .stream
        .frames()
        .filter(|frame| frame.flag == FLAGGED);
    assert!((100..300).contains(&flagged.count()));
    // The one late packet before the reply ends a frame after the last of the session, which the stop write follows by `STOP_AFTER_MS`.
    let late_ms = u64::from(FRAME_MS) - STOP_AFTER_MS;
    let answered = AfterStop::Answered {
        reply_after_ns: (late_ms + STOP_REPLY_AFTER_MS) * NS_PER_MS,
        audio_after_reply: 2,
    };
    assert_eq!(session.after_stop, answered);

    let heard = session.heard();
    let lead = lead_of(&heard, session.start_ns).unwrap();
    assert!(close(lead.lead_ms, to_f64(plan.lead_ms), 1e-6), "{lead:?}");
    let burst = burst_packets(plan, &heard);
    assert_eq!(
        (lead.burst_packets, lead.burst_frames),
        (u32::try_from(burst).unwrap(), plan.burst_frames)
    );
    assert!(
        close(lead.burst_ms, to_f64(plan.burst_ms), 1e-6),
        "{lead:?}"
    );
    assert_eq!(after_burst(&heard, &lead).len(), heard.len() - burst);
    let past_the_end = Lead {
        burst_packets: u32::MAX,
        ..lead
    };
    assert_eq!(after_burst(&heard, &past_the_end), []);

    let bare = synth(plan.without_burst());
    let (window, _) = marked(&bare);
    let bare = Session::cut(&bare.records, window, None);
    assert_eq!(bare.after_stop, AfterStop::Unmarked);
    assert_eq!(bare.stream.frames().count(), 400);
    let lead = lead_of(&bare.heard(), bare.start_ns).unwrap();
    assert_eq!((lead.burst_packets, lead.burst_frames), (0, 0));
}

#[test]
fn a_steady_packet_joins_the_burst_only_when_it_finds_over_52_5_ms_owed() {
    /// A packet of one 20 ms frame this late finds exactly 52.5 ms owed.
    const LATE_NS: u64 = 32_500_000;
    let plan = Plan::seen(START_NS, 400);
    let made = synth(plan);
    let (window, _) = marked(&made);
    let session = Session::cut(&made.records, window, None);
    let heard = session.heard();
    let burst = burst_packets(plan, &heard);
    // The first packet after the burst comes late, and no later one is logged before it.
    let delayed = |by_ns: u64| {
        let mut heard = heard.clone();
        let at_ns = heard[burst].arrival.at_ns + by_ns;
        for packet in &mut heard[burst..] {
            packet.arrival.at_ns = packet.arrival.at_ns.max(at_ns);
        }
        lead_of(&heard, session.start_ns).unwrap()
    };
    let (packets, frames) = (u32::try_from(burst).unwrap(), plan.burst_frames);
    let found = delayed(LATE_NS);
    assert_eq!((found.burst_packets, found.burst_frames), (packets, frames));
    let found = delayed(LATE_NS + 1);
    assert_eq!(
        (found.burst_packets, found.burst_frames),
        (packets + 1, frames + 1)
    );
}

#[test]
fn a_rate_needs_a_minute_and_five_hundred_of_the_packets_it_is_given() {
    let enough = evenly(500, FLOOR.span_ns);
    assert!(frame_clock(&enough).is_ok() && bitrate(&enough).is_ok());
    let bits = bitrate(&enough).unwrap();
    assert_eq!(bits.seconds, 60);
    assert!(
        close(bits.mean, to_f64(499 * PACKET_BITS) / 60.0, 1e-6),
        "{bits:?}"
    );
    assert!(bits.second_min <= bits.second_median && bits.second_median <= bits.second_max);

    for thin in [
        evenly(499, FLOOR.span_ns),
        evenly(500, FLOOR.span_ns - 1),
        vec![],
    ] {
        let packets = thin.len();
        assert_eq!(frame_clock(&thin).unwrap_err().sdus, packets);
        assert_eq!(bitrate(&thin).unwrap_err().sdus, packets);
    }
}

#[test]
fn a_second_of_the_bitrate_takes_its_last_instant_and_the_first_one_the_instant_of_the_first_packet()
 {
    let repeat = |at_ns: u64, packets: usize| std::iter::repeat_n(at_ns, packets);
    let (second, minute) = (NS_PER_SECOND, [FLOOR.span_ns]);
    // 300 packets at the instant of the first one, which the first second takes.
    let together = repeat(0, 301).chain(repeat(2 * second, 198));
    let bits = bitrate(&logged_at(together.chain(minute))).unwrap();
    assert_eq!(bits.second_max, 300 * PACKET_BITS);
    // 198 packets on the end of the second second, which a second that took its first instant would hand to the third.
    let on_the_end = repeat(0, 1)
        .chain(repeat(second + 1, 200))
        .chain(repeat(2 * second, 198))
        .chain(repeat(3 * second, 100));
    let bits = bitrate(&logged_at(on_the_end.chain(minute))).unwrap();
    assert_eq!(bits.second_max, 398 * PACKET_BITS);
    // 499 packets on the last instant of the minute, which the last whole second takes.
    let at_the_close = repeat(0, 1).chain(repeat(FLOOR.span_ns, 499));
    let bits = bitrate(&logged_at(at_the_close)).unwrap();
    assert_eq!(bits.second_max, 499 * PACKET_BITS);
}

#[test]
fn a_stamp_at_the_end_of_time_costs_no_memory_for_its_empty_seconds() {
    let steady = (0..499).map(|packet| packet * u64::from(FRAME_MS) * NS_PER_MS);
    let bits = bitrate(&logged_at(steady.chain([u64::MAX]))).unwrap();
    assert_eq!(u64::try_from(bits.seconds), Ok(u64::MAX / NS_PER_SECOND));
    assert_eq!((bits.second_min, bits.second_median), (0, 0));
    // One packet each 20 ms puts fifty in every second the steady stretch fills.
    assert_eq!(bits.second_max, 50 * PACKET_BITS);
}

proptest! {
    #[test]
    fn any_bytes_are_analysed_and_every_packet_is_counted_once(
        packets in proptest::collection::vec(
            (any::<bool>(), proptest::collection::vec(any::<u8>(), 1..=LONGEST_PACKET)),
            0..12,
        ),
        marks in proptest::collection::vec(any::<bool>(), 0..4),
        frames in 0..DAMAGED_FRAMES,
        damage in proptest::collection::vec(
            (any::<prop::sample::Index>(), any::<prop::sample::Index>(), any::<u8>()),
            0..4,
        ),
    ) {
        // A generated session with a few bytes changed, so whole frames and damaged ones are fed beside bytes no parser reads.
        let mut records = synth(Plan::seen(START_NS, frames)).records;
        for (record, byte, value) in damage {
            let record = record.index(records.len());
            let record = &mut records[record];
            let byte = byte.index(record.sdu.len());
            record.sdu[byte] = value;
        }
        let second = |k: u64| START_NS.saturating_add(k.saturating_mul(NS_PER_SECOND));
        records.extend((0_u64..).zip(packets).map(|(k, (from_pods, sdu))| Record {
            at_ns: second(k),
            direction: if from_pods { Direction::FromPods } else { Direction::ToPods },
            sdu,
        }));
        records.sort_by_key(|record| record.at_ns);
        let marks: Vec<_> = (0_u64..)
            .zip(marks)
            .map(|(k, starts)| {
                let kind = if starts { MarkKind::DictationStart } else { MarkKind::DictationStop };
                mark(second(k), kind)
            })
            .collect();
        let whole = load(&records);
        let both_ways = whole.from_pods.saturating_add(whole.to_pods);
        prop_assert_eq!(usize::try_from(both_ways).unwrap(), records.len());
        let report = analyse(&records, &marks, false);
        let frames = whole.frames().count();
        prop_assert_eq!(report.frames.lines().count(), frames.saturating_add(1));
        prop_assert_eq!(report.handles.lines().count(), whole.handles.len().saturating_add(1));
    }
}
