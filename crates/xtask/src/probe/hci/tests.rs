//! The btsnoop reader, driven with traces the fixture builds, and the whole pipeline fed arbitrary bytes.

use pods_proto::sdulog::Direction;
use proptest::prelude::*;

use super::export::render;
use super::fixture::{
    EVENT_CONNECTION_COMPLETE, EVENT_DISCONNECTION_COMPLETE, EVENT_LINK_KEY, EVENT_REMOTE_NAME,
    KEYBOARD_HANDLE, START_UNIX_MICROS, Trace, session,
};
use super::*;
use crate::probe::a2dp;

/// The identification pattern, the version and the datalink type.
const FILE_HEADER: usize = 16;
/// Where the version starts in the file header.
const VERSION_AT: usize = 8;
/// Where the datalink type starts in the file header.
const DATALINK_AT: usize = 12;
/// Two lengths, flags, drops and the timestamp.
const RECORD_HEADER: usize = 24;
/// Where the included length starts in a record header.
const INCLUDED_LEN_AT: usize = 4;
/// Where the timestamp starts in a record header.
const STAMP_AT: usize = 16;
/// The most data bytes one record may claim, written out so the test pins the number.
const LONGEST_RECORD: usize = 65_535;
/// The event code of a completed synchronous connection.
const EVENT_SYNCHRONOUS_CONNECTION: u8 = 0x2c;
/// A monitor opcode this reader has no use for: a system note.
const OPCODE_SYSTEM_NOTE: u16 = 12;
/// Monitor opcode of ACL data the host sent.
const OPCODE_ACL_TX: u16 = 4;

/// The stamp of a record written `ms` milliseconds into a synthetic trace.
fn at_ns(ms: u64) -> u64 {
    START_UNIX_MICROS
        .saturating_add(ms.saturating_mul(1_000))
        .saturating_mul(1_000)
}

fn with_bytes(mut trace: Vec<u8>, at: usize, bytes: &[u8]) -> Vec<u8> {
    trace[at..][..bytes.len()].copy_from_slice(bytes);
    trace
}

#[test]
fn reads_the_monitor_datalink_and_refuses_an_unknown_one() {
    let mut trace = Trace::new();
    trace.at_ms(1_500);
    trace.acl(Direction::ToPods, 0b00, &[1, 2, 3]);
    trace.acl(Direction::FromPods, 0b10, &[4]);
    let bytes = trace.bytes();
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
    ];
    assert_eq!(records(&bytes), Ok(expected.to_vec()));
    assert_eq!(records(&Trace::new().bytes()), Ok(Vec::new()));

    for datalink in [0, 1001, 1002, 2000, 2002, u32::MAX] {
        let other = with_bytes(bytes.clone(), DATALINK_AT, &datalink.to_be_bytes());
        assert_eq!(
            records(&other),
            Err(TraceError::UnknownDatalink { datalink })
        );
    }
    let little_endian = with_bytes(bytes.clone(), DATALINK_AT, &2001_u32.to_le_bytes());
    assert!(matches!(
        records(&little_endian),
        Err(TraceError::UnknownDatalink { .. })
    ));
    let error = TraceError::UnknownDatalink { datalink: 1001 }.to_string();
    assert!(error.contains("1001") && error.contains("2001"), "{error}");
}

#[test]
fn a_file_that_is_no_btsnoop_trace_is_refused() {
    let bytes = session().bytes();
    for len in [0, 1, FILE_HEADER - 1] {
        assert_eq!(
            records(&bytes[..len]),
            Err(TraceError::NoFileHeader { len })
        );
    }
    for at in 0..VERSION_AT {
        let mut other = bytes.clone();
        other[at] ^= 0x20;
        assert_eq!(records(&other), Err(TraceError::BadMagic), "byte {at}");
    }
    for version in [0, 2, u32::MAX] {
        let other = with_bytes(bytes.clone(), VERSION_AT, &version.to_be_bytes());
        assert_eq!(records(&other), Err(TraceError::BadVersion { version }));
    }
}

