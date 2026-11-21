//! Counts of the packets that are not audio, the control replies, the battery and the start trials.

use pods_proto::battery::Charge;
use pods_proto::uplink::{START, STOP};

use super::super::input::MarkKind;
use super::super::load::load;
use super::super::support::{
    NS_PER_MS, OPAQUE, SILENT_UNIT, audio, close, from_pods, mark, to_pods,
};
use super::*;

/// The reply the pods are reported to send after START.
const CONTROL_REPLY: [u8; 13] = [
    0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x00, 0x00, 0x03, 0x00, 0x01, 0x01, 0x01,
];
/// An ear detection packet.
const EAR: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x00];

/// A packet of `len` bytes whose opcode bytes read `opcode`.
fn packet(opcode: u16, len: usize) -> Vec<u8> {
    let [low, high] = opcode.to_le_bytes();
    let mut sdu = vec![0x04, 0x00, 0x04, 0x00, low, high];
    sdu.resize(len, 0x00);
    sdu
}

/// A battery packet for the two buds: kind, 01, level, status, 01 for each.
fn battery_packet(left: u8, right: u8) -> Vec<u8> {
    let mut sdu = vec![0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x02];
    sdu.extend_from_slice(&[0x04, 0x01, left, 0x02, 0x01]);
    sdu.extend_from_slice(&[0x02, 0x01, right, 0x01, 0x01]);
    sdu
}

fn charge(level: u8, charging: bool) -> Charge {
    Charge { level, charging }
}

#[test]
fn census_counts_the_other_packets_by_opcode() {
    let records = [
        from_pods(1, &packet(0x0027, 40)),
        from_pods(2, &EAR),
        to_pods(3, &packet(0x000f, 11)),
        from_pods(4, &EAR),
        from_pods(5, &CONTROL_REPLY),
        audio(6, &OPAQUE, &[(0, &SILENT_UNIT)]),
        from_pods(7, &packet(0x1234, 6)),
        from_pods(8, &[0x01, 0x00, 0x04, 0x00, 0x00]),
        from_pods(9, &EAR),
        from_pods(10, &[0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x09]),
    ];
    let loaded = load(&records);
    assert_eq!(census(&loaded), [(0x0006, 3), (0x0027, 1), (0x1234, 1)]);
    assert_eq!(without_opcode(&loaded), 1);
    assert_eq!(census(&load(&[])), []);
}

#[test]
fn largest_packet_names_its_opcode() {
    let records = [
        audio(1, &OPAQUE, &[(0, &[0x11; 200]), (480, &[0x22; 200])]),
        from_pods(2, &packet(0x0027, 1013)),
        to_pods(3, &packet(0x0099, 4000)),
        from_pods(4, &packet(0x0031, 1013)),
        from_pods(5, &EAR),
    ];
    assert_eq!(largest_packet(&load(&records)), Some((0x0027, 1013)));
    let audio_only = [audio(1, &OPAQUE, &[(0, &SILENT_UNIT)]), from_pods(2, &EAR)];
    assert_eq!(largest_packet(&load(&audio_only)), Some((0x0058, 29)));
    assert_eq!(largest_packet(&load(&[])), None);
    assert_eq!(largest_packet(&load(&[to_pods(1, &EAR)])), None);
    let short_only = [from_pods(1, &[0x01, 0x00, 0x04]), from_pods(2, &[0x01])];
    assert_eq!(largest_packet(&load(&short_only)), None);
}

#[test]
fn battery_is_the_first_and_the_last_report() {
    assert_eq!(battery(&load(&[from_pods(1, &EAR)])), None);
    let one = [from_pods(1, &battery_packet(85, 90))];
    let (first, last) = battery(&load(&one)).unwrap();
    assert_eq!(first, last);
    assert_eq!(
        (first.batteries.left, first.batteries.right),
        (Some(charge(85, false)), Some(charge(90, true)))
    );

    let three = [
        from_pods(1, &battery_packet(85, 90)),
        from_pods(2, &battery_packet(84, 91)),
        from_pods(3, &EAR),
        from_pods(4, &battery_packet(80, 95)),
    ];
    let (first, last) = battery(&load(&three)).unwrap();
    assert_eq!(
        (first.batteries.left, first.batteries.right),
        (Some(charge(85, false)), Some(charge(90, true)))
    );
    assert_eq!(
        (last.batteries.left, last.batteries.right),
        (Some(charge(80, false)), Some(charge(95, true)))
    );
}

#[test]
fn trial_times_are_measured_from_the_start_mark() {
    let records = [
        from_pods(900, &CONTROL_REPLY),
        audio(950, &OPAQUE, &[(0, &SILENT_UNIT)]),
        to_pods(1000, &START),
        from_pods(1000, &EAR),
        from_pods(1012, &CONTROL_REPLY),
        from_pods(1020, &CONTROL_REPLY),
        audio(1085, &OPAQUE, &[(0, &SILENT_UNIT)]),
        audio(1115, &OPAQUE, &[(480, &SILENT_UNIT)]),
        audio(2000, &OPAQUE, &[(960, &SILENT_UNIT)]),
        audio(3000, &OPAQUE, &[(0, &SILENT_UNIT)]),
    ];
    let marks = [
        mark(0, MarkKind::Other),
        mark(1000, MarkKind::Start),
        mark(1005, MarkKind::Other),
        mark(1050, MarkKind::Truncated),
        mark(2000, MarkKind::Stop),
        mark(3000, MarkKind::Start),
    ];
    let expected = [
        Trial {
            start_ns: 1000,
            control_after_ns: Some(12),
            audio_after_ns: Some(85),
        },
        Trial {
            start_ns: 3000,
            control_after_ns: None,
            audio_after_ns: Some(0),
        },
    ];
    assert_eq!(trials(&load(&records), &marks), expected);
    assert_eq!(trials(&load(&records), &[]), []);
}

