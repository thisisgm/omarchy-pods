//! The coarse clock row of one start: its figure with all it says of itself, and each refusal with its reason.

use pods_proto::sdulog::Record;

use super::super::super::super::input::CutLines;
use super::super::super::super::support::{NS_PER_MS, NS_PER_SECOND, stall};
use super::super::super::analyse;
use super::super::super::tests::{one, shown, start_at_zero, steady};

/// What every refusal of a coarse clock ends with.
const OPENING: &str = "counted from 1 s after the first audio SDU";
/// Why a span whose halves disagree gives no figure.
const UNSTEADY: &str = "the delay of the arrivals changes inside it";

fn coarse_row(records: &[Record]) -> (String, String) {
    let report = analyse(records, &start_at_zero(), "trials", CutLines::default());
    let (key, value, unit, note) = shown(one(&report, "clock_hz_coarse"));
    assert_eq!((key, unit), ("clock_hz_coarse", "Hz"));
    (value.to_owned(), note.to_owned())
}

#[test]
fn a_figure_says_what_it_was_counted_from_and_what_it_assumes() {
    let said = "start 1; units over a span of 3.930 s x 480 samples per unit; 132 audio SDUs, counted from 1 s after the first audio SDU; halves 0.00 percent apart, of 10 allowed; the pace of the arrivals, which is the clock only once the backlog is delivered; 480 samples per unit assumed, not measured";
    let steady_cycle = steady(64_000, 5);
    assert_eq!(
        coarse_row(&steady_cycle),
        ("64000".to_owned(), said.to_owned())
    );
    // 300 ms held inside the span leave an arrival interval over 90 ms, a gap, and move neither end of a half.
    let mut held_inside = steady_cycle;
    stall(&mut held_inside, 2 * NS_PER_SECOND, 300 * NS_PER_MS);
    assert_eq!(
        coarse_row(&held_inside),
        ("64000".to_owned(), format!("{said}; gap"))
    );
}

#[test]
fn a_refusal_says_what_the_span_held_or_how_far_apart_its_halves_are() {
    // Four seconds at 64 kHz leave 99 SDUs over 2.94 s after the opening.
    let thin = format!("start 1; 99 audio SDUs over 2.940 s; needs 75 over 3 s; {OPENING}");
    assert_eq!(coarse_row(&steady(64_000, 4)), ("refused".to_owned(), thin));
    // The last second, held until 1.5 s after its end, makes the second half of the span far slower than the first.
    let mut held_at_the_end = steady(64_000, 5);
    stall(
        &mut held_at_the_end,
        4 * NS_PER_SECOND,
        5 * NS_PER_SECOND / 2,
    );
    let (value, note) = coarse_row(&held_at_the_end);
    assert_eq!(value, "refused");
    let (apart, rest) = note
        .strip_prefix("start 1; halves of the span ")
        .and_then(|rest| rest.split_once(' '))
        .unwrap();
    assert!(apart.parse::<f64>().unwrap() > 10.0, "{note}");
    assert_eq!(
        rest,
        format!("percent apart, of 10 allowed: {UNSTEADY}; {OPENING}")
    );
    // The last 70 SDUs come with the last one, so the second half of the span has no time in it.
    let mut at_one_instant = steady(64_000, 5);
    let last_ns = at_one_instant.last().unwrap().at_ns;
    let from = at_one_instant.len() - 70;
    for record in &mut at_one_instant[from..] {
        record.at_ns = last_ns;
    }
    let no_time = format!("start 1; a half of the span has no time in it: {UNSTEADY}; {OPENING}");
    assert_eq!(coarse_row(&at_one_instant), ("refused".to_owned(), no_time));
}
