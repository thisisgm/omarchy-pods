//! The number helpers at their edges: nothing, one sample, all equal, a counter that wraps.

use super::super::support::close;
use super::*;

/// Two to the power 53, the last integer below which every integer is an `f64`.
const TWO_POW_53: u64 = 1 << 53;
/// How close a computed figure must be to the one worked out by hand.
const TIGHT: f64 = 1e-9;

fn same_bits(value: f64, expected: f64) -> bool {
    value.to_bits() == expected.to_bits()
}

#[test]
fn to_f64_is_exact_below_two_to_the_53_and_rounds_once_above() {
    for (value, expected) in [
        (0, 0.0),
        (1, 1.0),
        (u64::from(u32::MAX), 4_294_967_295.0),
        (1 << 32, 4_294_967_296.0),
        ((1 << 32) + 1, 4_294_967_297.0),
        (TWO_POW_53 - 1, 9_007_199_254_740_991.0),
        (TWO_POW_53, 9_007_199_254_740_992.0),
        (TWO_POW_53 + 1, 9_007_199_254_740_992.0),
        (TWO_POW_53 + 3, 9_007_199_254_740_996.0),
        (u64::MAX, 18_446_744_073_709_551_616.0),
    ] {
        assert!(same_bits(to_f64(value), expected), "{value}");
    }
    assert!(same_bits(count(0), 0.0));
    assert!(same_bits(count(500), 500.0));
}

#[test]
fn seconds_and_millis_scale_nanoseconds() {
    assert!(same_bits(seconds(0), 0.0));
    assert!(same_bits(seconds(60_000_000_000), 60.0));
    assert!(same_bits(seconds(1_500_000_000), 1.5));
    assert!(same_bits(millis(0), 0.0));
    assert!(same_bits(millis(7_500_000), 7.5));
    assert!(same_bits(millis(90_000_001), 90.000_001));
}

#[test]
fn per_second_refuses_a_zero_span() {
    assert_eq!(per_second(10.0, 0), None);
    assert!(same_bits(per_second(0.0, 1_000_000_000).unwrap(), 0.0));
    assert!(same_bits(per_second(133.0, 1_000_000_000).unwrap(), 133.0));
    assert!(same_bits(per_second(100.0, 4_000_000_000).unwrap(), 25.0));
}

#[test]
fn ppm_is_signed_and_zero_at_the_reference() {
    assert!(same_bits(ppm(64_000.0, 64_000.0), 0.0));
    assert!(close(ppm(64_064.0, 64_000.0), 1000.0, TIGHT));
    assert!(close(ppm(63_936.0, 64_000.0), -1000.0, TIGHT));
    assert!(close(ppm(64_000.0, 48_000.0), 333_333.333_333_333, 1e-6));
}

#[test]
fn share_of_nothing_is_no_number() {
    assert_eq!(share(0, 0), None);
    assert!(same_bits(share(0, 4).unwrap(), 0.0));
    assert!(same_bits(share(1, 4).unwrap(), 0.25));
    assert!(same_bits(share(4, 4).unwrap(), 1.0));
}

#[test]
fn mode_of_nothing_one_all_equal_and_a_tie() {
    assert_eq!(mode(std::iter::empty::<u32>()), None);
    assert_eq!(mode([7_u32].into_iter()), Some((7, 1)));
    assert_eq!(mode([5_u32, 5, 5].into_iter()), Some((5, 3)));
    assert_eq!(mode([9_u32, 2, 2, 9].into_iter()), Some((2, 2)));
    assert_eq!(mode([1_u32, 4, 4, 1, 4].into_iter()), Some((4, 3)));
}

#[test]
fn wrapping_deltas_follow_a_counter_through_its_wrap() {
    assert_eq!(wrapping_deltas(&[]), Vec::<u32>::new());
    assert_eq!(wrapping_deltas(&[9]), Vec::<u32>::new());
    assert_eq!(wrapping_deltas(&[3, 3, 3]), [0, 0]);
    let across = [u32::MAX - 959, u32::MAX - 479, 0, 480];
    assert_eq!(wrapping_deltas(&across), [480, 480, 480]);
    assert_eq!(mode(wrapping_deltas(&across).into_iter()), Some((480, 3)));
    assert_eq!(wrapping_deltas(&[480, 0]), [u32::MAX - 479]);
}

#[test]
fn median_is_the_lower_middle_of_a_sorted_list() {
    assert_eq!(median::<u64>(&[]), None);
    assert_eq!(median(&[5]), Some(5));
    assert_eq!(median(&[1, 3]), Some(1));
    assert_eq!(median(&[1, 2, 9]), Some(2));
    assert_eq!(median(&[4, 4, 4, 4]), Some(4));
}

/// `points` points on the line y = 3x + 1.
fn on_a_line(points: u32) -> Vec<(f64, f64)> {
    (0..points)
        .map(|k| (f64::from(k), 3.0 * f64::from(k) + 1.0))
        .collect()
}

#[test]
fn least_squares_needs_500_points_and_a_spread() {
    assert_eq!(least_squares(&[]), None);
    assert_eq!(least_squares(&on_a_line(3)), None);
    assert_eq!(least_squares(&on_a_line(499)), None);
    assert!(least_squares(&on_a_line(500)).is_some());
    let one_x: Vec<(f64, f64)> = (0..500_u32).map(|k| (1.0, f64::from(k))).collect();
    assert_eq!(least_squares(&one_x), None);
    let flat: Vec<(f64, f64)> = (0..500_u32).map(|k| (f64::from(k), 7.0)).collect();
    let fit = least_squares(&flat).unwrap();
    assert!(same_bits(fit.slope, 0.0));
    assert!(same_bits(fit.half_width, 0.0));
}

#[test]
fn least_squares_recovers_an_exact_line() {
    let fit = least_squares(&on_a_line(500)).unwrap();
    assert!(close(fit.slope, 3.0, TIGHT), "{fit:?}");
    assert!(close(fit.half_width, 0.0, TIGHT), "{fit:?}");
}

// Four points worked by hand, each 125 times: Sxx 625, Sxy 562.5, residual sum of squares 87.5, standard error sqrt(87.5 / 498 / 625), times 1.96.
#[test]
fn least_squares_interval_is_the_standard_error_times_1_96() {
    let four = [(0.0, 0.0), (1.0, 1.0), (2.0, 1.0), (3.0, 3.0)];
    let points: Vec<(f64, f64)> = four.iter().copied().cycle().take(500).collect();
    let fit = least_squares(&points).unwrap();
    assert!(close(fit.slope, 0.9, TIGHT), "{fit:?}");
    assert!(close(fit.half_width, 0.032_862_864_6, 1e-9), "{fit:?}");
}

#[test]
fn least_squares_keeps_its_precision_far_from_the_origin() {
    let points: Vec<(f64, f64)> = (0..1000_u32)
        .map(|k| (5.0e6 + f64::from(k) * 0.03, 7.0e8 + f64::from(k) * 4.0))
        .collect();
    let fit = least_squares(&points).unwrap();
    assert!(close(fit.slope, 4.0 / 0.03, 1e-6), "{fit:?}");
}
