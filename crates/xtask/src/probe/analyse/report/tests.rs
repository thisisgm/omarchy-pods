//! The rows a run is reported in: what each holds, when it is refused and what never reaches the table.

use pods_proto::sdulog::Record;
use pods_proto::uplink::{START, STOP};
use proptest::prelude::*;

use super::super::input::{Mark, MarkKind};
use super::super::load::load;
use super::super::shape::{StampBreak, gaps};
use super::super::support::{
    NS_PER_MS, NS_PER_SECOND, OPAQUE, SDU_INTERVAL_NS, SEED, SILENT_UNIT, STAMP_STEP, audio, close,
    from_pods, mark, ppm_apart, shifted, shows, to_pods,
};
use super::super::synth::synth;
use super::*;

/// How close a reported rate must come to the rate the stream was generated at.
pub(crate) const WITHIN_PPM: f64 = 20.0;
/// Ten minutes, the length of a soak.
const TEN_MINUTES_S: u32 = 600;
/// How late `late_end` makes the last SDU: 80 ms after the one before, under the 90 ms gap limit.
const LATE_END_NS: u64 = 50 * NS_PER_MS;
/// SDUs in a damaged run: over the 500 the rates need, and over 30 s at one each 30 ms.
const DAMAGED_SDUS: usize = 1010;
/// The delay a late SDU of a damaged run stays under, in milliseconds: some pass the 90 ms gap limit.
const LATE_UNDER_MS: u64 = 150;
/// The most units an SDU of a damaged run holds, and the most bytes one of those units holds.
const DAMAGED_MOST: usize = 4;
/// The keys of the rows that are refused on thin data.
pub(crate) const RATE_KEYS: [&str; 15] = [
    "sdus_per_second",
    "units_per_sdu",
    "sdu_interval_ms_min",
    "sdu_interval_ms_median",
    "sdu_interval_ms_max",
    "clock_units_per_second",
    "clock_hz",
    "clock_hz_low",
    "clock_hz_high",
    "clock_ppm_vs_64000",
    "clock_ppm_vs_48000",
    "audio_bits_per_second",
    "sdu_bits_per_second",
    "unit_bytes_min",
    "unit_bytes_max",
];
/// The reply the pods are reported to send after START.
const CONTROL_REPLY: [u8; 13] = [
    0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x00, 0x00, 0x03, 0x00, 0x01, 0x01, 0x01,
];
/// An ear detection packet.
pub(crate) const EAR: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x00];
/// A battery packet: the left bud at 85 percent, the right at 90 and charging.
const BATTERY: [u8; 17] = [
    0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x02, 0x04, 0x01, 0x55, 0x02, 0x01, 0x02, 0x01, 0x5a, 0x01,
    0x01,
];
/// A device information packet with two made up strings.
const DEVICE_INFO: &[u8] =
    b"\x04\x00\x04\x00\x1d\x00\x02\xed\x00\x04\x00Probe Pods\x00SERIAL0XYZ\x00";

#[test]
fn a_cut_last_line_is_counted_in_a_row_of_its_own() {
    let records = [from_pods(1, &EAR)];
    let whole = analyse(&records, &[], "listen", CutLines::default());
    assert_eq!(
        shown(one(&whole, "sdu_log_cut_lines")),
        (
            "sdu_log_cut_lines",
            "0",
            "lines",
            "last line of sdu.log without its newline, left out: a capture killed while writing"
        )
    );
    assert_eq!(
        shown(one(&whole, "marks_cut_lines")),
        (
            "marks_cut_lines",
            "0",
            "lines",
            "last line of marks.tsv without its newline, left out: a capture killed while writing"
        )
    );
    let cut = CutLines {
        log: true,
        marks: false,
    };
    let log_cut = analyse(&records, &[], "listen", cut);
    assert_eq!(one(&log_cut, "sdu_log_cut_lines").value, "1");
    assert_eq!(one(&log_cut, "marks_cut_lines").value, "0");
    let cut = CutLines {
        log: false,
        marks: true,
    };
    let marks_cut = analyse(&records, &[], "listen", cut);
    assert_eq!(one(&marks_cut, "sdu_log_cut_lines").value, "0");
    assert_eq!(one(&marks_cut, "marks_cut_lines").value, "1");
}

