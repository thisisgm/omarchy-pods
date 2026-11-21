//! The band limit each unit declares, and the spectral line and frequency it stands for.

use super::super::load::load;
use super::super::support::{OPAQUE, audio, close};
use super::*;

#[test]
fn max_sfb_is_read_from_bits_8_to_13() {
    assert_eq!(max_sfb(&[0x00, 0x7c]), Some(31));
    assert_eq!(max_sfb(&[0x00, 0x00]), Some(0));
    assert_eq!(max_sfb(&[0xff, 0x7c, 0xff]), Some(31));
    assert_eq!(max_sfb(&[0x00, 0x03]), Some(0));
    assert_eq!(max_sfb(&[0x00, 0x04]), Some(1));
    assert_eq!(max_sfb(&[0x00, 0x8c]), Some(35));
    assert_eq!(max_sfb(&[0x00, 0xff]), Some(63));
    assert_eq!(max_sfb(&[0x7c]), None);
    assert_eq!(max_sfb(&[]), None);
}

#[test]
fn band_table_matches_the_three_check_points() {
    assert_eq!(band_line(28), Some(240));
    assert_eq!(band_line(31), Some(336));
    assert_eq!(band_line(35), Some(480));
    assert_eq!(band_line(0), Some(0));
    assert_eq!(band_line(36), None);
    assert_eq!(band_line(63), None);
    let lines: Vec<u16> = (0..=63).filter_map(band_line).collect();
    assert_eq!(lines.len(), 36);
    assert!(lines.is_sorted_by(|low, high| low < high));
    assert_eq!(lines.iter().map(|&line| u32::from(line)).sum::<u32>(), 4884);
}

#[test]
fn band_hz_is_the_line_over_the_table_length_times_half_the_clock() {
    assert!(close(band_hz(336, 64_000.0), 22_400.0, 1e-9));
    assert!(close(band_hz(480, 48_000.0), 24_000.0, 1e-9));
    assert!(close(band_hz(240, 64_010.0), 16_002.5, 1e-9));
    assert!(close(band_hz(0, 64_000.0), 0.0, 1e-9));
}

#[test]
fn bandwidth_counts_each_band_and_sets_the_invalid_apart() {
    let wide: &[u8] = &[0x00, 0x7c, 0x55];
    let narrow: &[u8] = &[0x00, 0x00];
    let top: &[u8] = &[0x00, 0x8c];
    let invalid: &[u8] = &[0x00, 0x90];
    let short: &[u8] = &[0x00];
    let records = [
        audio(1, &OPAQUE, &[(0, wide), (480, narrow), (960, wide)]),
        audio(
            2,
            &OPAQUE,
            &[(1440, invalid), (1920, short), (2400, wide), (2880, &[])],
        ),
        audio(3, &OPAQUE, &[(3360, narrow)]),
    ];
    let report = bandwidth(&load(&records));
    let mut expected = [0_u32; 36];
    expected[0] = 2;
    expected[31] = 3;
    assert_eq!(report.histogram, expected);
    assert_eq!((report.invalid, report.unread), (1, 2));
    assert_eq!(report.max(), Some(31));

    let with_top = [audio(1, &OPAQUE, &[(0, wide), (480, top), (960, invalid)])];
    let report = bandwidth(&load(&with_top));
    assert_eq!(report.histogram[35], 1);
    assert_eq!(report.invalid, 1);
    assert_eq!(report.max(), Some(35));
}

#[test]
fn the_maximum_is_the_highest_limit_seen_however_rare() {
    let wide: &[u8] = &[0x00, 0x7c];
    let narrow: &[u8] = &[0x00, 0x00];
    let units = [(0, narrow), (480, narrow), (960, wide), (1440, narrow)];
    let report = bandwidth(&load(&[audio(1, &OPAQUE, &units)]));
    assert_eq!((report.histogram[0], report.histogram[31]), (3, 1));
    assert_eq!(report.max(), Some(31));
}

#[test]
fn no_unit_has_no_maximum() {
    let report = bandwidth(&load(&[]));
    assert_eq!(report.histogram, [0_u32; 36]);
    assert_eq!((report.invalid, report.unread), (0, 0));
    assert_eq!(report.max(), None);
    let only_invalid = [audio(1, &OPAQUE, &[(0, &[0x00, 0xff])])];
    assert_eq!(bandwidth(&load(&only_invalid)).max(), None);
}
