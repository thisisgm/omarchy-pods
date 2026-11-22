use super::super::load::load;
use super::super::shape::gaps;
use super::super::stats::ppm;
use super::super::support::{
    NS_PER_MS, NS_PER_SECOND, OPAQUE, SEED, audio, close, ppm_apart, shifted, stall,
};
use super::super::synth::synth;
use super::*;

const SAMPLES_PER_UNIT: u32 = 480;
const UNITS_PER_SDU: u32 = 4;
/// Ten minutes, the length of a soak.
const TEN_MINUTES_S: u32 = 600;
/// How close the estimate must come to the rate the stream was generated at.
const WITHIN_PPM: f64 = 20.0;
/// How close a figure must be to the one worked out by hand, as a fraction of it.
const TIGHT: f64 = 1e-9;

fn steady(rate_hz: u32, seconds: u32) -> Vec<pods_proto::sdulog::Record> {
    synth(rate_hz, SAMPLES_PER_UNIT, UNITS_PER_SDU, seconds, 0, SEED)
}

#[test]
fn clock_estimate_is_within_20_ppm_on_a_clean_ten_minute_log() {
    for rate_hz in [64_000, 48_000, 64_010, 47_990] {
        let truth = f64::from(rate_hz);
        let estimate = clock(&load(&steady(rate_hz, TEN_MINUTES_S)), SAMPLES_PER_UNIT).unwrap();
        assert!(
            ppm_apart(estimate.hz(), truth) < WITHIN_PPM,
            "{rate_hz}: {estimate:?}"
        );
        let ratio_hz = estimate.ratio * f64::from(SAMPLES_PER_UNIT);
        assert!(
            ppm_apart(ratio_hz, truth) < WITHIN_PPM,
            "{rate_hz}: {estimate:?}"
        );
        assert!(estimate.hz_low() <= estimate.hz() && estimate.hz() <= estimate.hz_high());
        assert!(ppm_apart(estimate.hz_low(), estimate.hz_high()) < WITHIN_PPM);
        assert!(!estimate.disagree(), "{rate_hz}: {estimate:?}");
        let expected_ppm = ppm(truth, 64_000.0);
        assert!(close(estimate.ppm_vs(64_000.0), expected_ppm, WITHIN_PPM));
        let expected_ppm = ppm(truth, 48_000.0);
        assert!(close(estimate.ppm_vs(48_000.0), expected_ppm, WITHIN_PPM));
    }
}

#[test]
fn clock_estimate_survives_jitter_and_a_stall() {
    const JITTER_MS: u32 = 5;
    const STALL_AT_NS: u64 = 300 * NS_PER_SECOND;
    const STALL_NS: u64 = 300 * NS_PER_MS;
    let mut records = synth(
        64_000,
        SAMPLES_PER_UNIT,
        UNITS_PER_SDU,
        TEN_MINUTES_S,
        JITTER_MS,
        SEED,
    );
    stall(&mut records, STALL_AT_NS, STALL_NS);
    let loaded = load(&records);

    let estimate = clock(&loaded, SAMPLES_PER_UNIT).unwrap();
    assert!(
        ppm_apart(estimate.hz(), 64_000.0) < WITHIN_PPM,
        "{estimate:?}"
    );
    assert!(!estimate.disagree(), "{estimate:?}");
    assert!(estimate.half_width > 0.0, "{estimate:?}");
    let per_unit = f64::from(SAMPLES_PER_UNIT);
    let low = (estimate.units_per_second - estimate.half_width) * per_unit;
    let high = (estimate.units_per_second + estimate.half_width) * per_unit;
    assert!(close(estimate.hz_low(), low, 1e-9) && close(estimate.hz_high(), high, 1e-9));
    assert!(estimate.hz_low() < estimate.hz() && estimate.hz() < estimate.hz_high());

    let found = gaps(&loaded);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].at_ns, STALL_AT_NS + STALL_NS);
    let late_ns = found[0].late_ns.unwrap();
    assert!((STALL_NS - 40 * NS_PER_MS..=STALL_NS + 40 * NS_PER_MS).contains(&late_ns));
    assert_eq!(found[0].stamps, None);
}