pub(crate) fn steady(rate_hz: u32, seconds: u32) -> Vec<Record> {
    synth(rate_hz, 480, 4, seconds, 0, SEED)
}

/// A 61 s run whose last SDU arrives late, which stretches the first to last ratio and hardly moves the slope.
pub(crate) fn late_end() -> Vec<Record> {
    let mut records = steady(64_000, 61);
    let last = records.last_mut().unwrap();
    last.at_ns = last.at_ns.saturating_add(LATE_END_NS);
    records
}

pub(crate) fn start_at_zero() -> [Mark; 1] {
    [mark(0, MarkKind::Start)]
}

pub(crate) fn rows<'a>(report: &'a Report, key: &str) -> Vec<&'a Row> {
    report.rows.iter().filter(|row| row.key == key).collect()
}

pub(crate) fn one<'a>(report: &'a Report, key: &str) -> &'a Row {
    let found = rows(report, key);
    assert_eq!(found.len(), 1, "{key}: {found:?}");
    found[0]
}

pub(crate) fn number(row: &Row) -> f64 {
    row.value.parse().unwrap()
}

pub(crate) fn has_note(row: &Row, item: &str) -> bool {
    row.note.split("; ").any(|part| part == item)
}

/// The key, value, unit and note of one row, as the table prints them.
pub(crate) fn shown(row: &Row) -> (&str, &str, &str, &str) {
    (&row.key, &row.value, row.unit, &row.note)
}

#[test]
fn analyse_reports_the_rate_a_generated_log_was_written_at() {
    for rate_hz in [64_000, 48_000, 63_990] {
        let truth = f64::from(rate_hz);
        let records = steady(rate_hz, TEN_MINUTES_S);
        let report = analyse(&records, &start_at_zero(), "soak", CutLines::default());

        let hz = one(&report, "clock_hz");
        assert!(
            ppm_apart(number(hz), truth) < WITHIN_PPM,
            "{rate_hz}: {hz:?}"
        );
        assert_eq!(hz.unit, "Hz");
        assert!(has_note(hz, "start 1") && has_note(hz, "slope x 480 samples per unit"));
        assert!(!has_note(hz, "gap"), "{hz:?}");
        let units = one(&report, "clock_units_per_second");
        assert!(
            ppm_apart(number(units), truth / 480.0) < WITHIN_PPM,
            "{units:?}"
        );
        assert!(!has_note(units, "disagree"), "{units:?}");
        let low = number(one(&report, "clock_hz_low"));
        let high = number(one(&report, "clock_hz_high"));
        assert!(low <= number(hz) && number(hz) <= high);
        let against_64 = number(one(&report, "clock_ppm_vs_64000"));
        assert!(close(
            against_64,
            (truth / 64_000.0 - 1.0) * 1e6,
            WITHIN_PPM
        ));
        let against_48 = number(one(&report, "clock_ppm_vs_48000"));
        assert!(close(
            against_48,
            (truth / 48_000.0 - 1.0) * 1e6,
            WITHIN_PPM
        ));

        let sdus = one(&report, "sdus_per_second");
        assert!(
            ppm_apart(number(sdus), truth / 1920.0) < WITHIN_PPM,
            "{sdus:?}"
        );
        assert_eq!(one(&report, "stamp_unit").value, "samples");
        assert_eq!(one(&report, "gaps").value, "0");
        assert_eq!(one(&report, "script").value, "soak");
        assert_eq!(one(&report, "audio_sdus").value, records.len().to_string());
        assert_eq!(one(&report, "audio_sdus_outside_a_start").value, "0");
        assert!(report.rows.iter().all(|row| row.value != "refused"));
    }
}

