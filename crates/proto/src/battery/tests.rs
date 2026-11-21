//! The `tst_battery` cases, translated, plus the range and order rules.

use super::*;

const DISCHARGING: u8 = 0x02;

// Sample input: a 16 byte decrypted payload, byte 1 left, byte 2 right, byte 3 case, high bit charging.
fn payload(left: u8, right: u8, case: u8) -> [u8; 16] {
    let mut p = [0; 16];
    p[1] = left;
    p[2] = right;
    p[3] = case;
    p
}

// The control channel battery packet for a headset, which is what establishes a real level.
fn headset_packet(level: u8) -> Vec<u8> {
    let mut buf = HEADER.to_vec();
    buf.extend([1, 0x01, 0x01, level, DISCHARGING, 0x01]);
    buf
}

fn apply_decrypted(
    b: &mut Batteries,
    p: [u8; 16],
    primary_left: bool,
    pod_in_case: bool,
    headset: bool,
) {
    b.apply(&BatteryReport::parse_decrypted(&p, primary_left, pod_in_case, headset).unwrap());
}

#[test]
fn parse_packet_rejects_empty() {
    assert!(BatteryReport::parse(&[]).is_err());
}

#[test]
fn parse_packet_rejects_short_below_header() {
    assert!(BatteryReport::parse(&HEADER[..4]).is_err());
}

#[test]
fn parse_packet_rejects_header_only() {
    assert_eq!(HEADER.len(), 6);
    assert_eq!(
        BatteryReport::parse(&HEADER),
        Err(ParseError::TooShort { need: 7, got: 6 })
    );
}

#[test]
fn parse_packet_rejects_battery_count_too_high() {
    let mut buf = HEADER.to_vec();
    buf.push(4);
    assert_eq!(
        BatteryReport::parse(&buf),
        Err(ParseError::UnknownValue {
            field: "battery count",
            value: 4
        })
    );
}

#[test]
fn parse_packet_rejects_count_size_mismatch() {
    let mut buf = HEADER.to_vec();
    buf.push(1);
    buf.extend([0; 3]);
    assert_eq!(
        BatteryReport::parse(&buf),
        Err(ParseError::WrongLength {
            expected: 12,
            got: 10
        })
    );
}

#[test]
fn parse_packet_accepts_single_headset() {
    let mut buf = HEADER.to_vec();
    buf.extend([1, 0x01, 0x01, 85, DISCHARGING, 0x01]);
    let report = BatteryReport::parse(&buf).unwrap();
    let mut b = Batteries::default();
    b.apply(&report);
    assert_eq!(b.headset.level, Some(85));
    assert!(b.headset.available);
}

#[test]
fn parse_packet_rejects_broken_spacer() {
    let mut buf = HEADER.to_vec();
    buf.extend([1, 0x04, 0x42, 50, DISCHARGING, 0x01]);
    assert!(BatteryReport::parse(&buf).is_err());
}

#[test]
fn parse_encrypted_packet_rejects_wrong_size() {
    for size in [8, 15, 17] {
        assert!(
            BatteryReport::parse_decrypted(&vec![0; size], true, false, false).is_err(),
            "size {size}"
        );
    }
}

#[test]
fn reset_clears_state() {
    let mut b = Batteries::default();
    b.apply(&BatteryReport::parse(&headset_packet(75)).unwrap());
    assert_eq!(b.headset.level, Some(75));
    b = Batteries::default();
    assert_eq!(b.headset.level, None);
    assert!(!b.headset.available);
}

#[test]
fn encrypted_case_zero_is_unknown_while_nothing_is_docked_to_read_it() {
    let mut b = Batteries::default();
    apply_decrypted(&mut b, payload(80, 80, 0), true, false, false);
    assert!(!b.case.available);
}

#[test]
fn encrypted_case_zero_is_trusted_when_a_pod_is_docked() {
    let mut b = Batteries::default();
    apply_decrypted(&mut b, payload(80, 80, 0), true, true, false);
    assert!(b.case.available);
    assert_eq!(b.case.level, Some(0));
}