#[test]
fn clock_estimate_moves_with_the_samples_per_unit() {
    let loaded = load(&steady(64_000, TEN_MINUTES_S));
    let estimate = clock(&loaded, 512).unwrap();
    assert!(ppm_apart(estimate.hz(), 64_000.0 * 512.0 / 480.0) < WITHIN_PPM);
    assert_eq!(estimate.samples_per_unit, 512);
}

#[test]
fn clock_estimate_does_not_move_with_where_the_stamps_start() {
    /// A host clock far past 2^53 ns, where an f64 no longer holds every nanosecond.
    const FAR_NS: u64 = 1 << 62;
    let near = clock(&load(&steady(64_010, TEN_MINUTES_S)), SAMPLES_PER_UNIT).unwrap();
    let far = shifted(steady(64_010, TEN_MINUTES_S), FAR_NS);
    assert_eq!(clock(&load(&far), SAMPLES_PER_UNIT).unwrap(), near);
}

#[test]
fn the_slope_and_the_ratio_disagree_when_one_end_is_late() {
    /// A last SDU this late stretches the ten minute span, and so lowers the ratio, by 50 parts in a million.
    const LIMIT_MS: u64 = 30;
    let last_sdu_late_by = |late_ms: u64| {
        let mut records = steady(64_000, TEN_MINUTES_S);
        records.last_mut().unwrap().at_ns += late_ms * NS_PER_MS;
        clock(&load(&records), SAMPLES_PER_UNIT).unwrap()
    };
    assert!(!last_sdu_late_by(0).disagree());
    let over = last_sdu_late_by(LIMIT_MS + 1);
    assert!(over.disagree(), "{over:?}");
    assert!(ppm_apart(over.hz(), 64_000.0) < WITHIN_PPM, "{over:?}");
    let under = last_sdu_late_by(LIMIT_MS - 1);
    assert!(!under.disagree(), "{under:?}");
}

#[test]
fn short_run_is_refused() {
    let short = load(&steady(64_000, 29));
    let thin = Thin {
        sdus: 966,
        span_ns: 28_950 * NS_PER_MS,
    };
    assert_eq!(cadence(&short).unwrap_err(), thin);
    assert_eq!(clock(&short, SAMPLES_PER_UNIT).unwrap_err(), thin);
    assert_eq!(bitrate(&short).unwrap_err(), thin);

    let mut slow = synth(64_000, SAMPLES_PER_UNIT, 20, 76, 0, SEED);
    slow.truncate(499);
    let few = load(&slow);
    let thin = Thin {
        sdus: 499,
        span_ns: 74_700 * NS_PER_MS,
    };
    assert_eq!(cadence(&few).unwrap_err(), thin);
    assert_eq!(clock(&few, SAMPLES_PER_UNIT).unwrap_err(), thin);
    assert_eq!(bitrate(&few).unwrap_err(), thin);
}

#[test]
fn thirty_seconds_and_five_hundred_sdus_are_enough() {
    let mut records = steady(64_000, 31);
    records.truncate(1001);
    let exact_span = load(&records);
    assert!(cadence(&exact_span).is_ok());
    assert!(clock(&exact_span, SAMPLES_PER_UNIT).is_ok());
    assert!(bitrate(&exact_span).is_ok());
    records.truncate(1000);
    let thin = Thin {
        sdus: 1000,
        span_ns: 29_970 * NS_PER_MS,
    };
    assert_eq!(clock(&load(&records), SAMPLES_PER_UNIT).unwrap_err(), thin);

    let mut slow = synth(64_000, SAMPLES_PER_UNIT, 20, 76, 0, SEED);
    slow.truncate(500);
    let exact_count = load(&slow);
    assert!(cadence(&exact_count).is_ok());
    assert!(clock(&exact_count, SAMPLES_PER_UNIT).is_ok());
    assert!(bitrate(&exact_count).is_ok());
}

