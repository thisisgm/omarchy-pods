//! The rows of one start mark: what each holds on a short, an empty and a long enough segment.

use pods_proto::sdulog::Record;
use pods_proto::uplink::START;

use super::super::super::input::{CutLines, MarkKind};
use super::super::super::support::{
    NS_PER_MS, NS_PER_SECOND, OPAQUE, SDU_INTERVAL_NS, SEED, SILENT_UNIT, audio, close, from_pods,
    mark, ppm_apart, shifted, stall, stream, to_pods,
};
use super::super::super::synth::synth;
use super::super::analyse;
use super::super::tests::{
    EAR, RATE_KEYS, WITHIN_PPM, has_note, late_end, mixed_run, number, one, rows, shown,
    start_at_zero, steady,
};

mod coarse;

/// What every rate row of the short segment of `mixed_run` says it held.
const THIN: &str = "start 1; 2 audio SDUs over 0.030 s; needs 500 over 30 s";

const fn refused(
    key: &'static str,
    unit: &'static str,
) -> (&'static str, &'static str, &'static str, &'static str) {
    (key, "refused", unit, THIN)
}

const TOO_FEW_FOR_A_LEAD: &str =
    "start 1; fewer than 50 packets with a whole unit in the first 10 s";
/// The rows `lead` gives, each cut by a frame length.
const LEAD_KEYS: [&str; 4] = ["lead_ms", "burst_packets", "burst_frames", "burst_ms"];
const OWN_FRAME: &str = "frame from clock_units_per_second";
const NOMINAL_FRAME: &str = "nominal 7.500 ms frame assumed, not measured";
const STAMPS_UNCHECKED: &str = "stamps not checked, no modal step from unit to unit";
/// A clock about 1560 ppm fast of 64000 Hz, so a figure taken from a fixed 64000 Hz misses every tolerance here.
const FAST_HZ: u32 = 64_100;
/// Spectral line 336, the top line of band 31, at `FAST_HZ`: 336 x 64100 / 960.
const TOP_LINE_HZ: f64 = 22_435.0;

const START_ROWS: &[(&str, &str, &str, &str)] = &[
    (
        "trial_control_ms",
        "12.000",
        "ms",
        "start 1; start mark to the first control packet from the pods",
    ),
    (
        "trial_audio_ms",
        "85.000",
        "ms",
        "start 1; start mark to the first audio SDU",
    ),
    ("segment_audio_sdus", "2", "packets", "start 1"),
    (
        "segment_span_s",
        "0.030",
        "s",
        "start 1; first to last audio SDU",
    ),
    refused("sdus_per_second", "SDUs/s"),
    refused("units_per_sdu", "units"),
    refused("sdu_interval_ms_min", "ms"),
    refused("sdu_interval_ms_median", "ms"),
    refused("sdu_interval_ms_max", "ms"),
    refused("clock_units_per_second", "units/s"),
    refused("clock_hz", "Hz"),
    refused("clock_hz_low", "Hz"),
    refused("clock_hz_high", "Hz"),
    refused("clock_ppm_vs_64000", "ppm"),
    refused("clock_ppm_vs_48000", "ppm"),
    (
        "clock_hz_coarse",
        "refused",
        "Hz",
        "start 1; 0 audio SDUs over 0.000 s; needs 75 over 3 s; counted from 1 s after the first audio SDU",
    ),
    refused("audio_bits_per_second", "bit/s"),
    refused("sdu_bits_per_second", "bit/s"),
    refused("unit_bytes_min", "bytes"),
    refused("unit_bytes_max", "bytes"),
    (
        "stamp_delta_per_unit",
        "480",
        "stamp ticks",
        "start 1; modal; share 1.0000",
    ),
    (
        "stamp_delta_per_sdu",
        "960",
        "stamp ticks",
        "start 1; modal; share 1.0000",
    ),
    (
        "stamp_unit",
        "samples",
        "name",
        "start 1; modal delta per unit 480",
    ),
    (
        "opaque_fields",
        "constant(0xa0) constant(0xa1) constant(0xa2) constant(0xa3) constant(0xa4) constant(0xa5) constant(0xa6) constant(0xa7) constant(0xa8) constant(0xa9) constant(0xaa) constant(0xab) constant(0xac) constant(0xad)",
        "kind per byte",
        "start 1; header bytes 8 to 21 in order; 2 SDUs; what each byte did from SDU to SDU; a counter here is never a gap",
    ),
    (
        "header_length_at_8",
        "0",
        "packets",
        "start 1; of 2 SDUs; little endian u16 from header byte 8 against the SDU length less 10",
    ),
    (
        "header_length_at_14",
        "0",
        "packets",
        "start 1; of 2 SDUs; little endian u16 from header byte 14 against the SDU length less 16",
    ),
    (
        "gaps",
        "0",
        "gaps",
        "start 1; arrival over 90 ms or a stamp delta off the modal",
    ),
    (
        "max_sfb_histogram",
        "0:2 31:1",
        "units by band index",
        "start 1",
    ),
    (
        "max_sfb_max",
        "31",
        "band index",
        "start 1; spectral line 336",
    ),
    ("max_sfb_invalid", "0", "units", "start 1; over 35"),
    ("max_sfb_unread", "1", "units", "start 1; under two bytes"),
    ("max_sfb_hz", "refused", "Hz", THIN),
    ("lead_ms", "refused", "ms", TOO_FEW_FOR_A_LEAD),
    ("burst_packets", "refused", "packets", TOO_FEW_FOR_A_LEAD),
    ("burst_frames", "refused", "frames", TOO_FEW_FOR_A_LEAD),
    ("burst_ms", "refused", "ms", TOO_FEW_FOR_A_LEAD),
    (
        "after_stop_audio_sdus",
        "0",
        "packets",
        "start 1; from the stop mark to the next start mark; in no rate",
    ),
    (
        "after_stop_audio_ms",
        "none",
        "ms",
        "start 1; stop mark to the last audio SDU after it",
    ),
];