#[test]
fn gap_is_reported_not_averaged_away() {
    const LOST: usize = 10_000;
    let mut records = steady(64_000, TEN_MINUTES_S);
    let clean = analyse(&records, &start_at_zero(), "soak", CutLines::default());
    records.remove(LOST);

    let found = gaps(&load(&records));
    assert_eq!(found.len(), 1, "{found:?}");
    let missing = StampBreak::Missing {
        first: 19_200_000,
        last: 19_201_440,
    };
    assert_eq!((found[0].sdu, found[0].stamps), (LOST, Some(missing)));

    let report = analyse(&records, &start_at_zero(), "soak", CutLines::default());
    assert_eq!(one(&report, "gaps").value, "1");
    let gap = one(&report, "gap");
    assert_eq!(gap.value, "300060.000");
    assert!(
        has_note(gap, "stamps 19200000 to 19201440 missing"),
        "{gap:?}"
    );
    for key in [
        "clock_units_per_second",
        "clock_hz",
        "clock_hz_low",
        "clock_hz_high",
    ] {
        assert!(has_note(one(&report, key), "gap"), "{key}");
        assert!(!has_note(one(&clean, key), "gap"), "{key}");
    }
    assert!(has_note(one(&report, "sdus_per_second"), "gap"));
    assert!(has_note(one(&report, "audio_bits_per_second"), "gap"));
    assert_eq!(one(&clean, "gaps").value, "0");
    assert_eq!(rows(&clean, "gap").len(), 0);
}

#[test]
fn a_clock_whose_slope_and_ratio_differ_says_disagree() {
    let mut records = steady(64_000, 61);
    records.remove(1000);
    let report = analyse(&records, &start_at_zero(), "soak", CutLines::default());
    let clock_keys = [
        "clock_units_per_second",
        "clock_hz",
        "clock_hz_low",
        "clock_hz_high",
        "clock_ppm_vs_64000",
        "clock_ppm_vs_48000",
    ];
    for key in clock_keys {
        let row = one(&report, key);
        assert!(has_note(row, "disagree") && has_note(row, "gap"), "{row:?}");
    }
    assert!(!has_note(one(&report, "sdus_per_second"), "disagree"));

    // A late last SDU opens no gap, so `disagree` here can come only from the slope against the ratio.
    let report = analyse(&late_end(), &start_at_zero(), "soak", CutLines::default());
    assert_eq!(one(&report, "gaps").value, "0");
    for key in clock_keys {
        let row = one(&report, key);
        assert!(
            has_note(row, "disagree") && !has_note(row, "gap"),
            "{row:?}"
        );
    }
    assert!(!has_note(one(&report, "sdus_per_second"), "disagree"));
}

#[test]
fn rates_never_span_a_stop() {
    const STOP_NS: u64 = 62 * NS_PER_SECOND;
    const SECOND_START_NS: u64 = 140 * NS_PER_SECOND;
    let first = steady(64_000, 61);
    let stray = shifted(steady(48_000, 61), STOP_NS + 500 * NS_PER_MS);
    let second = shifted(steady(48_000, 61), SECOND_START_NS);
    let (first_len, stray_len, second_len) = (first.len(), stray.len(), second.len());
    let records: Vec<Record> = first.into_iter().chain(stray).chain(second).collect();
    let marks = [
        mark(0, MarkKind::Start),
        mark(STOP_NS, MarkKind::Stop),
        mark(SECOND_START_NS, MarkKind::Start),
    ];
    let report = analyse(&records, &marks, "trials", CutLines::default());

    let clocks = rows(&report, "clock_hz");
    assert_eq!(clocks.len(), 2, "{clocks:?}");
    assert!(
        ppm_apart(number(clocks[0]), 64_000.0) < WITHIN_PPM,
        "{clocks:?}"
    );
    assert!(has_note(clocks[0], "start 1") && !has_note(clocks[0], "gap"));
    assert!(
        ppm_apart(number(clocks[1]), 48_000.0) < WITHIN_PPM,
        "{clocks:?}"
    );
    assert!(has_note(clocks[1], "start 2") && !has_note(clocks[1], "gap"));
    let counts: Vec<_> = rows(&report, "segment_audio_sdus")
        .iter()
        .map(|row| row.value.clone())
        .collect();
    assert_eq!(counts, [first_len.to_string(), second_len.to_string()]);
    let outside = one(&report, "audio_sdus_outside_a_start");
    assert_eq!(outside.value, stray_len.to_string());
    let after_stop: Vec<_> = rows(&report, "after_stop_audio_sdus")
        .iter()
        .map(|row| row.value.clone())
        .collect();
    assert_eq!(after_stop, [stray_len.to_string(), "none".to_owned()]);
    let gap_counts: Vec<_> = rows(&report, "gaps").iter().map(|row| &row.value).collect();
    assert_eq!(gap_counts, ["0", "0"]);
}

