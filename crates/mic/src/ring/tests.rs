//! Support tests: the one word that holds the run's arrival or none, the step between time and frames, and how far a write runs past the index it shows.

use super::{Arrival, NS_PER_SECOND, Ring, UNIT_FRAMES, scaled};

#[test]
fn time_and_frames_convert_at_a_rate_that_does_not_divide_a_second() {
    // A unit is 7.5 ms at either rate, and a frame at 48 kHz is 20 833 ns and a third.
    for (rate, frames, ns) in [
        (64_000, 480, 7_500_000),
        (48_000, 360, 7_500_000),
        (48_000, 1, 20_833),
    ] {
        assert_eq!(
            scaled(frames, NS_PER_SECOND, rate),
            ns,
            "{frames} at {rate}"
        );
    }
    for (rate, ns, frames) in [
        (64_000, 7_500_000, 480),
        (48_000, 7_500_000, 360),
        (48_000, 7_499_999, 359),
        // A time before the run's arrival rounds down, not toward zero.
        (48_000, -1, -1),
    ] {
        assert_eq!(scaled(ns, rate, NS_PER_SECOND), frames, "{ns} ns at {rate}");
    }
    // What no word holds stays at the end it left by.
    assert_eq!(scaled(i64::MAX, NS_PER_SECOND, 48_000), i64::MAX);
    assert_eq!(scaled(i64::MIN, NS_PER_SECOND, 48_000), i64::MIN);
}

#[test]
fn an_arrival_is_never_read_back_as_none() {
    let word = Arrival::none();
    assert_eq!(word.load(), None);
    for ns in [i64::MAX, 0, -1, i64::MIN + 1] {
        word.store(Some(ns));
        assert_eq!(word.load(), Some(ns));
    }
    // The one value the word keeps for none is still an arrival when it is stored: one ns later.
    word.store(Some(i64::MIN));
    assert_eq!(word.load(), Some(i64::MIN + 1));
    word.store(None);
    assert_eq!(word.load(), None);
}

#[test]
fn a_write_longer_than_a_unit_shows_its_index_every_unit() {
    let ring = Ring::new();
    let unit = i64::from(UNIT_FRAMES);
    let long = 3 * unit + 7;
    // The reader's lap check allows the writer one unit past the index it shows, so no sample may be stored further on than that.
    let mut furthest = 0;
    ring.write((0..long).map(|stored| {
        furthest = furthest.max(stored - ring.end());
        0.0
    }));
    assert_eq!(furthest, unit - 1);
    assert_eq!(ring.end(), long);
}