#[test]
fn start_rows_of_a_short_segment() {
    let (records, marks) = mixed_run();
    let report = analyse(&records, &marks, "voice", CutLines::default());
    let from_trial: Vec<_> = report
        .rows
        .iter()
        .skip_while(|row| row.key != "trial_control_ms")
        // The lateness rows close a start, and their own tests hold them.
        .take_while(|row| !row.key.starts_with("lateness_"))
        .map(shown)
        .collect();
    assert_eq!(from_trial, START_ROWS);
}

#[test]
fn a_start_with_no_audio_still_gets_its_rows() {
    let records = [from_pods(100, &EAR), from_pods(200, &EAR)];
    let marks = [mark(50, MarkKind::Start), mark(150, MarkKind::Stop)];
    let report = analyse(&records, &marks, "early", CutLines::default());
    assert_eq!(one(&report, "trial_control_ms").value, "none");
    assert_eq!(one(&report, "trial_audio_ms").value, "none");
    assert_eq!(one(&report, "segment_audio_sdus").value, "0");
    assert_eq!(one(&report, "segment_span_s").value, "0.000");
    for key in RATE_KEYS {
        let row = one(&report, key);
        assert_eq!(row.value, "refused", "{key}");
        assert!(has_note(row, "0 audio SDUs over 0.000 s"), "{row:?}");
    }
    let coarse = one(&report, "clock_hz_coarse");
    assert_eq!(coarse.value, "none");
    assert_eq!(coarse.note, "start 1; no audio SDU with a whole unit");
    for key in ["stamp_delta_per_unit", "stamp_delta_per_sdu", "stamp_unit"] {
        assert_eq!(one(&report, key).value, "none", "{key}");
    }
    assert!(has_note(one(&report, "gaps"), STAMPS_UNCHECKED));
    assert_eq!(one(&report, "max_sfb_histogram").value, "none");
    assert_eq!(one(&report, "max_sfb_max").value, "none");
    assert_eq!(one(&report, "max_sfb_hz").value, "none");
    for key in LEAD_KEYS {
        assert_eq!(one(&report, key).value, "refused", "{key}");
        assert_eq!(one(&report, key).note, TOO_FEW_FOR_A_LEAD, "{key}");
    }
    assert_eq!(one(&report, "after_stop_audio_sdus").value, "0");
    assert_eq!(one(&report, "after_stop_audio_ms").value, "none");
    assert_eq!(one(&report, "largest_packet").value, "8");

    let unmarked = analyse(&records, &[], "listen", CutLines::default());
    assert_eq!(rows(&unmarked, "trial_audio_ms").len(), 0);
    assert_eq!(rows(&unmarked, "clock_hz").len(), 0);
    let only_sent = analyse(&[to_pods(1, &START)], &[], "knock", CutLines::default());
    let largest = one(&only_sent, "largest_packet");
    assert_eq!(largest.value, "none");
    assert_eq!(largest.note, "no packet from the pods carries an opcode");
}