#[test]
fn short_run_rows_read_refused_with_the_counts() {
    let report = analyse(
        &steady(64_000, 29),
        &start_at_zero(),
        "soak",
        CutLines::default(),
    );
    for key in RATE_KEYS {
        let row = one(&report, key);
        assert_eq!(row.value, "refused", "{key}");
        assert!(!row.unit.is_empty(), "{key}");
        assert!(has_note(row, "start 1"), "{row:?}");
        assert!(has_note(row, "966 audio SDUs over 28.950 s"), "{row:?}");
        assert!(has_note(row, "needs 500 over 30 s"), "{row:?}");
    }
    assert_eq!(one(&report, "max_sfb_hz").value, "refused");
    assert_eq!(one(&report, "trial_audio_ms").value, "30.000");
    assert_eq!(one(&report, "segment_audio_sdus").value, "966");
    assert_eq!(one(&report, "segment_span_s").value, "28.950");

    let enough = analyse(
        &steady(64_000, 31),
        &start_at_zero(),
        "soak",
        CutLines::default(),
    );
    for key in RATE_KEYS {
        let row = one(&enough, key);
        assert!(number(row).is_finite(), "{row:?}");
    }
}

#[test]
fn trailing_bytes_are_totalled() {
    let whole = audio(10, &OPAQUE, &[(0, &SILENT_UNIT), (480, &SILENT_UNIT)]);
    let mut three_stray = audio(20, &OPAQUE, &[(960, &SILENT_UNIT)]);
    three_stray.sdu.extend_from_slice(&[0xee, 0xee, 0xee]);
    let mut cut_unit = audio(30, &OPAQUE, &[(1440, &SILENT_UNIT)]);
    cut_unit
        .sdu
        .extend_from_slice(&[0x80, 0x07, 0x00, 0x00, 0x09, 0x11]);
    let records = [whole.clone(), three_stray, cut_unit];
    let trailing: Vec<_> = load(&records)
        .audio
        .iter()
        .map(|sdu| sdu.trailing)
        .collect();
    assert_eq!(trailing, [0, 3, 6]);

    let report = analyse(&records, &start_at_zero(), "soak", CutLines::default());
    let expected = (
        "trailing_bytes",
        "9",
        "bytes",
        "after the last whole unit; in 2 SDUs",
    );
    assert_eq!(shown(one(&report, "trailing_bytes")), expected);
    assert_eq!(one(&report, "audio_units").value, "4");

    let report = analyse(&[whole], &start_at_zero(), "soak", CutLines::default());
    let expected = (
        "trailing_bytes",
        "0",
        "bytes",
        "after the last whole unit; in 0 SDUs",
    );
    assert_eq!(shown(one(&report, "trailing_bytes")), expected);
}

/// A short run with one packet of every kind the table counts.
pub(crate) fn mixed_run() -> (Vec<Record>, Vec<Mark>) {
    let mut cut_reply = CONTROL_REPLY.to_vec();
    cut_reply.pop();
    let records = vec![
        from_pods(100 * NS_PER_MS, &[0x01, 0x00, 0x04, 0x00]),
        from_pods(200 * NS_PER_MS, &BATTERY),
        from_pods(300 * NS_PER_MS, DEVICE_INFO),
        from_pods(400 * NS_PER_MS, &EAR),
        audio(900 * NS_PER_MS, &OPAQUE, &[(0, &SILENT_UNIT)]),
        to_pods(1000 * NS_PER_MS, &START),
        from_pods(1012 * NS_PER_MS, &CONTROL_REPLY),
        audio(
            1085 * NS_PER_MS,
            &OPAQUE,
            &[(0, &SILENT_UNIT), (480, &[0x00, 0x7c, 0x55])],
        ),
        audio(
            1115 * NS_PER_MS,
            &OPAQUE,
            &[(960, &SILENT_UNIT), (1440, &[0x00])],
        ),
        from_pods(1500 * NS_PER_MS, &EAR),
        from_pods(
            1700 * NS_PER_MS,
            &[0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x02, 0x00],
        ),
        from_pods(
            1800 * NS_PER_MS,
            &[0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x01],
        ),
        to_pods(2000 * NS_PER_MS, &STOP),
        from_pods(2003 * NS_PER_MS, &cut_reply),
        from_pods(2500 * NS_PER_MS, &BATTERY),
    ];
    let marks = vec![
        mark(0, MarkKind::Other),
        mark(1000 * NS_PER_MS, MarkKind::Start),
        mark(1600 * NS_PER_MS, MarkKind::Truncated),
        mark(2000 * NS_PER_MS, MarkKind::Stop),
        mark(3000 * NS_PER_MS, MarkKind::Other),
    ];
    (records, marks)
}

