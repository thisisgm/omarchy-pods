//! Sorting a log into the packets each analysis reads, and cutting it at the marks.

use pods_proto::battery::Charge;
use pods_proto::error::HEADER_LEN;
use pods_proto::uplink::{AUDIO_HEADER_LEN, START, STOP, UNIT_PREFIX_LEN};
use proptest::prelude::*;

use super::super::input::{Mark, MarkKind};
use super::super::support::{OPAQUE, SILENT_UNIT, audio, from_pods, mark, to_pods};
use super::*;

/// The reply the pods are reported to send after START.
const CONTROL_REPLY: [u8; 13] = [
    0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x00, 0x00, 0x03, 0x00, 0x01, 0x01, 0x01,
];
/// A battery packet with one entry: the left bud at 85 percent, charging.
const BATTERY: [u8; 12] = [
    0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x01, 0x04, 0x01, 0x55, 0x01, 0x01,
];
/// An ear detection packet, which stands for any other traffic.
const EAR: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x00];
/// Longer than any length bound a generated SDU has to cross.
const SDU_BOUND: usize = 96;

/// A device information packet holding `strings`, each closed by a NUL, after a made up binary head.
fn device_info(strings: &[&str]) -> Vec<u8> {
    let mut sdu = vec![
        0x04, 0x00, 0x04, 0x00, 0x1d, 0x00, 0x02, 0xed, 0x00, 0x04, 0x00,
    ];
    for string in strings {
        sdu.extend_from_slice(string.as_bytes());
        sdu.push(0x00);
    }
    sdu
}

#[test]
fn sorts_the_packets_by_what_reads_them() {
    let mut with_stray = audio(
        40,
        &OPAQUE,
        &[(480, &SILENT_UNIT), (960, &[0x00, 0x7c, 0x11])],
    );
    with_stray.sdu.extend_from_slice(&[0xee, 0xee, 0xee]);
    let records = [
        to_pods(5, &START),
        from_pods(10, &CONTROL_REPLY),
        audio(20, &OPAQUE, &[(0, &SILENT_UNIT)]),
        with_stray,
        from_pods(50, &BATTERY),
        from_pods(60, &device_info(&["Probe Pods", "A0000"])),
        from_pods(70, &[0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x02, 0x00, 0xaa]),
        from_pods(80, &[0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x01]),
        from_pods(90, &[0x01, 0x00, 0x04, 0x00]),
        from_pods(95, &EAR),
        to_pods(99, &STOP),
    ];
    let loaded = load(&records);

    assert_eq!(loaded.to_pods, 2);
    assert_eq!(loaded.seen.len(), 9);
    assert_eq!(loaded.unknown_subtype, 1);
    assert_eq!(loaded.cut_short, 1);
    assert_eq!(loaded.battery_unread, 0);

    assert_eq!(loaded.controls.len(), 1);
    assert_eq!(loaded.controls[0].at_ns, 10);
    assert_eq!(loaded.controls[0].bytes, CONTROL_REPLY);
    assert!(loaded.controls[0].length_matches);

    assert_eq!(loaded.audio.len(), 2);
    let first = &loaded.audio[0];
    assert_eq!((first.record, first.at_ns, first.len), (2, 20, 29));
    assert_eq!(first.opaque, OPAQUE);
    assert_eq!(first.trailing, 0);
    let second = &loaded.audio[1];
    assert_eq!((second.record, second.at_ns, second.trailing), (3, 40, 3));
    let units: Vec<_> = second
        .units
        .iter()
        .map(|unit| (unit.stamp, unit.len, unit.max_sfb))
        .collect();
    assert_eq!(units, [(480, 2, Some(0)), (960, 3, Some(31))]);
    assert_eq!(loaded.units().count(), 3);

    assert_eq!(loaded.batteries.len(), 1);
    let left = Charge {
        level: 85,
        charging: true,
    };
    assert_eq!(loaded.batteries[0].batteries.left, Some(left));
    assert_eq!(loaded.device_info, [vec!["Probe Pods", "A0000"]]);

    let uplink: Vec<_> = loaded.seen.iter().map(|packet| packet.uplink).collect();
    assert_eq!(
        uplink,
        [true, true, true, false, false, true, true, false, false]
    );
    assert_eq!(loaded.seen[8].opcode, Some(0x0006));
    assert_eq!(loaded.seen[7].opcode, None);
    assert_eq!(loaded.seen[7].len, 4);
}