#[test]
fn audio_after_a_stop_is_counted_with_the_start_it_ends() {
    const FIRST_START_NS: u64 = 100 * NS_PER_MS;
    const STOP_NS: u64 = 400 * NS_PER_MS;
    const SECOND_STOP_NS: u64 = STOP_NS + 50 * NS_PER_MS;
    const SECOND_START_NS: u64 = 900 * NS_PER_MS;
    const NO_STOP: &str = "no stop mark ends this start";
    let sdu = |at_ns: u64| audio(at_ns, &OPAQUE, &[(0, &SILENT_UNIT)]);
    let records = [
        sdu(50 * NS_PER_MS),
        sdu(200 * NS_PER_MS),
        sdu(STOP_NS),
        sdu(STOP_NS + 30 * NS_PER_MS),
        from_pods(STOP_NS + 45 * NS_PER_MS, &EAR),
        sdu(STOP_NS + 70 * NS_PER_MS),
        sdu(SECOND_START_NS),
    ];
    let marks = [
        mark(FIRST_START_NS, MarkKind::Start),
        mark(STOP_NS, MarkKind::Stop),
        mark(SECOND_STOP_NS, MarkKind::Stop),
        mark(SECOND_START_NS, MarkKind::Start),
    ];
    let report = analyse(&records, &marks, "trials", CutLines::default());
    let shown_rows = |key: &str| -> Vec<_> { rows(&report, key).into_iter().map(shown).collect() };
    let counted = "start 1; from the stop mark to the next start mark; in no rate";
    assert_eq!(
        shown_rows("after_stop_audio_sdus"),
        [
            ("after_stop_audio_sdus", "3", "packets", counted),
            (
                "after_stop_audio_sdus",
                "none",
                "packets",
                "start 2; no stop mark ends this start"
            ),
        ]
    );
    let timed = "start 1; stop mark to the last audio SDU after it";
    assert_eq!(
        shown_rows("after_stop_audio_ms"),
        [
            ("after_stop_audio_ms", "70.000", "ms", timed),
            (
                "after_stop_audio_ms",
                "none",
                "ms",
                "start 2; no stop mark ends this start"
            ),
        ]
    );
    assert_eq!(one(&report, "audio_sdus_outside_a_start").value, "4");
    let in_a_start: Vec<_> = rows(&report, "segment_audio_sdus")
        .iter()
        .map(|row| row.value.as_str())
        .collect();
    assert_eq!(in_a_start, ["1", "1"]);

    let restarted = [
        mark(0, MarkKind::Start),
        mark(300 * NS_PER_MS, MarkKind::Start),
    ];
    let report = analyse(&records, &restarted, "trials", CutLines::default());
    for key in ["after_stop_audio_sdus", "after_stop_audio_ms"] {
        for row in rows(&report, key) {
            assert_eq!(row.value, "none", "{row:?}");
            assert!(has_note(row, NO_STOP), "{row:?}");
        }
        assert_eq!(rows(&report, key).len(), 2, "{key}");
    }

    let stray_stop = [
        mark(0, MarkKind::Stop),
        mark(FIRST_START_NS, MarkKind::Start),
        mark(STOP_NS, MarkKind::Stop),
    ];
    let report = analyse(&records, &stray_stop, "trials", CutLines::default());
    assert_eq!(one(&report, "after_stop_audio_sdus").value, "4");
    let outside = one(&report, "audio_sdus_outside_a_start");
    assert_eq!(outside.value, "5");
    let where_counted = "after_stop_audio_sdus has those after the stop mark that ends a start";
    assert!(has_note(outside, where_counted), "{outside:?}");
}