/// The rows of the whole run that `mixed_run` gives, in order.
const RUN_ROWS: &[(&str, &str, &str, &str)] = &[
    ("script", "voice", "name", "from meta.txt"),
    (
        "sdu_log_cut_lines",
        "0",
        "lines",
        "last line of sdu.log without its newline, left out: a capture killed while writing",
    ),
    (
        "marks_cut_lines",
        "0",
        "lines",
        "last line of marks.tsv without its newline, left out: a capture killed while writing",
    ),
    ("packets_from_pods", "13", "packets", ""),
    (
        "packets_to_pods",
        "2",
        "packets",
        "written by the host; in no figure",
    ),
    (
        "audio_sdus",
        "3",
        "packets",
        "opcode 0x58 subtype 1 from the pods",
    ),
    ("audio_units", "5", "units", "whole units in the audio SDUs"),
    (
        "trailing_bytes",
        "0",
        "bytes",
        "after the last whole unit; in 0 SDUs",
    ),
    (
        "audio_sdus_outside_a_start",
        "1",
        "packets",
        "not between a start mark and the mark that ends it; in no rate; after_stop_audio_sdus has those after the stop mark that ends a start",
    ),
    (
        "uplink_unknown_subtype",
        "1",
        "packets",
        "opcode 0x58 with a subtype that is neither audio nor control",
    ),
    (
        "uplink_cut_short",
        "1",
        "packets",
        "opcode 0x58 too short to read",
    ),
    (
        "cut_receives",
        "1",
        "packets",
        "receives cut by the buffer; their bytes are not in sdu.log",
    ),
    ("largest_packet", "37", "bytes", "opcode 0x0058"),
    (
        "packets_without_opcode",
        "1",
        "packets",
        "shorter than 6 bytes",
    ),
    (
        "opcode_0x0004",
        "2",
        "packets",
        "little endian opcode at bytes 4 and 5",
    ),
    (
        "opcode_0x0006",
        "2",
        "packets",
        "little endian opcode at bytes 4 and 5",
    ),
    (
        "opcode_0x001d",
        "1",
        "packets",
        "little endian opcode at bytes 4 and 5",
    ),
    (
        "control_packets",
        "2",
        "packets",
        "opcode 0x58 subtype 0 from the pods",
    ),
    (
        "control_packet",
        "04000400580000000300010101",
        "hex",
        "+12.000 ms from start 1; length_matches true",
    ),
    (
        "control_packet",
        "040004005800000003000101",
        "hex",
        "+3.000 ms from stop 1; length_matches false",
    ),
    ("battery_reports", "2", "packets", ""),
    (
        "battery_unread",
        "0",
        "packets",
        "refused by the battery parser",
    ),
    ("battery_left_first", "85", "percent", "not charging"),
    ("battery_left_last", "85", "percent", "not charging"),
    ("battery_right_first", "90", "percent", "charging"),
    ("battery_right_last", "90", "percent", "charging"),
    (
        "device_info_packets",
        "1",
        "packets",
        "strings of the first are in device-info.txt",
    ),
];

#[test]
fn run_rows_count_every_packet_once() {
    let (records, marks) = mixed_run();
    let report = analyse(&records, &marks, "voice", CutLines::default());
    let run_rows: Vec<_> = report.rows.iter().take(RUN_ROWS.len()).map(shown).collect();
    assert_eq!(run_rows, RUN_ROWS);
}

