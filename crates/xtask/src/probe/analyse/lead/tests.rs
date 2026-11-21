//! How far the first frame of a stream lies before its start command, and the burst that delivers the backlog.

use super::super::support::{NS_PER_MS, NS_PER_SECOND, close};
use super::*;

/// The nominal duration of one frame, 7.5 ms.
const FRAME_NS: f64 = 7_500_000.0;
/// The same duration as a whole number, for building arrival times.
const FRAME_WHOLE_NS: u64 = 7_500_000;
/// Frames in each packet of the built streams.
const FRAMES_PER_PACKET: u64 = 4;
/// The arrival interval of the built streams, four frames.
const PACKET_NS: u64 = FRAMES_PER_PACKET * FRAME_WHOLE_NS;
/// When the start command goes out in the built streams.
const START_NS: u64 = 5 * NS_PER_SECOND;
/// Packets in six seconds of a built stream.
const PACKETS: u64 = 200;
/// How close a lead must come to the planted one, in milliseconds.
const WITHIN_MS: f64 = 1.0;
/// The fewest packets a lead is worked out from.
const FEWEST_PACKETS: u64 = 50;

/// `packets` packets of a stream whose first frame was captured at `origin_ns`, each arriving as its last frame ends.
fn on_the_line(origin_ns: u64, packets: u64) -> Vec<Arrival> {
    (1..=packets)
        .map(|k| Arrival {
            at_ns: origin_ns.saturating_add(k.saturating_mul(PACKET_NS)),
            frames_done: k.saturating_mul(FRAMES_PER_PACKET),
        })
        .collect()
}

#[test]
fn lead_is_zero_when_the_first_unit_is_captured_at_start() {
    let found = lead(&on_the_line(START_NS, PACKETS), START_NS, FRAME_NS).unwrap();
    assert!(close(found.lead_ms, 0.0, 1e-6), "{found:?}");
    assert_eq!((found.burst_packets, found.burst_frames), (0, 0));
    assert!(close(found.burst_ms, 0.0, 1e-9), "{found:?}");
}

#[test]
fn lead_finds_a_planted_burst() {
    const PRE_ROLL_NS: u64 = 900 * NS_PER_MS;
    const BURST_FROM_NS: u64 = START_NS + 80 * NS_PER_MS;
    const BURST_UNTIL_NS: u64 = START_NS + 330 * NS_PER_MS;
    let mut arrivals = on_the_line(START_NS - PRE_ROLL_NS, PACKETS);
    let backlog = arrivals
        .iter()
        .filter(|arrival| arrival.at_ns < BURST_UNTIL_NS)
        .count();
    let backlog = u64::try_from(backlog).unwrap();
    assert_eq!(backlog, 40);
    for (k, arrival) in (0..backlog).zip(&mut arrivals) {
        arrival.at_ns = BURST_FROM_NS + (BURST_UNTIL_NS - BURST_FROM_NS) * k / backlog;
    }
    assert!(arrivals.is_sorted_by_key(|arrival| arrival.at_ns));

    let found = lead(&arrivals, START_NS, FRAME_NS).unwrap();
    assert!(close(found.lead_ms, 900.0, WITHIN_MS), "{found:?}");
    let burst_end_ms = 80.0 + found.burst_ms;
    assert!(close(burst_end_ms, 330.0, 30.0), "{found:?}");
    assert!((38..=40).contains(&found.burst_packets), "{found:?}");
    let burst_packets = u64::from(found.burst_packets);
    assert_eq!(
        u64::from(found.burst_frames),
        burst_packets * FRAMES_PER_PACKET
    );
}

#[test]
fn lead_is_negative_by_the_start_delay_when_there_is_no_burst() {
    const START_DELAY_NS: u64 = 80 * NS_PER_MS;
    let arrivals = on_the_line(START_NS + START_DELAY_NS, PACKETS);
    let found = lead(&arrivals, START_NS, FRAME_NS).unwrap();
    assert!(close(found.lead_ms, -80.0, 1e-6), "{found:?}");
    assert_eq!((found.burst_packets, found.burst_frames), (0, 0));
}

#[test]
fn lead_is_the_earliest_arrival_line_not_the_first_packet() {
    const LATE_NS: u64 = 50 * NS_PER_MS;
    let mut arrivals = on_the_line(START_NS, PACKETS);
    arrivals[0].at_ns += LATE_NS;
    let found = lead(&arrivals, START_NS, FRAME_NS).unwrap();
    assert!(close(found.lead_ms, 0.0, 1e-6), "{found:?}");
    let burst_frames = u64::from(found.burst_frames);
    assert_eq!((found.burst_packets, burst_frames), (1, FRAMES_PER_PACKET));
    assert!(close(found.burst_ms, 0.0, 1e-9), "{found:?}");

    arrivals[1].at_ns += LATE_NS;
    arrivals[2].at_ns += LATE_NS;
    let found = lead(&arrivals, START_NS, FRAME_NS).unwrap();
    assert!(close(found.lead_ms, 0.0, 1e-6), "{found:?}");
    let burst_frames = u64::from(found.burst_frames);
    assert_eq!(
        (found.burst_packets, burst_frames),
        (3, 3 * FRAMES_PER_PACKET)
    );
    assert!(close(found.burst_ms, 60.0, 1e-9), "{found:?}");
}

