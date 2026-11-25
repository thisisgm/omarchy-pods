use pods_proto::sdulog::Record;
use pods_proto::uplink::OPAQUE_LEN;

use super::super::load::load;
use super::super::support::{
    NS_PER_MS, OPAQUE, SDU_INTERVAL_NS, SEED, SILENT_UNIT, STAMP_STEP, audio, close, stream,
};
use super::super::synth::synth;
use super::*;

/// SDUs in the hand built streams, enough for a one byte counter to wrap twice.
const SDUS: u32 = 600;
const U32_MODULUS: u64 = 1 << 32;
const COUNTER_AT: usize = 5;
const STEPS: usize = 599;
const CLOCK_AT: usize = 10;
const CLOCK_STEP_MS: u32 = 30;
/// The stamp step from SDU to SDU of a stream whose units of one SDU share a stamp.
const SHARED_STAMP_STEP: u32 = 1000;

fn opaque_with(at: usize, bytes: &[u8]) -> [u8; OPAQUE_LEN] {
    let mut opaque = OPAQUE;
    opaque[at..][..bytes.len()].copy_from_slice(bytes);
    opaque
}

fn counted(step: u32) -> Vec<Record> {
    stream(SDUS, |k| {
        opaque_with(COUNTER_AT, &k.wrapping_mul(step).to_le_bytes()[..1])
    })
}

fn constants() -> [FieldKind; OPAQUE_LEN] {
    OPAQUE.map(FieldKind::Constant)
}

fn steady_counter(step: u32, modulus: u64) -> FieldKind {
    FieldKind::Counter {
        step,
        modulus,
        on_step: STEPS,
        steps: STEPS,
    }
}

/// A stream whose opaque bytes from `CLOCK_AT` hold a clock that adds `step_of(k)` milliseconds on the way to SDU k.
fn clocked(step_of: impl Fn(u32) -> u32) -> Vec<Record> {
    let mut clock_ms = 0x00ff_fe00_u32;
    let clock: Vec<u32> = (0..SDUS)
        .map(|k| {
            clock_ms = clock_ms.wrapping_add(step_of(k));
            clock_ms
        })
        .collect();
    stream(SDUS, |k| {
        opaque_with(CLOCK_AT, &clock[usize::try_from(k).unwrap()].to_le_bytes())
    })
}

/// A stream whose four units of one SDU share a stamp, so the modal step from unit to unit is 0.
fn stamped_per_sdu(sdus: u32) -> Vec<Record> {
    stream(sdus, |_| OPAQUE)
        .iter()
        .zip(0_u32..)
        .map(|(record, k)| {
            let unit: (u32, &[u8]) = (k.wrapping_mul(SHARED_STAMP_STEP), &SILENT_UNIT[..]);
            audio(record.at_ns, &OPAQUE, &[unit; 4])
        })
        .collect()
}

/// A clock whose every step is 28 to 32 milliseconds, as a clock read at each send makes it.
fn jittered_steps(k: u32) -> u32 {
    28_u32.saturating_add(k.wrapping_mul(7) % 5)
}

/// A clock that steps 15 then 45 where 30 twice was due, once in 50 SDUs, as a late packet makes it.
fn paired_steps(k: u32) -> u32 {
    match k % 50 {
        10 => 15,
        11 => 45,
        _ => CLOCK_STEP_MS,
    }
}

#[test]
fn a_one_byte_counter_in_the_opaque_header_is_found_with_its_step() {
    for step in [1, 3, 255] {
        let mut expected = constants();
        expected[COUNTER_AT] = steady_counter(step, 256);
        assert_eq!(
            opaque_fields(&load(&counted(step))),
            expected,
            "step {step}"
        );
    }
}

#[test]
fn a_one_byte_counter_is_still_found_when_a_few_sdus_are_lost() {
    let mut records = counted(3);
    records.remove(400);
    records.remove(100);
    let mut expected = constants();
    expected[COUNTER_AT] = FieldKind::Counter {
        step: 3,
        modulus: 256,
        on_step: 595,
        steps: 597,
    };
    assert_eq!(opaque_fields(&load(&records)), expected);

    let mut lossy = counted(3);
    lossy.retain(|record| !(record.at_ns / SDU_INTERVAL_NS).is_multiple_of(10));
    let fields = opaque_fields(&load(&lossy));
    assert!(
        matches!(fields[COUNTER_AT], FieldKind::Varying { .. }),
        "{fields:?}"
    );
}