#[test]
fn no_audio_is_refused() {
    let thin = Thin {
        sdus: 0,
        span_ns: 0,
    };
    let nothing = load(&[]);
    assert_eq!(cadence(&nothing).unwrap_err(), thin);
    assert_eq!(clock(&nothing, SAMPLES_PER_UNIT).unwrap_err(), thin);
    assert_eq!(bitrate(&nothing).unwrap_err(), thin);
}

#[test]
fn cadence_counts_intervals_not_packets() {
    let found = cadence(&load(&steady(64_000, 61))).unwrap();
    let per_second = 1000.0 / 30.0;
    assert!(
        close(found.sdus_per_second, per_second, per_second * TIGHT),
        "{found:?}"
    );
    assert_eq!(found.units_per_sdu, 4);
    assert!(close(found.units_per_sdu_share, 1.0, TIGHT));
    let interval_ns = 30 * NS_PER_MS;
    assert_eq!(
        (
            found.interval_min_ns,
            found.interval_median_ns,
            found.interval_max_ns
        ),
        (interval_ns, interval_ns, interval_ns)
    );
}

#[test]
fn cadence_names_the_modal_unit_count_and_the_interval_range() {
    let mut records = steady(64_000, 61);
    let short = audio(records[10].at_ns, &OPAQUE, &[(0, &[0x00, 0x00])]);
    records[10] = short;
    records[20].at_ns -= 10 * NS_PER_MS;
    let found = cadence(&load(&records)).unwrap();
    assert_eq!(found.units_per_sdu, 4);
    let share = 2032.0 / 2033.0;
    assert!(close(found.units_per_sdu_share, share, TIGHT), "{found:?}");
    assert_eq!(found.interval_min_ns, 20 * NS_PER_MS);
    assert_eq!(found.interval_median_ns, 30 * NS_PER_MS);
    assert_eq!(found.interval_max_ns, 40 * NS_PER_MS);
}

#[test]
fn bitrate_is_the_bits_after_the_first_sdu_over_the_span() {
    let found = bitrate(&load(&steady(64_000, 61))).unwrap();
    let sdus_per_second = 1000.0 / 30.0;
    let audio_bits = sdus_per_second * 4.0 * 2.0 * 8.0;
    let sdu_bits = sdus_per_second * 50.0 * 8.0;
    assert!(
        close(found.audio_bits_per_second, audio_bits, audio_bits * TIGHT),
        "{found:?}"
    );
    assert!(
        close(found.sdu_bits_per_second, sdu_bits, sdu_bits * TIGHT),
        "{found:?}"
    );
    assert_eq!(found.unit_bytes, Some((2, 2)));
}

#[test]
fn bitrate_names_the_shortest_and_the_longest_unit() {
    let mut records = steady(64_000, 61);
    let long = [0x55_u8; 78];
    let mid = [0x55_u8; 68];
    records[0] = audio(records[0].at_ns, &OPAQUE, &[(0, &long)]);
    records[7] = audio(records[7].at_ns, &OPAQUE, &[(0, &long), (480, &mid)]);
    let found = bitrate(&load(&records)).unwrap();
    assert_eq!(found.unit_bytes, Some((2, 78)));
    let span_s = 60.96;
    let audio_bits = f64::from(2031 * 4 * 2 + 78 + 68) * 8.0 / span_s;
    assert!(
        close(found.audio_bits_per_second, audio_bits, audio_bits * TIGHT),
        "{found:?}"
    );

    let empty: Vec<_> = steady(64_000, 61)
        .iter()
        .map(|record| audio(record.at_ns, &OPAQUE, &[]))
        .collect();
    let found = bitrate(&load(&empty)).unwrap();
    assert_eq!(found.unit_bytes, None);
    assert!(close(found.audio_bits_per_second, 0.0, TIGHT), "{found:?}");
}
