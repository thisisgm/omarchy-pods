//! How late each audio SDU of a start landed, against the window of arrivals the reader of `pods-mic` keeps. The unit tests are support for the tests of the built `probe analyse`.

use pods_mic::{
    ARRIVAL_WINDOW, Arrivals, MAX_UNITS_PER_SDU, RATE_HZ, SAMPLES_PER_UNIT, STAMP_STEP, unit_writes,
};

use super::load::AudioPacket;
use super::report::{MS, NONE, PACKETS, Row, labelled};
use super::stats::millis;

const NS_PER_SECOND: u32 = 1_000_000_000;
const NS_PER_SAMPLE: u32 = NS_PER_SECOND / RATE_HZ;
const _: () = assert!(NS_PER_SECOND.is_multiple_of(RATE_HZ));
const NS_PER_MS: u64 = 1_000_000;
/// The width of a bin of the histogram, and the step from one cover to the next.
const STEP_MS: u64 = 5;
/// The covers the table counts against; the bins run from 0 ms, and the last bin holds everything from the last cover on.
const FIRST_COVER_MS: u64 = 30;
const LAST_COVER_MS: u64 = 60;
/// Each percentile row: its key, and the share of the SDUs at or under its value as a fraction.
const PERCENTILES: [(&str, usize, usize); 3] = [
    ("lateness_p99_ms", 99, 100),
    ("lateness_p99_9_ms", 999, 1000),
    ("lateness_p99_99_ms", 9999, 10_000),
];

/// The lateness of each SDU that holds a whole unit, in ns, as the chain of `pods-mic` judges it: its arrival less the time of the samples written up to its end at `RATE_HZ`, less the smallest such offset of the `ARRIVAL_WINDOW` SDUs up to it; a stamp that starts a new run empties the window.
fn lateness(audio: &[AudioPacket]) -> Vec<u64> {
    let mut expected = None;
    let mut written = 0_u64;
    let mut window = Arrivals::default();
    let mut late = Vec::new();
    for sdu in audio.iter().filter(|sdu| !sdu.units.is_empty()) {
        for unit in sdu.units.iter().take(MAX_UNITS_PER_SDU) {
            let writes = unit_writes(expected, unit.stamp);
            if writes.new_run {
                window = Arrivals::default();
            }
            let samples = u64::from(writes.units).saturating_mul(u64::from(SAMPLES_PER_UNIT));
            written = written.saturating_add(samples);
            expected = Some(unit.stamp.wrapping_add(STAMP_STEP));
        }
        let stood_ns = written.saturating_mul(u64::from(NS_PER_SAMPLE));
        let ns = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
        let offset = ns(sdu.at_ns).saturating_sub(ns(stood_ns));
        let earliest = window.push(offset);
        late.push(u64::try_from(offset.saturating_sub(earliest)).unwrap_or(0));
    }
    late
}

/// The smallest value with `numerator` of every `denominator` values at or under it; none of nothing.
fn percentile(sorted: &[u64], numerator: usize, denominator: usize) -> Option<u64> {
    let rank = sorted.len().saturating_mul(numerator).div_ceil(denominator);
    sorted.get(rank.checked_sub(1)?).copied()
}

