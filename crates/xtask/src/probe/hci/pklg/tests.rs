//! The `PacketLogger` reader, driven with traces the fixture builds, and the whole pipeline fed arbitrary bytes.

use proptest::prelude::*;

use super::super::export::render;
use super::super::fixture::{
    EVENT_REMOTE_NAME, PKLG_ACL_RECEIVED, PKLG_ACL_SENT, PKLG_COMMAND, PKLG_COUNTED_LEN,
    PKLG_EVENT, PKLG_SCO_RECEIVED, PKLG_SCO_SENT, PODS_HANDLE, START_UNIX_MICROS, Trace,
    VOICE_BYTES, mac_session, session,
};
use super::super::{Read, Records, channels_by_content, read};
use super::*;

/// One record written out by hand: 14 bytes follow the length, 1795176000 s and 4000 us, ACL received, then a 5 byte ACL packet.
const ONE_RECORD: [u8; 18] = [
    0x0e, 0x00, 0x00, 0x00, 0x40, 0x36, 0x00, 0x6b, 0xa0, 0x0f, 0x00, 0x00, 0x03, 0x0b, 0x20, 0x01,
    0x00, 0xaa,
];
/// When that record was written, in nanoseconds since 1970.
const ONE_RECORD_AT_NS: u64 = 1_795_176_000_004_000_000;
/// Where the seconds, the microseconds and the type start in a record; the length fills the bytes before the seconds.
const SECONDS_AT: usize = 4;
const MICROS_AT: usize = 8;
const TYPE_AT: usize = 12;
/// The largest value of each stamp field, and the stamp both make together, written out so the test pins the number.
const LAST_STAMP_NS: u64 = 4_294_971_589_967_295_000;

fn at_ns(ms: u64) -> u64 {
    START_UNIX_MICROS
        .saturating_add(ms.saturating_mul(1_000))
        .saturating_mul(1_000)
}

fn refused(record: usize, offset: usize, fault: Fault) -> Result<Vec<HciRecord>, TraceError> {
    Err(TraceError::PacketLogger {
        record,
        offset,
        fault,
    })
}

#[test]
fn reads_sent_and_received_acl_records() {
    let mut trace = Trace::packet_logger();
    trace.at_ms(1_500);
    trace.acl(Direction::ToPods, 0b00, &[1, 2, 3]);
    trace.acl(Direction::FromPods, 0b10, &[4]);
    trace.logged(PKLG_COMMAND, &[0x05, 0x04, 0x00]);
    trace.event(EVENT_REMOTE_NAME, &[0x00; 7]);
    trace.end_link(PODS_HANDLE);
    let expected = [
        HciRecord {
            at_ns: at_ns(1_500),
            kind: Kind::AclSent,
            data: vec![0x0b, 0x00, 0x03, 0x00, 1, 2, 3],
        },
        HciRecord {
            at_ns: at_ns(1_501),
            kind: Kind::AclReceived,
            data: vec![0x0b, 0x20, 0x01, 0x00, 4],
        },
        HciRecord {
            at_ns: at_ns(1_504),
            kind: Kind::Disconnection {
                handle: PODS_HANDLE,
            },
            data: Vec::new(),
        },
    ];
    assert_eq!(records_pklg(&trace.bytes()), Ok(expected.to_vec()));

    let kept = [
        PKLG_EVENT,
        PKLG_ACL_SENT,
        PKLG_ACL_RECEIVED,
        PKLG_SCO_SENT,
        PKLG_SCO_RECEIVED,
    ];
    for kind in (0..=u8::MAX).filter(|kind| !kept.contains(kind)) {
        let mut other = Trace::packet_logger();
        other.logged(kind, &[0x0b, 0x00, 0x00, 0x00]);
        assert_eq!(records_pklg(&other.bytes()), Ok(Vec::new()), "{kind:#04x}");
    }
}

#[test]
fn the_three_integers_of_a_record_are_little_endian() {
    let read = HciRecord {
        at_ns: ONE_RECORD_AT_NS,
        kind: Kind::AclReceived,
        data: vec![0x0b, 0x20, 0x01, 0x00, 0xaa],
    };
    assert_eq!(records_pklg(&ONE_RECORD), Ok(vec![read]));

    let mut big_endian_length = ONE_RECORD;
    big_endian_length[..SECONDS_AT].reverse();
    assert_eq!(
        records_pklg(&big_endian_length),
        refused(0, 0, Fault::CutShort)
    );
    let mut latest = ONE_RECORD;
    latest[SECONDS_AT..MICROS_AT].fill(0xff);
    latest[MICROS_AT..TYPE_AT].fill(0xff);
    let stamps: Vec<u64> = records_pklg(&latest)
        .unwrap()
        .iter()
        .map(|record| record.at_ns)
        .collect();
    assert_eq!(stamps, [LAST_STAMP_NS]);
}

#[test]
fn a_truncated_record_is_an_error_with_its_offset() {
    let mut trace = Trace::packet_logger();
    trace.acl(Direction::ToPods, 0b00, &[1, 2, 3]);
    let first_len = trace.bytes().len();
    trace.acl(Direction::FromPods, 0b10, &[4, 5]);
    let bytes = trace.bytes();
    let whole = records_pklg(&bytes).unwrap();
    assert_eq!(whole.len(), 2);
    for cut_at in 1..bytes.len() {
        let read = records_pklg(&bytes[..cut_at]);
        let (record, offset) = if cut_at < first_len {
            (0, 0)
        } else {
            (1, first_len)
        };
        let expected = if cut_at == first_len {
            Ok(whole[..1].to_vec())
        } else if cut_at - offset < HEADER_LEN {
            refused(record, offset, Fault::Residue)
        } else {
            refused(record, offset, Fault::CutShort)
        };
        assert_eq!(read, expected, "cut at {cut_at}");
    }
}