#[test]
fn a_cut_unit_is_trailing_and_whole_units_are_not() {
    let whole = audio(1, &OPAQUE, &[(0, &SILENT_UNIT), (480, &SILENT_UNIT)]);
    let mut cut = whole.clone();
    cut.sdu
        .extend_from_slice(&[0xc0, 0x03, 0x00, 0x00, 0x09, 0x11, 0x22]);
    let mut header_only = audio(3, &OPAQUE, &[]);
    header_only.sdu.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]);
    let loaded = load(&[whole, cut, header_only]);
    let seen: Vec<_> = loaded
        .audio
        .iter()
        .map(|sdu| (sdu.units.len(), sdu.trailing))
        .collect();
    assert_eq!(seen, [(2, 0), (2, 7), (0, 4)]);
}

#[test]
fn a_battery_packet_the_parser_refuses_is_counted() {
    let mut wrong_count = BATTERY;
    wrong_count[6] = 0x02;
    let mut bad_spacer = BATTERY;
    bad_spacer[8] = 0x07;
    let unread = |sdu: &[u8]| {
        let loaded = load(&[from_pods(1, sdu)]);
        assert_eq!(loaded.batteries, []);
        loaded.battery_unread
    };
    let refused = [&wrong_count[..], &bad_spacer[..], &BATTERY[..HEADER_LEN]];
    assert_eq!(refused.map(unread), [1, 1, 1]);
    let not_battery = [&EAR[..], &EAR[..HEADER_LEN], &BATTERY[..HEADER_LEN - 1]];
    assert_eq!(not_battery.map(unread), [0, 0, 0]);
}

#[test]
fn device_strings_are_the_printable_runs_between_nuls() {
    let packet = device_info(&["Probe\u{2019}s Pods", "A0000", "", "Example Inc.", "X1"]);
    assert_eq!(
        device_strings(&packet),
        ["Probe\u{2019}s Pods", "A0000", "Example Inc.", "X1"]
    );
    let mut with_binary = device_info(&["ok"]);
    with_binary.extend_from_slice(b"line\nbreak\0tab\there\0\x7f\0caf\xc3\xa9\0~ !\0");
    with_binary.extend_from_slice(b"cut\xc3\0\xa9\0\xff\xfe\0next\xc2\x85line\0tail");
    assert_eq!(
        device_strings(&with_binary),
        ["ok", "caf\u{e9}", "~ !", "tail"]
    );
    assert_eq!(device_strings(&[]), Vec::<String>::new());
    assert_eq!(device_strings(&[0x00, 0x00]), Vec::<String>::new());
}

#[test]
fn windows_open_at_a_start_and_close_at_the_next_stop_or_start() {
    let marks = [
        mark(1, MarkKind::Other),
        mark(10, MarkKind::Start),
        mark(15, MarkKind::Other),
        mark(20, MarkKind::Stop),
        mark(25, MarkKind::Stop),
        mark(30, MarkKind::Start),
        mark(35, MarkKind::Truncated),
        mark(40, MarkKind::Start),
    ];
    let expected = [(10, Some(20)), (30, Some(40)), (40, None)];
    let found: Vec<_> = windows(&marks)
        .iter()
        .map(|window| (window.start_ns, window.end_ns))
        .collect();
    assert_eq!(found, expected);
    assert_eq!(windows(&[]), []);
    assert_eq!(windows(&[mark(5, MarkKind::Stop)]), []);
}

#[test]
fn a_tail_runs_from_the_stop_that_ends_a_start_to_the_next_start() {
    let marks = [
        mark(5, MarkKind::Stop),
        mark(10, MarkKind::Start),
        mark(15, MarkKind::Other),
        mark(20, MarkKind::Stop),
        mark(25, MarkKind::Stop),
        mark(30, MarkKind::Start),
        mark(35, MarkKind::Truncated),
        mark(40, MarkKind::Start),
        mark(50, MarkKind::Stop),
    ];
    let ends = |tail: &Option<Window>| tail.map(|tail| (tail.start_ns, tail.end_ns));
    let found: Vec<_> = tails(&marks).iter().map(ends).collect();
    assert_eq!(found, [Some((20, Some(30))), None, Some((50, None))]);
    assert_eq!(tails(&[]), []);
    assert_eq!(tails(&[mark(5, MarkKind::Stop)]), []);
    assert_eq!(tails(&[mark(5, MarkKind::Start)]), [None]);
}

#[test]
fn a_segment_holds_the_packets_from_its_start_up_to_its_end() {
    let records: Vec<_> = [5, 10, 10, 15, 20, 20, 25]
        .into_iter()
        .map(|at_ns| from_pods(at_ns, &EAR))
        .collect();
    let stamps = |window: Window| -> Vec<u64> {
        segment(&records, window)
            .iter()
            .map(|record| record.at_ns)
            .collect()
    };
    let closed = Window {
        start_ns: 10,
        end_ns: Some(20),
    };
    assert_eq!(stamps(closed), [10, 10, 15]);
    let open = Window {
        start_ns: 20,
        end_ns: None,
    };
    assert_eq!(stamps(open), [20, 20, 25]);
    let before = Window {
        start_ns: 0,
        end_ns: Some(5),
    };
    assert_eq!(stamps(before), Vec::<u64>::new());
    let after = Window {
        start_ns: 26,
        end_ns: None,
    };
    assert_eq!(stamps(after), Vec::<u64>::new());
    let backwards = Window {
        start_ns: 20,
        end_ns: Some(10),
    };
    assert_eq!(stamps(backwards), Vec::<u64>::new());
}