/// The rows of one start: the count, the percentiles and the largest, the histogram, and the SDUs later than each cover.
pub(crate) fn rows(label: &str, audio: &[AudioPacket]) -> Vec<Row> {
    let mut late = lateness(audio);
    late.sort_unstable();
    let ms =
        |ns: Option<u64>| ns.map_or_else(|| NONE.to_owned(), |ns| format!("{:.3}", millis(ns)));
    let rule = format!(
        "SDUs with a whole unit; arrival less the time of the samples the decode chain writes up to its end at {RATE_HZ} Hz, less the smallest of the {ARRIVAL_WINDOW} SDUs up to it since the stamps last started a new run"
    );
    let mut rows = vec![Row::new(
        "lateness_sdus",
        &late.len(),
        PACKETS,
        &labelled(label, &[&rule]),
    )];
    for (key, numerator, denominator) in PERCENTILES {
        let value = percentile(&late, numerator, denominator);
        let note = format!("{numerator} of {denominator} at or under it");
        rows.push(Row::new(key, &ms(value), MS, &labelled(label, &[&note])));
    }
    let largest = late.last().copied();
    rows.push(Row::new(
        "lateness_max_ms",
        &ms(largest),
        MS,
        &labelled(label, &[]),
    ));
    let step = usize::try_from(STEP_MS).unwrap_or(1);
    for from_ms in (0..=LAST_COVER_MS).step_by(step) {
        let last = from_ms == LAST_COVER_MS;
        let until_ms = from_ms.saturating_add(STEP_MS);
        let bounds = [from_ms, until_ms].map(|ms| ms.saturating_mul(NS_PER_MS));
        let span = if last {
            format!("{from_ms} ms and more")
        } else {
            format!("{from_ms} ms up to {until_ms} ms")
        };
        let [from, until] = bounds;
        let held = late
            .iter()
            .filter(|ns| **ns >= from && (last || **ns < until));
        rows.push(Row::new(
            &format!("lateness_bin_{from_ms}_ms"),
            &held.count(),
            PACKETS,
            &labelled(label, &[&span]),
        ));
    }
    for cover_ms in (FIRST_COVER_MS..=LAST_COVER_MS).step_by(step) {
        let cover_ns = cover_ms.saturating_mul(NS_PER_MS);
        rows.push(Row::new(
            &format!("lateness_over_{cover_ms}_ms"),
            &late.iter().filter(|ns| **ns > cover_ns).count(),
            PACKETS,
            &labelled(label, &[&format!("later than a cover of {cover_ms} ms")]),
        ));
    }
    rows
}

#[cfg(test)]
mod tests {
    use pods_mic::STAMP_STEP;
    use pods_proto::uplink::OPAQUE_LEN;

    use super::super::load::Unit;
    use super::*;

    const PERIOD_NS: u64 = 30_000_000;
    const UNITS_PER_SDU: u32 = 4;
    /// How much sooner than its cadence the first SDU of the window test lands.
    const EARLY_NS: u64 = 20_000_000;
    const KEYS: [&str; 25] = [
        "lateness_sdus",
        "lateness_p99_ms",
        "lateness_p99_9_ms",
        "lateness_p99_99_ms",
        "lateness_max_ms",
        "lateness_bin_0_ms",
        "lateness_bin_5_ms",
        "lateness_bin_10_ms",
        "lateness_bin_15_ms",
        "lateness_bin_20_ms",
        "lateness_bin_25_ms",
        "lateness_bin_30_ms",
        "lateness_bin_35_ms",
        "lateness_bin_40_ms",
        "lateness_bin_45_ms",
        "lateness_bin_50_ms",
        "lateness_bin_55_ms",
        "lateness_bin_60_ms",
        "lateness_over_30_ms",
        "lateness_over_35_ms",
        "lateness_over_40_ms",
        "lateness_over_45_ms",
        "lateness_over_50_ms",
        "lateness_over_55_ms",
        "lateness_over_60_ms",
    ];

    /// SDU `index` of a steady stream, landing `late_ns` after its cadence, with `units` whole units.
    fn sdu(index: u32, late_ns: u64, units: u32) -> AudioPacket {
        let first = index.saturating_mul(UNITS_PER_SDU);
        let unit = |at: u32| Unit {
            stamp: first.saturating_add(at).saturating_mul(STAMP_STEP),
            len: 2,
            max_sfb: None,
        };
        let ended_ns = u64::from(index.saturating_add(1)).saturating_mul(PERIOD_NS);
        AudioPacket {
            record: usize::try_from(index).unwrap(),
            at_ns: ended_ns.saturating_add(late_ns),
            opaque: [0; OPAQUE_LEN],
            units: (0..units).map(unit).collect(),
            len: 0,
            trailing: 0,
        }
    }