#[test]
fn a_trial_with_no_audio_is_none() {
    let records = [
        to_pods(1000, &START),
        from_pods(1100, &EAR),
        to_pods(2000, &STOP),
        from_pods(2000, &CONTROL_REPLY),
        audio(2001, &OPAQUE, &[(0, &SILENT_UNIT)]),
        from_pods(3005, &CONTROL_REPLY),
    ];
    let marks = [
        mark(1000, MarkKind::Start),
        mark(2000, MarkKind::Stop),
        mark(3000, MarkKind::Start),
        mark(4000, MarkKind::Stop),
    ];
    let expected = [
        Trial {
            start_ns: 1000,
            control_after_ns: None,
            audio_after_ns: None,
        },
        Trial {
            start_ns: 3000,
            control_after_ns: Some(5),
            audio_after_ns: None,
        },
    ];
    assert_eq!(trials(&load(&records), &marks), expected);
}

#[test]
fn a_second_start_with_no_stop_ends_the_trial_before_it() {
    let records = [
        from_pods(3005, &CONTROL_REPLY),
        audio(3085, &OPAQUE, &[(0, &SILENT_UNIT)]),
    ];
    let marks = [mark(1000, MarkKind::Start), mark(3000, MarkKind::Start)];
    let expected = [
        Trial {
            start_ns: 1000,
            control_after_ns: None,
            audio_after_ns: None,
        },
        Trial {
            start_ns: 3000,
            control_after_ns: Some(5),
            audio_after_ns: Some(85),
        },
    ];
    assert_eq!(trials(&load(&records), &marks), expected);
}

#[test]
fn control_replies_are_listed_with_offsets_and_length_check() {
    let cut_reply = &CONTROL_REPLY[..12];
    let records = [
        from_pods(988 * NS_PER_MS, &CONTROL_REPLY),
        to_pods(1000 * NS_PER_MS, &START),
        from_pods(1012 * NS_PER_MS, &CONTROL_REPLY),
        from_pods(1400 * NS_PER_MS, cut_reply),
        from_pods(1600 * NS_PER_MS, &CONTROL_REPLY),
        from_pods(2003 * NS_PER_MS, &CONTROL_REPLY),
    ];
    let marks = [
        mark(0, MarkKind::Other),
        mark(1000 * NS_PER_MS, MarkKind::Start),
        mark(1010 * NS_PER_MS, MarkKind::Other),
        mark(2000 * NS_PER_MS, MarkKind::Stop),
    ];
    let listed = control_replies(&load(&records), &marks);
    let seen: Vec<_> = listed
        .iter()
        .map(|reply| (reply.bytes.as_slice(), reply.length_matches))
        .collect();
    assert_eq!(
        seen,
        [
            (&CONTROL_REPLY[..], true),
            (&CONTROL_REPLY[..], true),
            (cut_reply, false),
            (&CONTROL_REPLY[..], true),
            (&CONTROL_REPLY[..], true),
        ]
    );
    let expected = [
        (MarkKind::Start, 1, -12.0),
        (MarkKind::Start, 1, 12.0),
        (MarkKind::Start, 1, 400.0),
        (MarkKind::Stop, 1, -400.0),
        (MarkKind::Stop, 1, 3.0),
    ];
    for (reply, (kind, ordinal, offset_ms)) in listed.iter().zip(expected) {
        let nearest = reply.nearest.unwrap();
        assert_eq!(
            (nearest.kind, nearest.ordinal),
            (kind, ordinal),
            "{reply:?}"
        );
        assert!(close(nearest.offset_ms, offset_ms, 1e-9), "{reply:?}");
    }

    let unmarked = control_replies(&load(&records), &[mark(5, MarkKind::Other)]);
    assert_eq!(unmarked.len(), 5);
    assert!(unmarked.iter().all(|reply| reply.nearest.is_none()));
}

#[test]
fn a_packet_midway_between_two_marks_takes_the_earlier() {
    let records = [from_pods(1500 * NS_PER_MS, &CONTROL_REPLY)];
    let marks = [
        mark(1000 * NS_PER_MS, MarkKind::Start),
        mark(2000 * NS_PER_MS, MarkKind::Stop),
    ];
    let listed = control_replies(&load(&records), &marks);
    let nearest = listed[0].nearest.unwrap();
    assert_eq!((nearest.kind, nearest.ordinal), (MarkKind::Start, 1));
    assert!(close(nearest.offset_ms, 500.0, 1e-9));
}

#[test]
fn the_nearest_mark_is_counted_within_its_kind() {
    let records = [from_pods(5400 * NS_PER_MS, &CONTROL_REPLY)];
    let marks = [
        mark(1000 * NS_PER_MS, MarkKind::Start),
        mark(2000 * NS_PER_MS, MarkKind::Stop),
        mark(3000 * NS_PER_MS, MarkKind::Start),
        mark(4000 * NS_PER_MS, MarkKind::Stop),
        mark(5000 * NS_PER_MS, MarkKind::Start),
        mark(6000 * NS_PER_MS, MarkKind::Stop),
    ];
    let listed = control_replies(&load(&records), &marks);
    let nearest = listed[0].nearest.unwrap();
    assert_eq!((nearest.kind, nearest.ordinal), (MarkKind::Start, 3));
    assert!(close(nearest.offset_ms, 400.0, 1e-9));
}