#[test]
fn a_truncated_record_is_an_error_with_its_offset() {
    let mut trace = Trace::new();
    trace.acl(Direction::ToPods, 0b00, &[1, 2, 3]);
    trace.acl(Direction::FromPods, 0b10, &[4]);
    let bytes = trace.bytes();
    let second = FILE_HEADER + RECORD_HEADER + 7;
    assert_eq!(bytes.len(), second + RECORD_HEADER + 5);

    assert_eq!(records(&bytes[..second]).unwrap().len(), 1);
    for left in 1..RECORD_HEADER {
        let cut = TraceError::CutRecordHeader {
            record: 1,
            offset: second,
            left,
        };
        assert_eq!(records(&bytes[..second + left]), Err(cut));
    }
    for left in 0..5 {
        let cut = TraceError::CutRecord {
            record: 1,
            offset: second,
            len: 5,
            left,
        };
        assert_eq!(records(&bytes[..second + RECORD_HEADER + left]), Err(cut));
    }
    let error = TraceError::CutRecord {
        record: 1,
        offset: second,
        len: 5,
        left: 4,
    }
    .to_string();
    assert_eq!(
        error,
        "btsnoop record 1 at byte offset 47 is cut short: it claims 5 bytes of data and 4 are left"
    );
}

#[test]
fn a_record_length_past_the_file_or_past_65535_is_refused() {
    let mut trace = Trace::new();
    trace.acl(Direction::ToPods, 0b00, &[1, 2, 3]);
    let bytes = trace.bytes();
    let claims = |len: u32| {
        with_bytes(
            bytes.clone(),
            FILE_HEADER + INCLUDED_LEN_AT,
            &len.to_be_bytes(),
        )
    };
    let past_the_file = TraceError::CutRecord {
        record: 0,
        offset: FILE_HEADER,
        len: LONGEST_RECORD,
        left: 7,
    };
    assert_eq!(records(&claims(65_535)), Err(past_the_file));
    for len in [65_536, u32::MAX] {
        let too_long = TraceError::RecordTooLong {
            record: 0,
            offset: FILE_HEADER,
            len,
        };
        assert_eq!(records(&claims(len)), Err(too_long));
    }

    let mut longest = Trace::new();
    longest.record(OPCODE_ACL_TX, &vec![0x5a; LONGEST_RECORD]);
    let kept = records(&longest.bytes()).unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].data.len(), LONGEST_RECORD);
}

#[test]
fn a_stamp_outside_the_nanosecond_range_is_refused() {
    let mut trace = Trace::new();
    trace.record(OPCODE_SYSTEM_NOTE, b"note");
    trace.acl(Direction::ToPods, 0b00, &[1]);
    let bytes = trace.bytes();
    let second = FILE_HEADER + RECORD_HEADER + 4;
    for stamp in [0_u64, 62_168_255_999_999_999, u64::MAX] {
        let other = with_bytes(bytes.clone(), second + STAMP_AT, &stamp.to_be_bytes());
        let refused = TraceError::BadStamp {
            record: 1,
            offset: second,
        };
        assert_eq!(records(&other), Err(refused), "{stamp}");
    }
    let epoch = with_bytes(
        bytes.clone(),
        second + STAMP_AT,
        &62_168_256_000_000_000_u64.to_be_bytes(),
    );
    assert_eq!(records(&epoch).unwrap()[0].at_ns, 0);
}