#[test]
fn a_one_byte_counter_may_leave_its_step_once_in_twenty_steps_and_no_more() {
    const SDUS_OF_600_STEPS: u32 = 601;
    /// The counter skips a count at every SDU this many apart: 30 of the 600 steps, the most a counter may miss.
    const SKIP_EVERY: u32 = 20;
    /// An SDU between two of those, where one more skip makes 31.
    const ONE_MORE_AT: u32 = 10;
    let skipping = |one_more: bool| {
        stream(SDUS_OF_600_STEPS, |k| {
            let skips = k / SKIP_EVERY + u32::from(one_more && k >= ONE_MORE_AT);
            opaque_with(COUNTER_AT, &(k + skips).to_le_bytes()[..1])
        })
    };
    let at_the_limit = FieldKind::Counter {
        step: 1,
        modulus: 256,
        on_step: 570,
        steps: 600,
    };
    assert_eq!(
        opaque_fields(&load(&skipping(false)))[COUNTER_AT],
        at_the_limit
    );
    let over = opaque_fields(&load(&skipping(true)));
    assert!(
        matches!(over[COUNTER_AT], FieldKind::Varying { .. }),
        "{over:?}"
    );
}

#[test]
fn a_u32_counter_is_found() {
    for (at, first, step) in [(0, 250_u32, 1), (4, 0xffff_fe00, 300), (0, 0, 65_536 + 1)] {
        let records = stream(SDUS, |k| {
            opaque_with(at, &first.wrapping_add(k * step).to_le_bytes())
        });
        let mut expected = constants();
        let counter = steady_counter(step, U32_MODULUS);
        expected[at..at + 4].fill(counter);
        assert_eq!(opaque_fields(&load(&records)), expected, "at {at}");
    }
}

#[test]
fn a_u16_counter_is_found() {
    let records = stream(SDUS, |k| {
        let value = (65_000 + k * 7) % 65_536;
        opaque_with(8, &value.to_le_bytes()[..2])
    });
    let mut expected = constants();
    let counter = steady_counter(7, 65_536);
    expected[8..10].fill(counter);
    assert_eq!(opaque_fields(&load(&records)), expected);
}

#[test]
fn a_counter_in_one_byte_is_not_read_as_a_wider_one() {
    for at in [0, 1, 4, 7, 12, 13] {
        let records = stream(SDUS, |k| opaque_with(at, &k.to_le_bytes()[..1]));
        let mut expected = constants();
        expected[at] = steady_counter(1, 256);
        assert_eq!(opaque_fields(&load(&records)), expected, "at {at}");
    }
    let two = stream(SDUS, |k| {
        let mut opaque = opaque_with(0, &k.to_le_bytes()[..1]);
        opaque[1] = (k * 5).to_le_bytes()[0];
        opaque
    });
    let mut expected = constants();
    expected[0] = steady_counter(1, 256);
    expected[1] = steady_counter(5, 256);
    assert_eq!(opaque_fields(&load(&two)), expected);
}

#[test]
fn a_constant_byte_is_constant() {
    let records = stream(SDUS, |_| OPAQUE);
    assert_eq!(opaque_fields(&load(&records)), constants());
    let one = stream(1, |_| OPAQUE);
    assert_eq!(opaque_fields(&load(&one)), constants());
}

#[test]
fn a_byte_that_follows_no_rule_is_varying() {
    let records = stream(SDUS, |k| opaque_with(2, &(k * k % 7).to_le_bytes()[..1]));
    let mut expected = constants();
    expected[2] = FieldKind::Varying { distinct: 4 };
    assert_eq!(opaque_fields(&load(&records)), expected);
    let nothing = [FieldKind::Varying { distinct: 0 }; OPAQUE_LEN];
    assert_eq!(opaque_fields(&load(&[])), nothing);
}

#[test]
fn stamp_delta_480_reads_as_samples() {
    let report = stamps(&load(&synth(64_000, 480, 4, 61, 0, SEED)));
    let per_unit = report.per_unit.unwrap();
    assert_eq!(per_unit.delta, 480);
    assert!(close(per_unit.share, 1.0, 1e-12));
    let per_sdu = report.per_sdu.unwrap();
    assert_eq!(per_sdu.delta, 1920);
    assert!(close(per_sdu.share, 1.0, 1e-12));
    assert_eq!(report.implied(), Some(StampUnit::Samples));
    assert_eq!(report.step(), Some(480));

    assert_eq!(implied_unit(480), StampUnit::Samples);
    assert_eq!(implied_unit(512), StampUnit::Samples);
    assert_eq!(implied_unit(7500), StampUnit::Microseconds);
    for other in [0, 1, 479, 481, 960, 7499, 1920] {
        assert_eq!(implied_unit(other), StampUnit::Unknown(other));
    }
}