/// Packets of every kind the loader tells apart, then cut or padded at random.
fn sdu() -> impl Strategy<Value = Vec<u8>> {
    let known = prop_oneof![
        Just(CONTROL_REPLY.to_vec()),
        Just(BATTERY.to_vec()),
        Just(EAR.to_vec()),
        Just(device_info(&["Probe Pods"])),
        Just(audio(0, &OPAQUE, &[(0, &SILENT_UNIT), (480, &SILENT_UNIT)]).sdu),
    ];
    let bent = (
        known,
        0..SDU_BOUND,
        proptest::collection::vec(any::<u8>(), 0..8),
    )
        .prop_map(|(mut sdu, keep, tail)| {
            sdu.truncate(keep);
            sdu.extend(tail);
            sdu
        });
    prop_oneof![bent, proptest::collection::vec(any::<u8>(), 0..SDU_BOUND)]
}

fn records() -> impl Strategy<Value = Vec<Record>> {
    let record = (any::<u64>(), any::<bool>(), sdu()).prop_map(|(at_ns, sent, sdu)| {
        if sent {
            to_pods(at_ns, &sdu)
        } else {
            from_pods(at_ns, &sdu)
        }
    });
    proptest::collection::vec(record, 0..12)
}

proptest! {
    #[test]
    fn load_never_panics_and_counts_every_packet(records in records()) {
        let loaded = load(&records);
        let to_pods = usize::try_from(loaded.to_pods).unwrap();
        prop_assert_eq!([loaded.seen.len(), to_pods].iter().sum::<usize>(), records.len());
        let odd = usize::try_from(loaded.unknown_subtype).unwrap();
        let cut = usize::try_from(loaded.cut_short).unwrap();
        let read_as_uplink: usize = [loaded.audio.len(), loaded.controls.len(), odd, cut].iter().sum();
        prop_assert_eq!(loaded.seen.iter().filter(|packet| packet.uplink).count(), read_as_uplink);
        for sdu in &loaded.audio {
            let prefixes = sdu.units.len().saturating_mul(UNIT_PREFIX_LEN);
            let coded: usize = sdu.units.iter().map(|unit| unit.len).sum();
            prop_assert_eq!([AUDIO_HEADER_LEN, prefixes, coded, sdu.trailing].iter().sum::<usize>(), sdu.len);
        }
    }

    #[test]
    fn segment_never_panics(records in records(), start_ns in any::<u64>(), end_ns in any::<Option<u64>>()) {
        let window = Window { start_ns, end_ns };
        prop_assert!(segment(&records, window).len() <= records.len());
    }

    #[test]
    fn a_tail_opens_where_its_start_closes_and_never_reaches_the_next_start(
        mut marks in proptest::collection::vec((0..50_u64, 0..4_u8), 0..12),
    ) {
        marks.sort_unstable();
        let marks: Vec<Mark> = marks
            .into_iter()
            .map(|(at_ns, kind)| {
                let kinds = [MarkKind::Start, MarkKind::Stop, MarkKind::Truncated, MarkKind::Other];
                mark(at_ns, kinds[usize::from(kind)])
            })
            .collect();
        let starts = windows(&marks);
        let after = tails(&marks);
        prop_assert_eq!(after.len(), starts.len());
        let next_starts = starts.iter().skip(1).map(|start| Some(start.start_ns)).chain([None]);
        for ((start, tail), next_start_ns) in starts.iter().zip(&after).zip(next_starts) {
            let Some(tail) = tail else {
                prop_assert!(start.end_ns.is_none() || start.end_ns == next_start_ns);
                continue;
            };
            prop_assert_eq!(Some(tail.start_ns), start.end_ns);
            prop_assert_eq!(tail.end_ns, next_start_ns);
        }
    }

    #[test]
    fn device_strings_are_always_printable(sdu in proptest::collection::vec(any::<u8>(), 0..SDU_BOUND)) {
        for string in device_strings(&sdu) {
            prop_assert!(!string.is_empty());
            prop_assert!(!string.chars().any(char::is_control));
            prop_assert!(sdu.windows(string.len()).any(|window| window == string.as_bytes()));
        }
    }
}