#[test]
fn only_the_records_the_tables_need_are_kept() {
    let mut trace = Trace::new();
    trace.record(OPCODE_SYSTEM_NOTE, b"a note from the kernel");
    for code in [EVENT_CONNECTION_COMPLETE, EVENT_REMOTE_NAME, EVENT_LINK_KEY] {
        trace.event(code, &[0x5a; 24]);
    }
    trace.event(EVENT_SYNCHRONOUS_CONNECTION, &[0x00; 17]);
    trace.event(EVENT_SYNCHRONOUS_CONNECTION, &[0x0d; 17]);
    trace.event(EVENT_SYNCHRONOUS_CONNECTION, &[]);
    trace.event(EVENT_DISCONNECTION_COMPLETE, &[0x00, 0x0c, 0xf0, 0x13]);
    trace.event(EVENT_DISCONNECTION_COMPLETE, &[0x04, 0x0c, 0x00, 0x13]);
    trace.event(EVENT_DISCONNECTION_COMPLETE, &[0x00, 0x0c]);
    let kept = records(&trace.bytes()).unwrap();
    let kinds: Vec<Kind> = kept.iter().map(|record| record.kind).collect();
    assert_eq!(
        kinds,
        [
            Kind::SynchronousConnection { opened: true },
            Kind::SynchronousConnection { opened: false },
            Kind::Disconnection {
                handle: KEYBOARD_HANDLE
            },
        ]
    );
    assert!(kept.iter().all(|record| record.data.is_empty()));
    assert_eq!(kept[0].at_ns, at_ns(4));
}

#[test]
fn every_error_names_the_record_and_the_rule() {
    for (error, text) in [
        (
            TraceError::NoFileHeader { len: 0 },
            "btsnoop trace is 0 bytes, shorter than its 16 byte file header",
        ),
        (
            TraceError::BadMagic,
            "btsnoop trace does not start with the btsnoop identification pattern",
        ),
        (
            TraceError::BadVersion { version: 2 },
            "btsnoop trace has version 2, and only version 1 exists",
        ),
        (
            TraceError::UnknownDatalink { datalink: 1002 },
            "btsnoop trace has datalink type 1002; only the monitor datalink 2001, which btmon -w writes, is read",
        ),
        (
            TraceError::CutRecordHeader {
                record: 3,
                offset: 90,
                left: 23,
            },
            "btsnoop record 3 at byte offset 90 is cut short: 23 bytes are left and a record header is 24",
        ),
        (
            TraceError::RecordTooLong {
                record: 3,
                offset: 90,
                len: 65_536,
            },
            "btsnoop record 3 at byte offset 90 claims 65536 bytes of data, and a record holds at most 65535",
        ),
        (
            TraceError::BadStamp {
                record: 3,
                offset: 90,
            },
            "btsnoop record 3 at byte offset 90 has a timestamp outside the years 1970 to 2554, which is all a nanosecond stamp holds",
        ),
    ] {
        assert_eq!(error.to_string(), text);
    }
}

/// Bytes that make signalling commands and channel identifiers the reader acts on likely.
fn signalling_byte() -> impl Strategy<Value = u8> {
    prop_oneof![
        Just(0x00),
        Just(0x01),
        Just(0x02),
        Just(0x10),
        Just(0x19),
        Just(0x1f),
        Just(0x40),
        Just(0x41),
        any::<u8>(),
    ]
}

/// One thing a hostile trace may hold on the pods' link.
#[derive(Debug, Clone)]
enum Step {
    Signal(bool, u8, Vec<u8>),
    Pdu(bool, u16, Vec<u8>, Vec<usize>),
    Acl(bool, u16, Vec<u8>),
    Event(u8, Vec<u8>),
}

fn side(from_pods: bool) -> Direction {
    if from_pods {
        Direction::FromPods
    } else {
        Direction::ToPods
    }
}