#[test]
fn stamp_report_of_a_stream_with_other_deltas() {
    let microseconds = stamps(&load(&synth(64_000, 7500, 4, 61, 0, SEED)));
    assert_eq!(microseconds.implied(), Some(StampUnit::Microseconds));
    let report = stamps(&load(&stamped_per_sdu(8)));
    let per_unit = report.per_unit.unwrap();
    assert_eq!(per_unit.delta, 0);
    assert!(close(per_unit.share, 24.0 / 31.0, 1e-12));
    assert_eq!(report.per_sdu.unwrap().delta, 1000);
    assert_eq!(report.implied(), Some(StampUnit::Unknown(0)));
    assert_eq!(report.step(), None);

    let nothing = stamps(&load(&[]));
    assert_eq!((nothing.per_unit, nothing.per_sdu), (None, None));
    assert_eq!(nothing.implied(), None);
    assert_eq!(nothing.step(), None);
    let one_unit = stamps(&load(&[audio(1, &OPAQUE, &[(7, &SILENT_UNIT)])]));
    assert_eq!((one_unit.per_unit, one_unit.per_sdu), (None, None));
}

#[test]
fn stamps_with_no_step_from_unit_to_unit_show_no_gap_and_arrivals_still_do() {
    let mut records = stamped_per_sdu(SDUS);
    records.remove(300);
    let loaded = load(&records);
    assert_eq!(stamps(&loaded).step(), None);
    assert_eq!(gaps(&loaded), []);

    records.drain(100..103);
    let late = Gap {
        sdu: 100,
        at_ns: records[100].at_ns,
        late_ns: Some(4 * SDU_INTERVAL_NS),
        stamps: None,
    };
    assert_eq!(gaps(&load(&records)), [late]);
}

#[test]
fn stamp_delta_survives_the_wrap_of_the_stamp() {
    let first = u32::MAX - 1000;
    let records: Vec<_> = stream(SDUS, |_| OPAQUE)
        .iter()
        .zip(0_u32..)
        .map(|(record, k)| {
            let stamp = |unit: u32| first.wrapping_add((k * 4 + unit) * STAMP_STEP);
            let units: Vec<(u32, &[u8])> =
                (0..4).map(|unit| (stamp(unit), &SILENT_UNIT[..])).collect();
            audio(record.at_ns, &OPAQUE, &units)
        })
        .collect();
    let loaded = load(&records);
    let report = stamps(&loaded);
    assert_eq!(report.per_unit.unwrap().delta, 180);
    assert!(close(report.per_unit.unwrap().share, 1.0, 1e-12));
    assert_eq!(gaps(&loaded), []);
}

#[test]
fn a_clean_stream_has_no_gap() {
    assert_eq!(gaps(&load(&synth(64_000, 480, 4, 61, 0, SEED))), []);
    assert_eq!(gaps(&load(&counted(3))), []);
    assert_eq!(gaps(&load(&[])), []);
}

#[test]
fn three_lost_sdus_in_a_row_are_late_too() {
    let mut records = counted(1);
    records.drain(200..203);
    let found = gaps(&load(&records));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].late_ns, Some(4 * SDU_INTERVAL_NS));
    let first_lost = 200 * 4 * STAMP_STEP;
    let missing = StampBreak::Missing {
        first: first_lost,
        last: first_lost + 11 * STAMP_STEP,
    };
    assert_eq!(found[0].stamps, Some(missing));
}

#[test]
fn an_arrival_interval_is_a_gap_only_over_90_ms() {
    let mut records = stream(SDUS, |_| OPAQUE);
    for record in &mut records[100..] {
        record.at_ns += 60 * NS_PER_MS;
    }
    assert_eq!(gaps(&load(&records)), []);
    for record in &mut records[100..] {
        record.at_ns += 1;
    }
    let expected = Gap {
        sdu: 100,
        at_ns: records[100].at_ns,
        late_ns: Some(90 * NS_PER_MS + 1),
        stamps: None,
    };
    assert_eq!(gaps(&load(&records)), [expected]);
}

#[test]
fn a_header_byte_that_looks_like_a_counter_and_skips_is_no_gap() {
    let paired = load(&clocked(paired_steps));
    let low_byte = FieldKind::Counter {
        step: CLOCK_STEP_MS,
        modulus: 256,
        on_step: 575,
        steps: STEPS,
    };
    assert_eq!(opaque_fields(&paired)[CLOCK_AT], low_byte);
    assert_eq!(gaps(&paired), []);

    let jittered = load(&clocked(jittered_steps));
    let fields = opaque_fields(&jittered);
    assert!(
        matches!(fields[CLOCK_AT], FieldKind::Varying { .. }),
        "{fields:?}"
    );
    assert_eq!(gaps(&jittered), []);

    let skipping = stream(SDUS, |k| {
        let count = if k < 50 { k } else { k + 2 };
        opaque_with(COUNTER_AT, &count.to_le_bytes()[..1])
    });
    assert_eq!(gaps(&load(&skipping)), []);
}