#[test]
fn a_packet_only_three_frames_late_is_not_a_burst() {
    let mut arrivals = on_the_line(START_NS, PACKETS);
    arrivals[0].at_ns += 3 * FRAME_WHOLE_NS;
    let found = lead(&arrivals, START_NS, FRAME_NS).unwrap();
    assert_eq!(found.burst_packets, 0, "{found:?}");
    arrivals[0].at_ns += 1;
    let found = lead(&arrivals, START_NS, FRAME_NS).unwrap();
    assert_eq!(found.burst_packets, 1, "{found:?}");
}

#[test]
fn a_stall_does_not_change_the_lead() {
    const START_DELAY_NS: u64 = 80 * NS_PER_MS;
    const STALL_AT_NS: u64 = START_NS + 2 * NS_PER_SECOND;
    const STALL_NS: u64 = 300 * NS_PER_MS;
    let mut arrivals = on_the_line(START_NS + START_DELAY_NS, PACKETS);
    let before = lead(&arrivals, START_NS, FRAME_NS).unwrap();
    let mut stalled = 0;
    for arrival in &mut arrivals {
        if (STALL_AT_NS..STALL_AT_NS + STALL_NS).contains(&arrival.at_ns) {
            arrival.at_ns = STALL_AT_NS + STALL_NS;
            stalled += 1;
        }
    }
    assert_eq!(stalled, 10);
    let after = lead(&arrivals, START_NS, FRAME_NS).unwrap();
    assert!(close(before.lead_ms, -80.0, 1e-6), "{before:?}");
    assert_eq!(after, before);
}

#[test]
fn lead_takes_its_line_from_the_first_ten_seconds_only() {
    const TEN_SECONDS_PACKETS: u64 = 400;
    let mut arrivals = on_the_line(START_NS, TEN_SECONDS_PACKETS);
    let early = arrivals.last_mut().unwrap();
    assert!(early.at_ns >= START_NS + 10 * NS_PER_SECOND);
    early.at_ns -= 20 * NS_PER_MS;
    let found = lead(&arrivals, START_NS, FRAME_NS).unwrap();
    assert!(close(found.lead_ms, 0.0, 1e-6), "{found:?}");

    let inside = &mut arrivals[300];
    assert!(inside.at_ns < START_NS + 10 * NS_PER_SECOND);
    inside.at_ns -= 20 * NS_PER_MS;
    let found = lead(&arrivals, START_NS, FRAME_NS).unwrap();
    assert!(close(found.lead_ms, 20.0, 1e-6), "{found:?}");
}

#[test]
fn lead_needs_fifty_packets() {
    assert_eq!(lead(&[], START_NS, FRAME_NS), None);
    let too_few = on_the_line(START_NS, FEWEST_PACKETS - 1);
    assert_eq!(lead(&too_few, START_NS, FRAME_NS), None);
    let enough = on_the_line(START_NS, FEWEST_PACKETS);
    assert!(lead(&enough, START_NS, FRAME_NS).is_some());

    let mut late_tail = on_the_line(START_NS, 2 * FEWEST_PACKETS);
    for arrival in late_tail.iter_mut().skip(too_few.len()) {
        arrival.at_ns += 10 * NS_PER_SECOND;
    }
    assert_eq!(lead(&late_tail, START_NS, FRAME_NS), None);
}

#[test]
fn the_window_ends_ten_seconds_after_the_start_and_leaves_that_instant_out() {
    const WINDOW_END_NS: u64 = START_NS + 10 * NS_PER_SECOND;
    let mut arrivals = on_the_line(START_NS, FEWEST_PACKETS);
    arrivals.last_mut().unwrap().at_ns = WINDOW_END_NS;
    assert_eq!(lead(&arrivals, START_NS, FRAME_NS), None);
    arrivals.last_mut().unwrap().at_ns = WINDOW_END_NS - 1;
    assert!(lead(&arrivals, START_NS, FRAME_NS).is_some());
}

#[test]
fn lead_refuses_a_frame_duration_that_is_no_positive_number() {
    let arrivals = on_the_line(START_NS, PACKETS);
    for frame_ns in [0.0, -7_500_000.0, f64::NAN, f64::INFINITY] {
        assert_eq!(lead(&arrivals, START_NS, frame_ns), None, "{frame_ns}");
    }
}

#[test]
fn lead_follows_the_frame_duration_it_is_given() {
    /// A frame a microsecond short of nominal, which over the 800 frames built puts the nominal line 0.8 ms early.
    const SHORT_FRAME_NS: u32 = 7_499_000;
    let arrivals: Vec<Arrival> = (1..=PACKETS)
        .map(|k| Arrival {
            at_ns: START_NS + k * FRAMES_PER_PACKET * u64::from(SHORT_FRAME_NS),
            frames_done: k * FRAMES_PER_PACKET,
        })
        .collect();
    let found = lead(&arrivals, START_NS, f64::from(SHORT_FRAME_NS)).unwrap();
    assert!(close(found.lead_ms, 0.0, 1e-6), "{found:?}");
    let nominal = lead(&arrivals, START_NS, FRAME_NS).unwrap();
    assert!(close(nominal.lead_ms, 0.8, 1e-6), "{nominal:?}");
}