#[test]
fn device_strings_stay_out_of_the_table() {
    let (records, marks) = mixed_run();
    let report = analyse(&records, &marks, "voice", CutLines::default());
    let strings = report.device_strings.as_ref().unwrap();
    assert_eq!(strings, &["Probe Pods", "SERIAL0XYZ"]);
    let table = table(&report.rows);
    for private in ["Probe", "SERIAL0XYZ"] {
        assert!(!shows(&table, private), "{table}");
        assert!(!shows(&report.units, private), "{}", report.units);
    }

    let without = analyse(&[from_pods(1, &EAR)], &[], "listen", CutLines::default());
    assert_eq!(without.device_strings, None);
    let row = one(&without, "device_info_packets");
    assert_eq!(shown(row), ("device_info_packets", "0", "packets", ""));
}

#[test]
fn units_table_lists_every_unit_under_its_log_line() {
    let records = [
        from_pods(10, &EAR),
        audio(20, &OPAQUE, &[(0, &SILENT_UNIT)]),
        audio(
            40,
            &OPAQUE,
            &[
                (480, &SILENT_UNIT),
                (960, &[0x00, 0x7c, 0x55]),
                (1440, &[0x09]),
            ],
        ),
    ];
    let report = analyse(&records, &[], "listen", CutLines::default());
    let expected = "index\tsdu\tarrival_ns\tstamp\tlen\tmax_sfb\n\
                    0\t2\t20\t0\t2\t0\n\
                    1\t3\t40\t480\t2\t0\n\
                    2\t3\t40\t960\t3\t31\n\
                    3\t3\t40\t1440\t1\t\n";
    assert_eq!(report.units, expected);
}

#[test]
fn the_table_is_four_tab_separated_fields_a_line() {
    let (records, marks) = mixed_run();
    let report = analyse(&records, &marks, "voice", CutLines::default());
    let table = table(&report.rows);
    let mut lines = table.lines();
    assert_eq!(lines.next(), Some("key\tvalue\tunit\tnote"));
    let body: Vec<_> = lines.collect();
    assert_eq!(body.len(), report.rows.len());
    assert!(table.ends_with('\n'));
    for (line, row) in body.iter().zip(&report.rows) {
        let fields: Vec<_> = line.split('\t').collect();
        assert_eq!(
            fields,
            [
                row.key.as_str(),
                row.value.as_str(),
                row.unit,
                row.note.as_str()
            ]
        );
        assert!(
            !row.key.is_empty() && !row.value.is_empty() && !row.unit.is_empty(),
            "{row:?}"
        );
    }
}

/// Packets of every kind the analysis tells apart, then cut or padded at random.
fn sdu() -> impl Strategy<Value = Vec<u8>> {
    let known = prop_oneof![
        Just(CONTROL_REPLY.to_vec()),
        Just(BATTERY.to_vec()),
        Just(EAR.to_vec()),
        Just(DEVICE_INFO.to_vec()),
        Just(audio(0, &OPAQUE, &[(0, &SILENT_UNIT), (480, &SILENT_UNIT)]).sdu),
    ];
    (
        known,
        0..64_usize,
        proptest::collection::vec(any::<u8>(), 0..8),
    )
        .prop_map(|(mut sdu, keep, tail)| {
            sdu.truncate(keep);
            sdu.extend(tail);
            sdu
        })
}

fn any_records() -> impl Strategy<Value = Vec<Record>> {
    let record = (0..2000_u64, any::<bool>(), sdu()).prop_map(|(at_ns, sent, sdu)| {
        if sent {
            to_pods(at_ns, &sdu)
        } else {
            from_pods(at_ns, &sdu)
        }
    });
    proptest::collection::vec(record, 0..24)
}