#[test]
fn encrypted_case_level_survives_the_pods_leaving_the_case() {
    let mut b = Batteries::default();
    apply_decrypted(&mut b, payload(80, 80, 80), true, true, false);
    assert_eq!(b.case.level, Some(80));
    apply_decrypted(&mut b, payload(80, 80, 0), true, false, false);
    assert!(b.case.available);
    assert_eq!(b.case.level, Some(80));
}

// Observed on an AirPods Max (USB-C, A3184): byte 1 held 100 while the primary went left then right.
#[test]
fn encrypted_headset_level_survives_the_primary_flip() {
    let mut b = Batteries::default();
    apply_decrypted(&mut b, payload(100, 0, 0), true, false, true);
    assert_eq!(b.headset.level, Some(100));
    apply_decrypted(&mut b, payload(100, 0, 0), false, false, true);
    assert!(b.headset.available);
    assert_eq!(b.headset.level, Some(100));
}

// The unused slot reads 0, so a real 0 must stay distinguishable from unknown.
#[test]
fn encrypted_headset_zero_is_a_real_reading() {
    let mut b = Batteries::default();
    apply_decrypted(&mut b, payload(0, 0, 0), true, false, true);
    assert!(b.headset.available);
    assert_eq!(b.headset.level, Some(0));
}

#[test]
fn encrypted_headset_unknown_leaves_the_headset_unavailable() {
    let mut b = Batteries::default();
    apply_decrypted(&mut b, payload(0x7F, 0, 0), true, false, true);
    assert!(!b.headset.available);
}

#[test]
fn encrypted_headset_unknown_leaves_the_known_level_alone() {
    let mut b = Batteries::default();
    b.apply(&BatteryReport::parse(&headset_packet(100)).unwrap());
    assert_eq!(b.headset.level, Some(100));
    // 0x7F means unknown, and slot 2's 50 is what the old first non-unknown scan would have adopted.
    apply_decrypted(&mut b, payload(0x7F, 50, 0), true, false, true);
    assert_eq!(b.headset.level, Some(100));
}

#[test]
fn encrypted_headset_charging_comes_from_the_high_bit() {
    let mut b = Batteries::default();
    apply_decrypted(&mut b, payload(90, 0, 0), true, false, true);
    assert_eq!(b.headset.level, Some(90));
    assert!(!b.headset.charging);
    // 0x5A is 90 percent.
    let charging_90 = CHARGING_BIT | 0x5A;
    apply_decrypted(&mut b, payload(charging_90, 0, 0), true, false, true);
    assert_eq!(b.headset.level, Some(90));
    assert!(b.headset.charging);
}

#[test]
fn control_channel_level_above_100_is_unknown() {
    let mut buf = HEADER.to_vec();
    buf.extend([1, 0x04, 0x01, 101, DISCHARGING, 0x01]);
    let report = BatteryReport::parse(&buf).unwrap();
    assert_eq!(report.left.map(|c| c.level), Some(None));
}

#[test]
fn encrypted_pod_levels_above_100_keep_the_last_reading() {
    let mut b = Batteries::default();
    apply_decrypted(&mut b, payload(60, 70, 0), true, false, false);
    apply_decrypted(&mut b, payload(0x7F, 101, 0), true, false, false);
    assert_eq!(b.left.level, Some(60));
    assert_eq!(b.right.level, Some(70));
}

#[test]
fn control_channel_order_sets_primary_and_secondary() {
    let mut buf = HEADER.to_vec();
    buf.extend([
        2,
        0x02,
        0x01,
        40,
        DISCHARGING,
        0x01,
        0x04,
        0x01,
        50,
        0x01,
        0x01,
    ]);
    let report = BatteryReport::parse(&buf).unwrap();
    assert_eq!(report.primary, Some(Component::Right));
    assert_eq!(report.secondary, Some(Component::Left));
    assert_eq!(
        report.left,
        Some(Charge {
            level: Some(50),
            charging: true,
            available: true
        })
    );
}

#[test]
fn control_channel_disconnected_entry_keeps_the_last_reading() {
    let mut buf = HEADER.to_vec();
    buf.extend([1, 0x08, 0x01, 0, 0x04, 0x01]);
    assert_eq!(BatteryReport::parse(&buf).unwrap().case, None);
}