#[test]
fn a_withheld_sdu_is_found_by_its_stamps_whatever_the_header_does() {
    const WITHHELD: usize = 300;
    let patterns: [fn(u32) -> u32; 2] = [paired_steps, jittered_steps];
    for step_of in patterns {
        let mut records = clocked(step_of);
        let after_ns = records[WITHHELD + 1].at_ns;
        records.remove(WITHHELD);
        let first_lost = u32::try_from(WITHHELD).unwrap() * 4 * STAMP_STEP;
        let expected = Gap {
            sdu: WITHHELD,
            at_ns: after_ns,
            late_ns: None,
            stamps: Some(StampBreak::Missing {
                first: first_lost,
                last: first_lost + 3 * STAMP_STEP,
            }),
        };
        assert_eq!(gaps(&load(&records)), [expected]);
    }
}

#[test]
fn length_agrees_counts_the_sdus_whose_header_holds_their_length() {
    const LEN_AT: usize = 6;
    const LEFT_OUT: usize = 16;
    let unit: (u32, &[u8]) = (0, &SILENT_UNIT);
    let sdu_len = |units: usize| 22 + units * 7;
    let declared = |units: usize, less: usize| {
        let len = u16::try_from(sdu_len(units) - less).unwrap();
        opaque_with(LEN_AT, &len.to_le_bytes())
    };
    let records = [
        audio(1, &declared(4, LEFT_OUT), &[unit; 4]),
        audio(2, &declared(2, LEFT_OUT), &[unit; 2]),
        audio(3, &declared(4, LEFT_OUT), &[unit; 3]),
        audio(4, &declared(4, LEFT_OUT - 1), &[unit; 4]),
    ];
    let loaded = load(&records);
    assert_eq!(length_agrees(&loaded, LEN_AT, LEFT_OUT), 2);
    assert_eq!(length_agrees(&loaded, LEN_AT, LEFT_OUT - 1), 1);
    assert_eq!(length_agrees(&loaded, LEN_AT, 4096), 0);
    assert_eq!(length_agrees(&loaded, OPAQUE_LEN - 1, LEFT_OUT), 0);
    assert_eq!(length_agrees(&load(&[]), LEN_AT, LEFT_OUT), 0);
}

#[test]
fn a_stamp_delta_that_is_no_whole_step_is_named_irregular() {
    let mut records = stream(SDUS, |_| OPAQUE);
    let at_ns = records[40].at_ns;
    let stamps: Vec<(u32, &[u8])> = [28_800, 28_980, 29_080, 29_260]
        .into_iter()
        .map(|stamp| (stamp, &SILENT_UNIT[..]))
        .collect();
    records[40] = audio(at_ns, &OPAQUE, &stamps);
    let found = gaps(&load(&records));
    let inside = Gap {
        sdu: 40,
        at_ns,
        late_ns: None,
        stamps: Some(StampBreak::Irregular { delta: 100 }),
    };
    let after = Gap {
        sdu: 41,
        at_ns: records[41].at_ns,
        late_ns: None,
        stamps: Some(StampBreak::Irregular { delta: 260 }),
    };
    assert_eq!(found, [inside, after]);
}

#[test]
fn a_stamp_that_steps_back_is_never_units_that_did_not_come() {
    const STEP: u32 = 512;
    const REPEATING: usize = 40;
    let stamped = |at_ns: u64, first_unit: u32| {
        let units: Vec<(u32, &[u8])> = (0..4)
            .map(|unit| ((first_unit + unit) * STEP, &SILENT_UNIT[..]))
            .collect();
        audio(at_ns, &OPAQUE, &units)
    };
    let mut records: Vec<Record> = stream(SDUS, |_| OPAQUE)
        .iter()
        .zip(0_u32..)
        .map(|(record, k)| stamped(record.at_ns, k * 4))
        .collect();
    let at_ns = records[REPEATING].at_ns;
    records[REPEATING] = stamped(at_ns, 158);

    let found = gaps(&load(&records));
    let back = Gap {
        sdu: REPEATING,
        at_ns,
        late_ns: None,
        stamps: Some(StampBreak::Irregular {
            delta: 0_u32.wrapping_sub(STEP),
        }),
    };
    let forward = Gap {
        sdu: REPEATING + 1,
        at_ns: records[REPEATING + 1].at_ns,
        late_ns: None,
        stamps: Some(StampBreak::Missing {
            first: 162 * STEP,
            last: 163 * STEP,
        }),
    };
    assert_eq!(found, [back, forward]);
}

#[test]
fn only_whole_steps_forward_within_half_the_range_leave_stamps_out() {
    const HALF: u32 = 1 << 31;
    let missing = StampBreak::Missing {
        first: 1,
        last: HALF - 2,
    };
    assert_eq!(broken(0, HALF - 1, 1), missing);
    assert_eq!(broken(0, HALF, 1), StampBreak::Irregular { delta: HALF });
    assert_eq!(broken(7, 7, 480), StampBreak::Irregular { delta: 0 });
}