fn step() -> impl Strategy<Value = Step> {
    let body = || proptest::collection::vec(signalling_byte(), 0..12);
    let cid = prop_oneof![Just(0x0002_u16), 0x0040_u16..0x0044];
    let payload = proptest::collection::vec(any::<u8>(), 8..40);
    let cuts = proptest::collection::vec(1_usize..4, 0..3);
    let event = prop_oneof![Just(0x05_u8), Just(0x2c), Just(0x18)];
    prop_oneof![
        4 => (any::<bool>(), 1_u8..8, body()).prop_map(|(from, code, data)| Step::Signal(from, code, data)),
        4 => (any::<bool>(), cid, payload, cuts).prop_map(|(from, cid, payload, cuts)| Step::Pdu(from, cid, payload, cuts)),
        1 => (any::<bool>(), 0_u16..4, body()).prop_map(|(from, flag, data)| Step::Acl(from, flag, data)),
        1 => (event, body()).prop_map(|(code, parameters)| Step::Event(code, parameters)),
    ]
}

fn play(steps: &[Step]) -> Vec<u8> {
    let mut trace = Trace::new();
    for step in steps {
        match step {
            Step::Signal(from, code, data) => trace.signal(side(*from), *code, data),
            Step::Pdu(from, cid, payload, cuts) => {
                trace.fragments(side(*from), *cid, payload, cuts);
            }
            Step::Acl(from, flag, data) => trace.acl(side(*from), *flag, data),
            Step::Event(code, parameters) => trace.event(*code, parameters),
        }
    }
    trace.bytes()
}

/// Runs every layer over a trace; the return value only keeps the calls from being optimised away.
fn read_everything(trace: &[u8]) -> usize {
    let Ok(records) = records(trace) else {
        return 0;
    };
    let found = channels(&records);
    let read = [
        a2dp::capabilities(&found).len(),
        a2dp::media(&found).len(),
        usize::from(a2dp::configuration(&found).is_some()),
        usize::try_from(a2dp::unread(&found).not_rtp).unwrap(),
        render(records).map_or(0, |outputs| outputs.len()),
    ];
    read.into_iter().sum()
}

/// One record whose claimed length need not match the data behind it, so the walker meets cut and shifted records.
fn loose_record() -> impl Strategy<Value = Vec<u8>> {
    let claimed = prop_oneof![0_u32..64, any::<u32>()];
    let opcode = prop_oneof![3_u16..6, any::<u16>()];
    let stamp = prop_oneof![Just(62_168_256_000_000_001_u64), any::<u64>()];
    let data = proptest::collection::vec(any::<u8>(), 0..64);
    (claimed, opcode, stamp, data).prop_map(|(claimed, opcode, stamp, data)| {
        let mut record = claimed.to_be_bytes().to_vec();
        record.extend(claimed.to_be_bytes());
        record.extend(0_u16.to_be_bytes());
        record.extend(opcode.to_be_bytes());
        record.extend(0_u32.to_be_bytes());
        record.extend(stamp.to_be_bytes());
        record.extend(data);
        record
    })
}

proptest! {
    #[test]
    fn never_panics(
        loose in proptest::collection::vec(loose_record(), 0..16),
        tail in proptest::collection::vec(any::<u8>(), 0..256),
    ) {
        let mut trace = Trace::new().bytes();
        trace.extend(loose.concat());
        trace.extend(tail);
        read_everything(&trace);
    }

    #[test]
    fn never_panics_on_whole_records(
        packets in proptest::collection::vec((0_u16..24, proptest::collection::vec(any::<u8>(), 0..48)), 0..64),
    ) {
        let mut trace = Trace::new();
        for (opcode, data) in &packets {
            trace.record(*opcode, data);
        }
        prop_assert!(records(&trace.bytes()).is_ok());
        read_everything(&trace.bytes());
    }

    #[test]
    fn never_panics_on_signalling_and_fragments(steps in proptest::collection::vec(step(), 0..96)) {
        let trace = play(&steps);
        prop_assert!(records(&trace).is_ok());
        prop_assert!(read_everything(&trace) >= 4, "the four tables are always rendered");
    }

    #[test]
    fn never_panics_on_a_session_with_one_byte_changed(at in any::<prop::sample::Index>(), byte in any::<u8>()) {
        let mut trace = session().bytes();
        let at = at.index(trace.len());
        trace[at] = byte;
        read_everything(&trace);
    }
}