#[test]
fn lead_rows_use_the_segment_clock_or_say_nominal() {
    const START_DELAY_NS: u64 = 80 * NS_PER_MS;
    let marks = start_at_zero();

    let long = shifted(steady(FAST_HZ, 61), START_DELAY_NS);
    let report = analyse(&long, &marks, "soak", CutLines::default());
    let lead_row = one(&report, "lead_ms");
    assert!(close(number(lead_row), -80.0, 0.5), "{lead_row:?}");
    assert!(has_note(
        lead_row,
        "start mark minus the earliest arrival line"
    ));
    assert_eq!(
        shown(one(&report, "burst_packets")),
        (
            "burst_packets",
            "0",
            "packets",
            "start 1; frame from clock_units_per_second"
        )
    );
    assert_eq!(
        shown(one(&report, "burst_frames")),
        (
            "burst_frames",
            "0",
            "frames",
            "start 1; frame from clock_units_per_second"
        )
    );
    assert_eq!(one(&report, "burst_ms").value, "0.000");
    for key in LEAD_KEYS {
        let row = one(&report, key);
        assert!(
            has_note(row, OWN_FRAME) && !has_note(row, NOMINAL_FRAME),
            "{row:?}"
        );
        assert!(
            !has_note(row, "gap") && !has_note(row, "disagree"),
            "{row:?}"
        );
    }

    // At 7.5 ms a frame the line of a fast clock falls 15.5 ms over the ten seconds the lead reads.
    let short = shifted(steady(FAST_HZ, 12), START_DELAY_NS);
    let report = analyse(&short, &marks, "trials", CutLines::default());
    let lead_row = one(&report, "lead_ms");
    assert!(close(number(lead_row), -64.5, 0.5), "{lead_row:?}");
    for key in LEAD_KEYS {
        let row = one(&report, key);
        assert!(
            has_note(row, NOMINAL_FRAME) && !has_note(row, OWN_FRAME),
            "{row:?}"
        );
    }
}

#[test]
fn a_packet_with_no_whole_unit_is_no_evidence_for_the_lead() {
    /// The packets a lead needs in its window.
    const LEAD_NEEDS: u32 = 50;
    /// Ten packets over what a lead needs.
    const MORE_THAN_NEEDED: u64 = 60;
    /// The stream with units comes this late, so its line lies far behind the packet put before it.
    const HEARD_LATE_NS: u64 = 100 * NS_PER_MS;
    let marks = start_at_zero();
    let empty = |sdu: u64| audio(sdu * SDU_INTERVAL_NS, &OPAQUE, &[]);
    let lead_values = |records: &[Record]| {
        let report = analyse(records, &marks, "trials", CutLines::default());
        LEAD_KEYS.map(|key| one(&report, key).value.clone())
    };
    let assert_refused = |records: &[Record]| {
        let report = analyse(records, &marks, "trials", CutLines::default());
        for key in LEAD_KEYS {
            let row = one(&report, key);
            assert_eq!(row.value, "refused", "{row:?}");
            assert_eq!(row.note, TOO_FEW_FOR_A_LEAD, "{row:?}");
        }
    };

    let header_only: Vec<Record> = (1..=MORE_THAN_NEEDED).map(empty).collect();
    assert_refused(&header_only);

    // One packet with units short of the count, then packets with none up to ten over it.
    let mut mixed = stream(LEAD_NEEDS - 1, |_| OPAQUE);
    mixed.extend((u64::from(LEAD_NEEDS)..=MORE_THAN_NEEDED).map(empty));
    assert_eq!(mixed.len(), usize::try_from(MORE_THAN_NEEDED).unwrap());
    assert_refused(&mixed);

    // Enough packets with units give a lead, and a packet with none 70 ms before the first of them moves no figure.
    let heard = shifted(stream(LEAD_NEEDS, |_| OPAQUE), HEARD_LATE_NS);
    let mut behind_an_empty = vec![empty(2)];
    behind_an_empty.extend(heard.clone());
    let alone = lead_values(&heard);
    assert_eq!(alone, ["-100.000", "0", "0", "0.000"]);
    assert_eq!(lead_values(&behind_an_empty), alone);
}