    fn value<'a>(rows: &'a [Row], key: &str) -> &'a str {
        let row = rows.iter().find(|row| row.key == key).unwrap();
        &row.value
    }

    #[test]
    fn a_start_with_no_audio_has_every_row_and_no_figure() {
        let rows = rows("start 1", &[]);
        let keys: Vec<&str> = rows.iter().map(|row| row.key.as_str()).collect();
        assert_eq!(keys, KEYS);
        assert_eq!(value(&rows, "lateness_sdus"), "0");
        for key in &KEYS[1..5] {
            assert_eq!(value(&rows, key), NONE, "{key}");
        }
        for key in &KEYS[5..] {
            assert_eq!(value(&rows, key), "0", "{key}");
        }
    }

    #[test]
    fn an_early_arrival_is_measured_against_only_while_the_window_holds_it() {
        let stream: Vec<AudioPacket> = (0..40)
            .map(|index| sdu(index, if index == 0 { 0 } else { EARLY_NS }, UNITS_PER_SDU))
            .collect();
        let late = lateness(&stream);
        let window = ARRIVAL_WINDOW;
        assert_eq!(late[0], 0);
        assert_eq!(late[1..window], vec![EARLY_NS; window - 1]);
        assert_eq!(late[window..], vec![0; 40 - window]);
    }

    #[test]
    fn an_sdu_with_no_whole_unit_is_not_judged_and_a_short_one_counts_by_its_last_stamp() {
        let stream = [
            sdu(0, 0, UNITS_PER_SDU),
            sdu(1, 0, 0),
            sdu(2, 0, UNITS_PER_SDU - 1),
        ];
        // The third holds one unit less, so its last stamp stands one unit sooner: it reads as one unit late.
        let unit_ns = u64::from(STAMP_STEP) * u64::from(NS_PER_SAMPLE);
        assert_eq!(lateness(&stream), [0, unit_ns]);
    }

    #[test]
    fn the_exact_bounds_fall_in_the_bin_they_open_and_under_the_cover_they_equal() {
        let at_ms = |ms: u64| sdu(1, ms * NS_PER_MS, UNITS_PER_SDU);
        for (late_ms, bin, over) in [
            (30, "30", "0"),
            (59, "55", "1"),
            (60, "60", "1"),
            (61, "60", "1"),
        ] {
            let rows = rows("start 1", &[sdu(0, 0, UNITS_PER_SDU), at_ms(late_ms)]);
            assert_eq!(
                value(&rows, &format!("lateness_bin_{bin}_ms")),
                "1",
                "{late_ms}"
            );
            assert_eq!(value(&rows, "lateness_over_30_ms"), over, "{late_ms}");
            let over_60 = if late_ms > 60 { "1" } else { "0" };
            assert_eq!(value(&rows, "lateness_over_60_ms"), over_60, "{late_ms}");
            assert_eq!(value(&rows, "lateness_max_ms"), format!("{late_ms}.000"));
            let bins = rows.iter().filter(|row| row.key.contains("_bin_"));
            let held: Vec<&str> = bins.map(|row| row.value.as_str()).collect();
            let ones = held.iter().filter(|count| **count == "1").count();
            let zeros = held.iter().filter(|count| **count == "0").count();
            assert_eq!((ones, zeros), (2, 11), "{late_ms}: {held:?}");
        }
    }

    #[test]
    fn a_percentile_is_the_smallest_value_with_its_share_at_or_under_it() {
        let sorted: Vec<u64> = (1..=1000).collect();
        assert_eq!(percentile(&sorted, 99, 100), Some(990));
        assert_eq!(percentile(&sorted, 999, 1000), Some(999));
        assert_eq!(percentile(&sorted, 9999, 10_000), Some(1000));
        assert_eq!(percentile(&sorted[..1], 99, 100), Some(1));
        assert_eq!(percentile(&[], 99, 100), None);
    }
}
