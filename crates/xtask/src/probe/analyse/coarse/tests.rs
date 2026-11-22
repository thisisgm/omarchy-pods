//! The coarse clock of a short stretch: what it reads through, where it gives no figure, and the one stream it reads wrongly.

use pods_proto::sdulog::Record;
use pods_proto::uplink::encode_audio;

use super::super::backlog::with_backlog;
use super::super::lead::Arrival;
use super::super::load::load;
use super::super::support::{NS_PER_MS, NS_PER_SECOND, OPAQUE, SEED, close, from_pods, stall};
use super::super::synth::synth;
use super::*;

/// Samples in one unit of the generated streams.
const SAMPLES_PER_UNIT: u32 = 480;
/// Units in one SDU of the generated streams.
const UNITS_PER_SDU: u32 = 4;
/// The length of one cycle of the scripts with short starts.
const CYCLE_S: u32 = 5;
const MS_PER_SECOND: u32 = 1000;
/// How near the figure of a steady stream comes to the rate it was generated at.
const STEADY_WITHIN_HZ: f64 = 100.0;
/// The figures a decode labels 64 kHz: those within five percent of it.
const LABELLED_64_KHZ: std::ops::RangeInclusive<f64> = 60_800.0..=67_200.0;
/// The figures a decode labels 48 kHz.
const LABELLED_48_KHZ: std::ops::RangeInclusive<f64> = 45_600.0..=50_400.0;

fn steady(rate_hz: u32, seconds: u32, jitter_ms: u32) -> Vec<Record> {
    let units = (SAMPLES_PER_UNIT, UNITS_PER_SDU);
    synth(rate_hz, units.0, units.1, seconds, jitter_ms, SEED)
}

fn coarse_of(records: &[Record]) -> Result<CoarseClock, NoCoarse> {
    coarse_clock(&Arrival::of(&load(records)), SAMPLES_PER_UNIT)
}

/// One cycle of a peer clocked at `rate_hz` whose first sample is `lead_ms` older than its start and whose backlog comes an SDU every `pace_ms`.
fn cycle(rate_hz: u32, lead_ms: u32, pace_ms: u32) -> Vec<Record> {
    let recorded_s = CYCLE_S.saturating_add(lead_ms.div_ceil(MS_PER_SECOND));
    with_backlog(
        steady(rate_hz, recorded_s, 0),
        u64::from(lead_ms) * NS_PER_MS,
        u64::from(pace_ms) * NS_PER_MS,
        u64::from(CYCLE_S) * NS_PER_SECOND,
    )
}

#[test]
fn a_five_second_stream_gives_the_rate_it_was_generated_at() {
    for rate_hz in [64_000, 48_000, 56_000] {
        for jitter_ms in [0, 5] {
            let found = coarse_of(&steady(rate_hz, CYCLE_S, jitter_ms)).unwrap();
            assert!(
                close(found.hz(), f64::from(rate_hz), STEADY_WITHIN_HZ),
                "{rate_hz}: {found:?}"
            );
            assert!(found.span_ns >= COARSE_MIN_SPAN_NS && found.sdus >= COARSE_MIN_SDUS);
            assert!(found.halves_apart_percent < 1.0, "{found:?}");
        }
    }
}

#[test]
fn a_backlog_delivered_in_the_opening_does_not_raise_the_figure() {
    // 900 ms of backlog at 6 ms an SDU are delivered 150 ms into a 48 kHz stream; first to last, the stream would read 56.3 kHz.
    let found = coarse_of(&cycle(48_000, 900, 6)).unwrap();
    assert!(close(found.hz(), 48_000.0, STEADY_WITHIN_HZ), "{found:?}");
}

