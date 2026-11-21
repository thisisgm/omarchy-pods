//! The `tst_battery` cases, translated, plus the range and order rules.

use super::*;
use cpp_source::{BATTERY, PACKETS};

const DISCHARGING: u8 = 0x02;

// Decrypted payload: byte 1 primary, byte 2 secondary, byte 3 case, high bit charging.
fn payload(primary: u8, secondary: u8, case: u8) -> [u8; 16] {
    let mut p = [0; 16];
    p[1] = primary;
    p[2] = secondary;
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
        Err(ParseError::TooShort {
            parser: Parser::Battery,
            need: 7,
            got: 6
        })
    );
}

#[test]
fn parse_packet_rejects_battery_count_too_high() {
    let mut buf = HEADER.to_vec();
    buf.push(4);
    assert_eq!(
        BatteryReport::parse(&buf),
        Err(ParseError::UnknownValue {
            parser: Parser::Battery,
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
            parser: Parser::Battery,
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
    assert_eq!(b.headset.map(|c| c.level), Some(85));
}

#[test]
fn parse_packet_rejects_broken_spacer() {
    let mut buf = HEADER.to_vec();
    buf.extend([1, 0x04, 0x42, 50, DISCHARGING, 0x01]);
    assert_eq!(
        BatteryReport::parse(&buf),
        Err(ParseError::UnknownValue {
            parser: Parser::Battery,
            field: "battery spacer",
            value: 0x42
        })
    );
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
    assert_eq!(b.headset.map(|c| c.level), Some(75));
    b = Batteries::default();
    assert_eq!(b.headset, None);
}

#[test]
fn encrypted_case_zero_is_unknown_while_nothing_is_docked_to_read_it() {
    let mut b = Batteries::default();
    apply_decrypted(&mut b, payload(80, 80, 0), true, false, false);
    assert!(b.case.is_none());
}

#[test]
fn encrypted_case_zero_is_trusted_when_a_pod_is_docked() {
    let mut b = Batteries::default();
    apply_decrypted(&mut b, payload(80, 80, 0), true, true, false);
    assert_eq!(b.case.map(|c| c.level), Some(0));
}

#[test]
fn encrypted_case_level_survives_the_pods_leaving_the_case() {
    let mut b = Batteries::default();
    apply_decrypted(&mut b, payload(80, 80, 80), true, true, false);
    assert_eq!(b.case.map(|c| c.level), Some(80));
    apply_decrypted(&mut b, payload(80, 80, 0), true, false, false);
    assert_eq!(b.case.map(|c| c.level), Some(80));
}

// Seen on an AirPods Max (USB-C, A3184): byte 1 held 100 while the primary went left then right.
#[test]
fn encrypted_headset_level_survives_the_primary_flip() {
    let mut b = Batteries::default();
    apply_decrypted(&mut b, payload(100, 0, 0), true, false, true);
    assert_eq!(b.headset.map(|c| c.level), Some(100));
    apply_decrypted(&mut b, payload(100, 0, 0), false, false, true);
    assert_eq!(b.headset.map(|c| c.level), Some(100));
}

// The unused slot reads 0, so a real 0 must stay distinguishable from unknown.
#[test]
fn encrypted_headset_zero_is_a_real_reading() {
    let mut b = Batteries::default();
    apply_decrypted(&mut b, payload(0, 0, 0), true, false, true);
    assert_eq!(b.headset.map(|c| c.level), Some(0));
}

#[test]
fn encrypted_headset_unknown_leaves_the_headset_unavailable() {
    let mut b = Batteries::default();
    apply_decrypted(&mut b, payload(0x7F, 0, 0), true, false, true);
    assert!(b.headset.is_none());
}

#[test]
fn encrypted_headset_unknown_leaves_the_known_level_alone() {
    let mut b = Batteries::default();
    b.apply(&BatteryReport::parse(&headset_packet(100)).unwrap());
    assert_eq!(b.headset.map(|c| c.level), Some(100));
    // 0x7F means unknown; slot 2's 50 is what the old first non-unknown scan would have adopted.
    apply_decrypted(&mut b, payload(0x7F, 50, 0), true, false, true);
    assert_eq!(b.headset.map(|c| c.level), Some(100));
}

#[test]
fn encrypted_headset_charging_comes_from_the_high_bit() {
    let mut b = Batteries::default();
    apply_decrypted(&mut b, payload(90, 0, 0), true, false, true);
    assert_eq!(b.headset.map(|c| c.level), Some(90));
    assert!(!b.headset.unwrap().charging);
    // 0x5A is 90 percent.
    let charging_90 = CHARGING_BIT | 0x5A;
    apply_decrypted(&mut b, payload(charging_90, 0, 0), true, false, true);
    assert_eq!(b.headset.map(|c| c.level), Some(90));
    assert!(b.headset.unwrap().charging);
}

#[test]
fn control_channel_level_above_100_keeps_the_last_reading() {
    let mut buf = HEADER.to_vec();
    buf.extend([1, 0x04, 0x01, 101, DISCHARGING, 0x01]);
    let report = BatteryReport::parse(&buf).unwrap();
    assert_eq!(report.batteries.left, None);
    let mut fresh = Batteries::default();
    fresh.apply(&report);
    assert!(fresh.left.is_none());
    let mut known = Batteries::default();
    apply_decrypted(&mut known, payload(60, 70, 0), true, false, false);
    known.apply(&report);
    assert_eq!(known.left.map(|c| c.level), Some(60));
}

#[test]
fn encrypted_pod_levels_above_100_keep_the_last_reading() {
    let mut b = Batteries::default();
    apply_decrypted(&mut b, payload(60, 70, 0), true, false, false);
    apply_decrypted(&mut b, payload(0x7F, 101, 0), true, false, false);
    assert_eq!(b.left.map(|c| c.level), Some(60));
    assert_eq!(b.right.map(|c| c.level), Some(70));
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
        report.batteries.left,
        Some(Charge {
            level: 50,
            charging: true
        })
    );
}

#[test]
fn control_channel_disconnected_entry_keeps_the_last_reading() {
    let mut buf = HEADER.to_vec();
    buf.extend([1, 0x08, 0x01, 0, 0x04, 0x01]);
    assert_eq!(BatteryReport::parse(&buf).unwrap().batteries.case, None);
}

#[test]
fn encrypted_right_primary_swaps_the_bud_bytes() {
    let report = BatteryReport::parse_decrypted(&payload(60, 70, 0), false, false, false).unwrap();
    assert_eq!(report.batteries.right.map(|c| c.level), Some(60));
    assert_eq!(report.batteries.left.map(|c| c.level), Some(70));
    assert_eq!(report.primary, Some(Component::Right));
    assert_eq!(report.secondary, Some(Component::Left));
}

#[test]
fn control_channel_unknown_part_is_skipped() {
    let mut buf = HEADER.to_vec();
    buf.extend([2, 0x10, 0x01, 50, DISCHARGING, 0x01]);
    buf.extend([0x04, 0x01, 60, DISCHARGING, 0x01]);
    let report = BatteryReport::parse(&buf).unwrap();
    assert_eq!(
        report.batteries.left,
        Some(Charge {
            level: 60,
            charging: false
        })
    );
    assert_eq!(report.primary, Some(Component::Left));
    assert_eq!(report.secondary, None);
    assert_eq!(
        (
            report.batteries.right,
            report.batteries.case,
            report.batteries.headset
        ),
        (None, None, None)
    );
}

#[test]
fn control_channel_case_entry_is_never_primary() {
    let mut buf = HEADER.to_vec();
    buf.extend([3, 0x08, 0x01, 90, 0x01, 0x01]);
    buf.extend([0x02, 0x01, 40, DISCHARGING, 0x01]);
    buf.extend([0x04, 0x01, 50, DISCHARGING, 0x01]);
    let report = BatteryReport::parse(&buf).unwrap();
    assert_eq!(report.primary, Some(Component::Right));
    assert_eq!(report.secondary, Some(Component::Left));
    assert_eq!(report.batteries.case.map(|c| c.level), Some(90));
}

#[test]
fn header_and_codes_match_the_cpp_battery_definitions() {
    assert_eq!(
        HEADER.to_vec(),
        PACKETS.hex(Some("namespace Parse"), "BATTERY_STATUS")
    );
    let component = Some("enum class Component");
    for (part, name) in [
        (Component::Headset, "Headset"),
        (Component::Right, "Right"),
        (Component::Left, "Left"),
        (Component::Case, "Case"),
    ] {
        let cpp = BATTERY.byte(component, &format!("{name} = "), ',');
        assert_eq!(Component::try_from(cpp), Ok(part), "{name}");
    }
    assert_eq!(
        Component::try_from(0x10),
        Err(ParseError::UnknownValue {
            parser: Parser::Battery,
            field: "battery component",
            value: 0x10
        })
    );
    let status = Some("enum class BatteryStatus");
    assert_eq!(STATUS_CHARGING, BATTERY.byte(status, "Charging = ", ','));
    assert_eq!(DISCHARGING, BATTERY.byte(status, "Discharging = ", ','));
    assert_eq!(
        STATUS_DISCONNECTED,
        BATTERY.byte(status, "Disconnected = ", ',')
    );
}