#[test]
fn lead_rows_say_when_the_clock_their_frame_comes_from_is_bent() {
    let mut records = steady(64_000, 61);
    records.remove(1000);
    let report = analyse(&records, &start_at_zero(), "soak", CutLines::default());
    assert!(has_note(one(&report, "clock_units_per_second"), "disagree"));
    for key in LEAD_KEYS {
        let row = one(&report, key);
        assert!(has_note(row, OWN_FRAME), "{row:?}");
        assert!(has_note(row, "disagree") && has_note(row, "gap"), "{row:?}");
    }

    // A stall bends no slope past the limit, so the rows say `gap` alone.
    let mut stalled = steady(64_000, 61);
    stall(&mut stalled, 20 * NS_PER_SECOND, 200 * NS_PER_MS);
    let report = analyse(&stalled, &start_at_zero(), "soak", CutLines::default());
    assert!(!has_note(
        one(&report, "clock_units_per_second"),
        "disagree"
    ));
    for key in LEAD_KEYS {
        let row = one(&report, key);
        assert!(
            has_note(row, "gap") && !has_note(row, "disagree"),
            "{row:?}"
        );
    }

    // A late last SDU bends the ratio and opens no gap, so the rows say `disagree` alone.
    let report = analyse(&late_end(), &start_at_zero(), "soak", CutLines::default());
    for key in LEAD_KEYS {
        let row = one(&report, key);
        assert!(has_note(row, OWN_FRAME), "{row:?}");
        assert!(
            has_note(row, "disagree") && !has_note(row, "gap"),
            "{row:?}"
        );
    }
}

#[test]
fn max_sfb_hz_is_the_top_line_at_the_segment_clock() {
    let wide: &[u8] = &[0x00, 0x7c];
    let records: Vec<Record> = steady(FAST_HZ, 61)
        .iter()
        .zip(0_u32..)
        .map(|(record, k)| {
            let stamp = |unit: u32| (k * 4 + unit) * 480;
            let units = [
                (stamp(0), wide),
                (stamp(1), &SILENT_UNIT[..]),
                (stamp(2), &SILENT_UNIT[..]),
                (stamp(3), &SILENT_UNIT[..]),
            ];
            audio(record.at_ns, &OPAQUE, &units)
        })
        .collect();
    let report = analyse(&records, &start_at_zero(), "soak", CutLines::default());
    let hz = one(&report, "max_sfb_hz");
    assert!(ppm_apart(number(hz), TOP_LINE_HZ) < WITHIN_PPM, "{hz:?}");
    assert!(has_note(hz, "line 336 x clock_hz / 960"), "{hz:?}");
    assert!(has_note(hz, "band table made for 48000 Hz"), "{hz:?}");
    let declared = "a limit the coder declares, not a measured bandwidth";
    assert!(has_note(hz, declared), "{hz:?}");
    assert_eq!(one(&report, "max_sfb_max").value, "31");
    assert_eq!(one(&report, "max_sfb_histogram").value, "0:6108 31:2036");
    assert_eq!(one(&report, "gaps").value, "0");
}

#[test]
fn every_figure_scaled_by_the_unit_length_says_it_is_assumed() {
    const ASSUMED: &str = "480 samples per unit assumed, not measured";
    let report = analyse(
        &steady(64_000, 61),
        &start_at_zero(),
        "soak",
        CutLines::default(),
    );
    let scaled = [
        "clock_hz",
        "clock_hz_low",
        "clock_hz_high",
        "clock_ppm_vs_64000",
        "clock_ppm_vs_48000",
        "max_sfb_hz",
    ];
    for key in scaled {
        let row = one(&report, key);
        assert!(has_note(row, ASSUMED), "{row:?}");
    }
    for key in RATE_KEYS.iter().filter(|key| !scaled.contains(key)) {
        let row = one(&report, key);
        assert!(!has_note(row, ASSUMED), "{row:?}");
    }
    assert_eq!(
        one(&report, "clock_hz").note,
        "start 1; slope x 480 samples per unit; 480 samples per unit assumed, not measured"
    );
}