#[test]
fn a_backlog_still_draining_inside_the_span_gives_no_figure() {
    // 2400 ms of backlog at 20 ms an SDU, twice the pace of 48 kHz, last 2.36 s: a third of the span, which as a whole would read 64.5 kHz.
    let refused = coarse_of(&cycle(48_000, 2400, 20)).unwrap_err();
    let NoCoarse::Unsteady {
        halves_apart_percent: Some(apart),
    } = refused
    else {
        panic!("{refused:?}");
    };
    assert!(apart > HALVES_WITHIN_PERCENT, "{apart}");
}

#[test]
fn no_pace_of_a_backlog_of_up_to_a_second_passes_one_rate_for_the_other() {
    // A backlog comes no slower than it was recorded: an SDU of four units lasts 40 ms at 48 kHz and 30 ms at 64 kHz.
    for (rate_hz, slowest_pace_ms, other) in
        [(48_000, 40, LABELLED_64_KHZ), (64_000, 30, LABELLED_48_KHZ)]
    {
        let mut figures = 0_u32;
        for lead_ms in (0..=1000).step_by(50) {
            for pace_ms in 1..=slowest_pace_ms {
                if let Ok(found) = coarse_of(&cycle(rate_hz, lead_ms, pace_ms)) {
                    let passes = other.contains(&found.hz());
                    assert!(!passes, "{rate_hz} {lead_ms} {pace_ms}: {found:?}");
                    figures += 1;
                }
            }
        }
        assert!(figures > 0, "{rate_hz}: every cycle was refused");
    }
}

#[test]
fn a_larger_backlog_at_the_steady_pace_of_the_other_rate_reads_as_that_rate() {
    // 1700 ms of backlog at 30 ms an SDU, the pace of 64 kHz, last the whole cycle: no arrival tells this 48 kHz stream from a 64 kHz one.
    let found = coarse_of(&cycle(48_000, 1700, 30)).unwrap();
    assert!(close(found.hz(), 64_000.0, STEADY_WITHIN_HZ), "{found:?}");
}

#[test]
fn a_stall_at_an_end_of_the_span_gives_no_figure_and_one_inside_it_changes_nothing() {
    // The last second of a 64 kHz stream, held until 1.5 s after its end: first to last, the span would read 46 kHz.
    let mut held_at_the_end = steady(64_000, CYCLE_S, 0);
    stall(
        &mut held_at_the_end,
        4 * NS_PER_SECOND,
        5 * NS_PER_SECOND / 2,
    );
    let refused = coarse_of(&held_at_the_end).unwrap_err();
    assert!(matches!(refused, NoCoarse::Unsteady { .. }), "{refused:?}");
    // A second of a 48 kHz stream held across the end of the opening: the span opens with what was kept back and would read 61 kHz.
    let mut held_over_the_opening = steady(48_000, CYCLE_S, 0);
    stall(&mut held_over_the_opening, NS_PER_SECOND / 2, NS_PER_SECOND);
    let refused = coarse_of(&held_over_the_opening).unwrap_err();
    assert!(matches!(refused, NoCoarse::Unsteady { .. }), "{refused:?}");
    let whole = coarse_of(&steady(64_000, CYCLE_S, 0)).unwrap();
    let mut held_inside = steady(64_000, CYCLE_S, 0);
    stall(&mut held_inside, 2 * NS_PER_SECOND, 300 * NS_PER_MS);
    assert_eq!(coarse_of(&held_inside).unwrap(), whole);
}

#[test]
fn an_arrival_as_late_as_comes_without_a_gap_moves_the_figure_by_its_share_of_the_span() {
    /// The lateness that makes a 90 ms interval where 30 ms was due, the longest that is no gap.
    const LATE_NS: u64 = 60 * NS_PER_MS;
    let whole = coarse_of(&steady(64_000, CYCLE_S, 0)).unwrap();
    let mut late = steady(64_000, CYCLE_S, 0);
    late.last_mut().unwrap().at_ns += LATE_NS;
    let found = coarse_of(&late).unwrap();
    // 60 ms over a span of 3.93 s is 1.5 percent of it.
    let moved = (found.hz() / whole.hz() - 1.0).abs() * PERCENT;
    assert!(close(moved, 1.5, 0.1), "{moved}");
    assert!(found.halves_apart_percent < HALVES_WITHIN_PERCENT);
}