#[test]
fn residue_after_the_last_record_is_refused() {
    let mut trace = Trace::packet_logger();
    trace.acl(Direction::ToPods, 0b00, &[1, 2, 3]);
    let bytes = trace.bytes();
    for residue in 1..HEADER_LEN {
        let mut with_residue = bytes.clone();
        with_residue.extend(vec![0x00; residue]);
        let expected = refused(1, bytes.len(), Fault::Residue);
        assert_eq!(records_pklg(&with_residue), expected, "{residue} bytes");
    }

    // A length under what it counts itself is refused, and the smallest that fits is a record with no payload.
    for counted in 0..=PKLG_COUNTED_LEN {
        let mut short = bytes.clone();
        short.extend(counted.to_le_bytes());
        short.extend([0x00; HEADER_LEN - SECONDS_AT]);
        let read = records_pklg(&short);
        if counted == PKLG_COUNTED_LEN {
            assert_eq!(read.map(|kept| kept.len()), Ok(1));
        } else {
            let expected = refused(1, bytes.len(), Fault::LengthTooSmall);
            assert_eq!(read, expected, "a length of {counted}");
        }
    }
    assert_eq!(records_pklg(&[]), Err(TraceError::NoRecord));
}

#[test]
fn sco_records_are_counted_and_hold_no_audio() {
    let mut trace = Trace::packet_logger();
    trace.logged(PKLG_SCO_SENT, VOICE_BYTES);
    trace.logged(PKLG_SCO_RECEIVED, VOICE_BYTES);
    let kept = records_pklg(&trace.bytes()).unwrap();
    let read: Vec<_> = kept
        .iter()
        .map(|record| (record.kind, record.data.as_slice()))
        .collect();
    let none: &[u8] = &[];
    let sent = (Kind::SynchronousData(Direction::ToPods), none);
    let received = (Kind::SynchronousData(Direction::FromPods), none);
    assert_eq!(read, [sent, received]);
}

#[test]
fn the_btsnoop_pattern_alone_chooses_the_reader() {
    let btsnoop = session().bytes();
    let pattern_len = b"btsnoop\0".len();
    assert!(matches!(
        read(&btsnoop),
        Ok(Read::Btsnoop(Records { cut: None, .. }))
    ));
    assert_eq!(
        read(&btsnoop[..pattern_len]),
        Err(TraceError::NoFileHeader { len: pattern_len })
    );
    assert_eq!(
        read(&btsnoop[..pattern_len - 1]),
        Err(TraceError::PacketLogger {
            record: 0,
            offset: 0,
            fault: Fault::Residue
        })
    );
    let mac = mac_session().bytes();
    assert_eq!(read(&mac), records_pklg(&mac).map(Read::PacketLogger));
    assert!(records_pklg(&mac).is_ok_and(|kept| !kept.is_empty()));
    assert_eq!(read(&[]), Err(TraceError::NoRecord));
}

/// Runs every layer over a trace; the return value only keeps the calls from being optimised away.
fn read_everything(trace: &[u8]) -> usize {
    let Ok(kept) = records_pklg(trace) else {
        return 0;
    };
    let found = channels_by_content(&kept);
    let rendered = render(Read::PacketLogger(kept)).map_or(0, |outputs| outputs.len());
    rendered.saturating_add(found.by_content.len())
}

/// One record whose claimed length need not match the payload behind it, so the walker meets cut and shifted records.
fn loose_record() -> impl Strategy<Value = Vec<u8>> {
    let counted = prop_oneof![0_u32..80, any::<u32>()];
    let kind = prop_oneof![0_u8..4, 0x18_u8..0x1a, any::<u8>()];
    let payload = proptest::collection::vec(any::<u8>(), 0..64);
    (counted, any::<[u8; 8]>(), kind, payload).prop_map(|(counted, stamp, kind, payload)| {
        let mut record = counted.to_le_bytes().to_vec();
        record.extend(stamp);
        record.push(kind);
        record.extend(payload);
        record
    })
}

proptest! {
    #[test]
    fn never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        read_everything(&bytes);
    }

    #[test]
    fn never_panics_on_loose_records(
        loose in proptest::collection::vec(loose_record(), 0..16),
        tail in proptest::collection::vec(any::<u8>(), 0..16),
    ) {
        let mut trace = loose.concat();
        trace.extend(tail);
        read_everything(&trace);
    }

    #[test]
    fn whole_records_always_parse_and_only_acl_keeps_bytes(
        packets in proptest::collection::vec((any::<u8>(), proptest::collection::vec(any::<u8>(), 0..48)), 1..64),
    ) {
        let mut trace = Trace::packet_logger();
        for (kind, payload) in &packets {
            trace.logged(*kind, payload);
        }
        let kept = records_pklg(&trace.bytes()).unwrap();
        prop_assert!(kept.len() <= packets.len());
        for record in &kept {
            let is_acl = matches!(record.kind, Kind::AclSent | Kind::AclReceived);
            prop_assert!(is_acl || record.data.is_empty());
        }
        read_everything(&trace.bytes());
    }

    #[test]
    fn never_panics_on_a_session_with_one_byte_changed(at in any::<prop::sample::Index>(), byte in any::<u8>()) {
        let mut trace = mac_session().bytes();
        let at = at.index(trace.len());
        trace[at] = byte;
        read_everything(&trace);
    }
}