#[test]
fn the_clock_is_counted_in_units_whatever_the_stamps_step_by() {
    /// The stamp step of a unit, which is not its length in samples.
    const STAMP_STEP: u32 = 180;
    /// Units a second when a step of `STAMP_STEP` is clocked at 24000 Hz: the same 7.5 ms a unit as 480 samples at 64000 Hz.
    const UNITS_PER_SECOND: f64 = 24_000.0 / 180.0;
    let records = synth(24_000, STAMP_STEP, 4, 61, 0, SEED);
    let report = analyse(&records, &start_at_zero(), "soak", CutLines::default());
    let units = one(&report, "clock_units_per_second");
    assert!(
        ppm_apart(number(units), UNITS_PER_SECOND) < WITHIN_PPM,
        "{units:?}"
    );
    let hz = one(&report, "clock_hz");
    assert!(ppm_apart(number(hz), 64_000.0) < WITHIN_PPM, "{hz:?}");
    assert!(has_note(hz, "slope x 480 samples per unit"), "{hz:?}");
    assert_eq!(one(&report, "stamp_delta_per_unit").value, "180");
    assert_eq!(one(&report, "stamp_delta_per_sdu").value, "720");
    assert_eq!(one(&report, "stamp_unit").value, "unknown");
    assert_eq!(one(&report, "gaps").value, "0");
    assert_eq!(rows(&report, "gap").len(), 0);
}

#[test]
fn opaque_and_stamp_rows_name_what_was_found() {
    let records = stream(600, |k| {
        let mut opaque = OPAQUE;
        let skipped_once = u32::from(k >= 300);
        opaque[5] = (k * 3 + skipped_once).to_le_bytes()[0];
        opaque[9] = (k * k % 7).to_le_bytes()[0];
        opaque
    });
    let report = analyse(&records, &start_at_zero(), "voice", CutLines::default());
    let fields = one(&report, "opaque_fields");
    let tokens: Vec<_> = fields.value.split(' ').collect();
    assert_eq!(tokens.len(), 14);
    assert_eq!(tokens[4], "constant(0xa4)");
    assert_eq!(tokens[5], "counter(step=3,modulus=256,share=0.9983)");
    assert_eq!(tokens[9], "varying(distinct=4)");
    assert!(has_note(fields, "600 SDUs"), "{fields:?}");
    assert!(has_note(fields, "a counter here is never a gap"));
    assert_eq!(one(&report, "gaps").value, "0");
    assert_eq!(rows(&report, "gap").len(), 0);

    let odd: Vec<Record> = (0..60_u32)
        .map(|k| {
            audio(
                u64::from(k + 1) * 30 * NS_PER_MS,
                &OPAQUE,
                &[(k * 1000, &SILENT_UNIT)],
            )
        })
        .collect();
    let report = analyse(&odd, &start_at_zero(), "voice", CutLines::default());
    let unit = one(&report, "stamp_unit");
    assert_eq!(
        shown(unit),
        (
            "stamp_unit",
            "unknown",
            "name",
            "start 1; modal delta per unit 1000"
        )
    );
    let micro: Vec<Record> = (0..60_u32)
        .map(|k| {
            audio(
                u64::from(k + 1) * 30 * NS_PER_MS,
                &OPAQUE,
                &[(k * 7500, &SILENT_UNIT)],
            )
        })
        .collect();
    let report = analyse(&micro, &start_at_zero(), "voice", CutLines::default());
    assert_eq!(one(&report, "stamp_unit").value, "microseconds");
}

#[test]
fn length_rows_count_the_sdus_whose_header_holds_their_length() {
    /// An SDU of `stream`: the 22 byte header and four units of a 5 byte prefix and 2 coded bytes.
    const SDU_LEN: u16 = 50;
    let records = stream(600, |k| {
        let mut opaque = OPAQUE;
        opaque[..2].copy_from_slice(&(SDU_LEN - 10).to_le_bytes());
        let second = if k % 3 == 0 { SDU_LEN } else { SDU_LEN - 16 };
        opaque[6..8].copy_from_slice(&second.to_le_bytes());
        opaque
    });
    let report = analyse(&records, &start_at_zero(), "voice", CutLines::default());
    assert_eq!(
        shown(one(&report, "header_length_at_8")),
        (
            "header_length_at_8",
            "600",
            "packets",
            "start 1; of 600 SDUs; little endian u16 from header byte 8 against the SDU length less 10"
        )
    );
    assert_eq!(
        shown(one(&report, "header_length_at_14")),
        (
            "header_length_at_14",
            "400",
            "packets",
            "start 1; of 600 SDUs; little endian u16 from header byte 14 against the SDU length less 16"
        )
    );
}