/// A run long enough for every rate, with late and bunched SDUs, uneven unit counts and broken stamps.
fn damaged_run() -> impl Strategy<Value = Vec<Record>> {
    let delay = prop_oneof![Just(0_u64), 0..LATE_UNDER_MS];
    let step = prop_oneof![Just(STAMP_STEP), Just(0_u32), any::<u32>()];
    let unit = (
        step,
        proptest::collection::vec(any::<u8>(), 1..=DAMAGED_MOST),
    );
    let sdu = (delay, proptest::collection::vec(unit, 1..=DAMAGED_MOST));
    proptest::collection::vec(sdu, DAMAGED_SDUS).prop_map(|sdus| {
        let (mut on_time_ns, mut at_ns, mut stamp) = (0_u64, 0_u64, 0_u32);
        let records = sdus.iter().map(|(late_ms, units)| {
            on_time_ns = on_time_ns.saturating_add(SDU_INTERVAL_NS);
            let held_ns = late_ms.saturating_mul(NS_PER_MS);
            at_ns = at_ns.max(on_time_ns.saturating_add(held_ns));
            let units = units.iter().map(|(step, data)| {
                stamp = stamp.wrapping_add(*step);
                (stamp, data.as_slice())
            });
            audio(at_ns, &OPAQUE, &units.collect::<Vec<_>>())
        });
        records.collect()
    })
}

/// Whether `text` holds a figure that is not a number, as `f64` prints one.
fn not_a_number(text: &str) -> bool {
    let mut words = text.split(|letter: char| !letter.is_alphanumeric());
    words.any(|word| word == "NaN" || word == "inf")
}

fn any_marks() -> impl Strategy<Value = Vec<Mark>> {
    let kind = prop_oneof![
        Just(MarkKind::Start),
        Just(MarkKind::Stop),
        Just(MarkKind::Truncated),
        Just(MarkKind::Other),
    ];
    proptest::collection::vec((0..2000_u64, kind), 0..6).prop_map(|mut marks| {
        marks.sort_by_key(|&(at_ns, _)| at_ns);
        marks
            .into_iter()
            .map(|(at_ns, kind)| mark(at_ns, kind))
            .collect()
    })
}

proptest! {
    #[test]
    fn analyse_never_panics_and_every_row_is_one_line(
        mut records in any_records(),
        marks in any_marks(),
        sorted in any::<bool>(),
    ) {
        if sorted {
            records.sort_by_key(|record| record.at_ns);
        }
        let report = analyse(&records, &marks, "voice", CutLines::default());
        for row in &report.rows {
            let fields = [row.key.as_str(), row.value.as_str(), row.unit, row.note.as_str()];
            prop_assert!(fields.iter().all(|field| !field.contains(['\t', '\n'])), "{:?}", row);
            prop_assert!(!row.value.contains("NaN") && !row.value.contains("inf"), "{:?}", row);
        }
        prop_assert_eq!(table(&report.rows).lines().count(), report.rows.len().saturating_add(1));
    }

    #[test]
    fn a_damaged_run_long_enough_for_the_rates_prints_only_numbers(records in damaged_run()) {
        let report = analyse(&records, &start_at_zero(), "soak", CutLines::default());
        for row in &report.rows {
            prop_assert!(!not_a_number(&row.value) && !not_a_number(&row.note), "{:?}", row);
        }
        for key in RATE_KEYS.into_iter().chain(["lead_ms", "burst_ms"]) {
            let row = one(&report, key);
            prop_assert!(row.value.parse::<f64>().is_ok_and(f64::is_finite), "{:?}", row);
        }
    }

    #[test]
    fn every_audio_sdu_is_in_a_start_after_a_stop_or_before_the_first_start(
        mut records in any_records(),
        marks in any_marks(),
    ) {
        records.sort_by_key(|record| record.at_ns);
        let report = analyse(&records, &marks, "voice", CutLines::default());
        let total = |key: &str| -> usize {
            let counts = rows(&report, key).into_iter().filter_map(|row| row.value.parse::<usize>().ok());
            counts.sum()
        };
        let starts = marks.iter().filter(|mark| mark.kind == MarkKind::Start);
        let first_start_ns = starts.map(|mark| mark.at_ns).next();
        let audio = load(&records).audio;
        let early = audio.iter().filter(|sdu| first_start_ns.is_none_or(|start_ns| sdu.at_ns < start_ns));
        let outside = total("audio_sdus_outside_a_start");
        prop_assert_eq!(outside, [early.count(), total("after_stop_audio_sdus")].iter().sum::<usize>());
        prop_assert_eq!(audio.len(), [total("segment_audio_sdus"), outside].iter().sum::<usize>());
    }
}