#[test]
fn a_middle_arrival_as_late_as_comes_without_a_gap_keeps_the_figure_and_a_later_one_loses_it() {
    /// The first 135 SDUs of a 64 kHz stream leave 101 over exactly 3 s after the opening, the shortest span.
    const SHORTEST: usize = 135;
    /// The middle SDU of that span is due here.
    const MIDDLE_DUE_NS: u64 = 2550 * NS_PER_MS;
    let held_for = |late_ms: u64| {
        let mut records = steady(64_000, CYCLE_S, 0);
        records.truncate(SHORTEST);
        stall(&mut records, MIDDLE_DUE_NS, late_ms * NS_PER_MS);
        coarse_of(&records)
    };
    // 60 ms late makes a 90 ms interval, the longest that is no gap: the halves read 128.2 and 138.9 units a second, 8.0 percent of 133.3.
    let kept = held_for(60).unwrap();
    assert_eq!(kept.span_ns, COARSE_MIN_SPAN_NS);
    assert!(close(kept.halves_apart_percent, 8.0, 0.05), "{kept:?}");
    assert!(close(kept.hz(), 64_000.0, STEADY_WITHIN_HZ), "{kept:?}");
    // 90 ms late is a gap: the halves read 125.8 and 141.8, 12.0 percent apart.
    let lost = held_for(90).unwrap_err();
    let NoCoarse::Unsteady {
        halves_apart_percent: Some(apart),
    } = lost
    else {
        panic!("{lost:?}");
    };
    assert!(close(apart, 12.0, 0.05), "{apart}");
}

#[test]
fn too_little_after_the_opening_gives_no_figure() {
    // Four seconds at 64 kHz leave 99 SDUs over 2.94 s after the opening: enough SDUs, too short a span.
    let refused = coarse_of(&steady(64_000, 4, 0)).unwrap_err();
    let NoCoarse::Thin(thin) = refused else {
        panic!("{refused:?}");
    };
    assert!(thin.sdus >= COARSE_MIN_SDUS && thin.span_ns < COARSE_MIN_SPAN_NS);
    // Five seconds at 16 kHz leave 32 SDUs over 3.72 s: a long enough span, too few SDUs.
    let refused = coarse_of(&steady(16_000, CYCLE_S, 0)).unwrap_err();
    let NoCoarse::Thin(thin) = refused else {
        panic!("{refused:?}");
    };
    assert!(thin.sdus < COARSE_MIN_SDUS && thin.span_ns >= COARSE_MIN_SPAN_NS);
    // Under a second of stream has no SDU after the opening at all.
    let nothing_after = Thin {
        sdus: 0,
        span_ns: 0,
    };
    assert_eq!(
        coarse_of(&steady(64_000, 1, 0)),
        Err(NoCoarse::Thin(nothing_after))
    );
}

#[test]
fn a_stretch_with_no_whole_unit_has_no_audio() {
    assert_eq!(coarse_of(&[]), Err(NoCoarse::NoAudio));
    let header_only = encode_audio(&OPAQUE, &[]).unwrap();
    let records: Vec<Record> = (1..=200)
        .map(|k| from_pods(k * 30 * NS_PER_MS, &header_only))
        .collect();
    assert_eq!(coarse_of(&records), Err(NoCoarse::NoAudio));
}

#[test]
fn a_half_that_arrives_at_one_instant_gives_no_figure() {
    // The last 70 of the 132 SDUs of the span come with the last one, so its second half has no time in it.
    let mut records = steady(64_000, CYCLE_S, 0);
    let last_ns = records.last().unwrap().at_ns;
    let from = records.len() - 70;
    for record in &mut records[from..] {
        record.at_ns = last_ns;
    }
    assert_eq!(
        coarse_of(&records),
        Err(NoCoarse::Unsteady {
            halves_apart_percent: None
        })
    );
}