#[test]
fn gap_rows_say_what_showed_each_gap_and_stop_at_twenty() {
    let mut records = stream(600, |k| {
        let mut opaque = OPAQUE;
        opaque[5] = k.to_le_bytes()[0];
        opaque
    });
    records.drain(200..203);
    for record in &mut records[400..] {
        record.at_ns += 70 * NS_PER_MS;
    }
    let report = analyse(&records, &start_at_zero(), "voice", CutLines::default());
    assert_eq!(one(&report, "gaps").value, "2");
    let listed: Vec<_> = rows(&report, "gap").into_iter().map(shown).collect();
    let expected = [
        (
            "gap",
            "6120.000",
            "ms",
            "start 1; after the start mark; late 120.000 ms; stamps 384000 to 389280 missing",
        ),
        (
            "gap",
            "12190.000",
            "ms",
            "start 1; after the start mark; late 100.000 ms",
        ),
    ];
    assert_eq!(listed, expected);

    let mut lossy = stream(600, |_| OPAQUE);
    lossy.retain(|record| !(record.at_ns / (30 * NS_PER_MS)).is_multiple_of(10));
    let report = analyse(&lossy, &start_at_zero(), "voice", CutLines::default());
    let count = one(&report, "gaps");
    assert_eq!(count.value, "59");
    assert!(has_note(count, "first 20 listed"), "{count:?}");
    assert_eq!(rows(&report, "gap").len(), 20);
    let irregular = [
        audio(
            30 * NS_PER_MS,
            &OPAQUE,
            &[(0, &SILENT_UNIT), (480, &SILENT_UNIT), (960, &SILENT_UNIT)],
        ),
        audio(
            60 * NS_PER_MS,
            &OPAQUE,
            &[
                (1440, &SILENT_UNIT),
                (1540, &SILENT_UNIT),
                (2020, &SILENT_UNIT),
            ],
        ),
    ];
    let report = analyse(&irregular, &start_at_zero(), "voice", CutLines::default());
    assert!(has_note(
        one(&report, "gap"),
        "stamp delta 100 where 480 was due"
    ));
}

#[test]
fn the_gaps_row_says_when_the_stamps_could_not_show_a_gap() {
    /// SDUs in each stream, 30 ms apart.
    const SDUS: u32 = 60;
    /// The SDU taken out: the 60 ms interval it leaves is under the 90 ms a late arrival must pass.
    const LOST: usize = 30;
    /// The stamp step from SDU to SDU of the stream whose four units of one SDU share a stamp.
    const SHARED_STAMP_STEP: u32 = 1000;
    let mut shared: Vec<Record> = stream(SDUS, |_| OPAQUE)
        .iter()
        .zip(0_u32..)
        .map(|(record, k)| {
            let unit: (u32, &[u8]) = (k * SHARED_STAMP_STEP, &SILENT_UNIT[..]);
            audio(record.at_ns, &OPAQUE, &[unit; 4])
        })
        .collect();
    shared.remove(LOST);
    let report = analyse(&shared, &start_at_zero(), "voice", CutLines::default());
    assert_eq!(one(&report, "stamp_delta_per_unit").value, "0");
    assert_eq!(
        shown(one(&report, "gaps")),
        (
            "gaps",
            "0",
            "gaps",
            "start 1; arrival over 90 ms; stamps not checked, no modal step from unit to unit"
        )
    );

    let mut stepping = stream(SDUS, |_| OPAQUE);
    stepping.remove(LOST);
    let report = analyse(&stepping, &start_at_zero(), "voice", CutLines::default());
    let count = one(&report, "gaps");
    assert_eq!(count.value, "1");
    assert!(!has_note(count, STAMPS_UNCHECKED), "{count:?}");
}

#[test]
fn a_gap_is_timed_from_its_start_mark() {
    const START_NS: u64 = 5 * NS_PER_SECOND;
    let mut records = shifted(stream(600, |_| OPAQUE), START_NS);
    records.remove(200);
    let report = analyse(
        &records,
        &[mark(START_NS, MarkKind::Start)],
        "voice",
        CutLines::default(),
    );
    assert_eq!(one(&report, "gap").value, "6060.000");
}
