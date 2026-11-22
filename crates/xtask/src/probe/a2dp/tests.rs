//! The music tables, driven with traces the fixture builds.

use pods_proto::packets::CONTROL_PSM;
use pods_proto::sdulog::Direction;
use proptest::prelude::*;

use super::super::hci::fixture::{
    AAC_BYTES, ACCEPT, AVDTP_PSM, AVDTP_START, AVDTP_SUSPEND, CODEC_AAC, CODEC_SBC, CODEC_VENDOR,
    COMMAND, FIRST_SEID, GET_ALL_CAPABILITIES, GET_CAPABILITIES, KEYBOARD_HANDLE, MEDIA_CIDS,
    MEDIA_CODEC, MEDIA_PAYLOAD_LEN, MEDIA_TRANSPORT, PODS_HANDLE, SBC_BYTES, SET_CONFIGURATION,
    SIGNALLING_CIDS, START_UNIX_MICROS, Trace, VENDOR_BYTES, avdtp, capability, media_codec, rtp,
    session,
};
use super::super::hci::{channels, records};
use super::*;

const HOST: Direction = Direction::ToPods;
const PODS: Direction = Direction::FromPods;
/// AVDTP message type of a refused command.
const REJECT: u8 = 0b11;
/// `L2CAP_DISCONNECTION_REQ` (Core 6.1 Vol 3 Part A 4.6).
const DISCONNECTION_REQUEST: u8 = 0x06;
/// The first bytes of the packets of a fragmented message: label 1, response accept, in a start, a continue and an end packet (AVDTP 1.3 section 8.4.2, Table 8.4).
const START_PACKET_ACCEPT: u8 = 0x16;
const CONTINUE_PACKET_ACCEPT: u8 = 0x1a;
const END_PACKET_ACCEPT: u8 = 0x1e;
/// How many packets the start packet says its message takes.
const SIGNAL_PACKETS: u8 = 0x02;
/// What a rejected Set Configuration ends with: the category at fault and the error (`BlueZ` profiles/audio/avdtp.h, `AVDTP_UNSUPPORTED_CONFIGURATION`).
const REFUSED_CONFIGURATION: [u8; 2] = [MEDIA_CODEC, 0x29];
/// What a rejected Suspend ends with: the stream end point at fault and the error (`BlueZ` profiles/audio/avdtp.h, `AVDTP_BAD_STATE`).
const REFUSED_SUSPEND: [u8; 2] = [FIRST_SEID, 0x31];
/// The first byte of an RTP header of version 1 and of version 3, which no media packet carries (RFC 3550 section 5.1).
const RTP_VERSION_1: u8 = 0x40;
const RTP_VERSION_3: u8 = 0xc0;
/// The vendor and the codec identifier a vendor codec element opens with, 4 and 2 bytes (A2DP 1.3 section 4.7.2).
const VENDOR_IDENTIFIERS_LEN: u8 = 6;
const MICROS_PER_SECOND: u64 = 1_000_000;
/// The RTP header in front of every media packet.
const RTP_HEADER_LEN: usize = 12;
/// The identifiers of a third AVDTP channel.
const THIRD_CIDS: (u16, u16) = (0x0044, 0x0054);
/// The whole second every synthetic trace starts in.
const START_SECOND: u64 = START_UNIX_MICROS / MICROS_PER_SECOND;

/// The music of every link `handle` named, in the order the links came up.
fn on(trace: &Trace, handle: u16) -> Vec<Music> {
    let found = channels(&records(&trace.bytes()).unwrap().kept);
    let on_handle = found.list.iter().filter(|channel| channel.handle == handle);
    let mut links: Vec<usize> = on_handle.map(|channel| channel.link).collect();
    links.dedup();
    links.iter().map(|link| music(&found, *link)).collect()
}

/// The music of the one link on the pods' handle.
fn read(trace: &Trace) -> Music {
    let [only] = <[Music; 1]>::try_from(on(trace, PODS_HANDLE)).unwrap();
    only
}

/// A trace with the signalling channel and the media channel open.
fn streaming() -> Trace {
    let mut trace = Trace::new();
    trace.open(HOST, AVDTP_PSM, SIGNALLING_CIDS);
    trace.open(HOST, AVDTP_PSM, MEDIA_CIDS);
    trace
}

fn signal(trace: &mut Trace, sender: Direction, label: u8, message: u8, id: u8, body: &[u8]) {
    trace.send(sender, SIGNALLING_CIDS, &avdtp(label, message, id, body));
}

fn standard(codec_type: u8, bytes: &[u8]) -> Codec {
    Codec {
        kind: CodecKind::Standard(codec_type),
        bytes: bytes.to_vec(),
    }
}

/// The body of a Set Configuration command: two stream end point bytes, then the capabilities.
fn choice(capabilities: &[u8]) -> Vec<u8> {
    let mut body = vec![FIRST_SEID, FIRST_SEID];
    body.extend(capabilities);
    body
}

#[test]
fn avdtp_capabilities_and_configuration_are_listed() {
    let found = read(&session());
    let vendor = Codec {
        kind: CodecKind::Vendor {
            vendor_id: 0x0000_004c,
            codec_id: 0x8001,
        },
        bytes: VENDOR_BYTES.to_vec(),
    };
    assert_eq!(
        found.capabilities,
        [
            standard(CODEC_SBC, &SBC_BYTES),
            standard(CODEC_AAC, &AAC_BYTES),
            vendor
        ]
    );
    assert_eq!(found.configuration, Some(standard(CODEC_AAC, &AAC_BYTES)));
    assert_eq!(found.unread, Unread::default());
}

#[test]
fn only_what_the_pods_accepted_is_a_capability() {
    let sbc = media_codec(CODEC_SBC, &SBC_BYTES);
    let mut trace = streaming();
    signal(&mut trace, HOST, 1, COMMAND, GET_CAPABILITIES, &sbc);
    signal(&mut trace, HOST, 1, ACCEPT, GET_CAPABILITIES, &sbc);
    signal(&mut trace, PODS, 1, REJECT, GET_CAPABILITIES, &sbc);
    signal(&mut trace, PODS, 1, ACCEPT, SET_CONFIGURATION, &sbc);
    trace.send(PODS, MEDIA_CIDS, &avdtp(1, ACCEPT, GET_CAPABILITIES, &sbc));
    assert_eq!(read(&trace).capabilities, []);
    signal(&mut trace, PODS, 1, ACCEPT, GET_CAPABILITIES, &sbc);
    signal(&mut trace, PODS, 2, ACCEPT, GET_ALL_CAPABILITIES, &sbc);
    let found = read(&trace);
    assert_eq!(
        found.capabilities,
        [
            standard(CODEC_SBC, &SBC_BYTES),
            standard(CODEC_SBC, &SBC_BYTES)
        ]
    );
    assert_eq!(found.configuration, None);
}

#[test]
fn a_configuration_is_set_only_when_its_command_is_accepted() {
    let sbc = choice(&media_codec(CODEC_SBC, &SBC_BYTES));
    let aac = choice(&media_codec(CODEC_AAC, &AAC_BYTES));
    let sbc_set = Some(standard(CODEC_SBC, &SBC_BYTES));
    let mut trace = streaming();
    signal(&mut trace, HOST, 3, COMMAND, SET_CONFIGURATION, &sbc);
    assert_eq!(read(&trace).configuration, None);
    signal(&mut trace, PODS, 4, ACCEPT, SET_CONFIGURATION, &[]);
    signal(&mut trace, HOST, 3, ACCEPT, SET_CONFIGURATION, &[]);
    assert_eq!(read(&trace).configuration, None);
    signal(&mut trace, PODS, 3, ACCEPT, SET_CONFIGURATION, &[]);
    assert_eq!(read(&trace).configuration, sbc_set);

    signal(&mut trace, PODS, 5, COMMAND, SET_CONFIGURATION, &aac);
    assert_eq!(read(&trace).configuration, sbc_set);
    signal(
        &mut trace,
        HOST,
        5,
        REJECT,
        SET_CONFIGURATION,
        &REFUSED_CONFIGURATION,
    );
    signal(&mut trace, HOST, 5, ACCEPT, SET_CONFIGURATION, &[]);
    assert_eq!(read(&trace).configuration, sbc_set);
    signal(&mut trace, PODS, 6, COMMAND, SET_CONFIGURATION, &aac);
    signal(&mut trace, HOST, 6, ACCEPT, SET_CONFIGURATION, &[]);
    signal(&mut trace, PODS, 6, ACCEPT, SET_CONFIGURATION, &[]);
    assert_eq!(
        read(&trace).configuration,
        Some(standard(CODEC_AAC, &AAC_BYTES))
    );
    signal(
        &mut trace,
        PODS,
        7,
        COMMAND,
        SET_CONFIGURATION,
        &choice(&[]),
    );
    signal(&mut trace, HOST, 7, ACCEPT, SET_CONFIGURATION, &[]);
    assert_eq!(
        read(&trace).configuration,
        Some(standard(CODEC_AAC, &AAC_BYTES))
    );
}

#[test]
fn a_set_configuration_command_cut_short_replaces_the_one_before_it() {
    let aac = choice(&media_codec(CODEC_AAC, &AAC_BYTES));
    let mut trace = streaming();
    signal(&mut trace, HOST, 3, COMMAND, SET_CONFIGURATION, &aac);
    signal(
        &mut trace,
        HOST,
        3,
        COMMAND,
        SET_CONFIGURATION,
        &[FIRST_SEID],
    );
    signal(&mut trace, PODS, 3, ACCEPT, SET_CONFIGURATION, &[]);
    let found = read(&trace);
    assert_eq!(found.configuration, None);
    assert_eq!(found.unread.malformed_signals, 1);
}

#[test]
fn a_vendor_codec_reports_vendor_and_codec_identifier() {
    let mut offered = capability(MEDIA_TRANSPORT, &[]);
    offered.extend(media_codec(CODEC_VENDOR, &VENDOR_BYTES));
    let mut trace = streaming();
    signal(&mut trace, PODS, 1, ACCEPT, GET_CAPABILITIES, &offered);
    let found = read(&trace);
    let vendor = Codec {
        kind: CodecKind::Vendor {
            vendor_id: 0x0000_004c,
            codec_id: 0x8001,
        },
        bytes: VENDOR_BYTES.to_vec(),
    };
    assert_eq!(found.capabilities, [vendor]);
    assert_eq!(found.unread.malformed_signals, 0);

    let identifiers_only = media_codec(
        CODEC_VENDOR,
        &VENDOR_BYTES[..usize::from(VENDOR_IDENTIFIERS_LEN)],
    );
    signal(
        &mut trace,
        PODS,
        2,
        ACCEPT,
        GET_CAPABILITIES,
        &identifiers_only,
    );
    assert_eq!(read(&trace).capabilities.len(), 2);
    let short = usize::from(VENDOR_IDENTIFIERS_LEN) - 1;
    let cut = media_codec(CODEC_VENDOR, &VENDOR_BYTES[..short]);
    signal(&mut trace, PODS, 3, ACCEPT, GET_CAPABILITIES, &cut);
    let found = read(&trace);
    assert_eq!(found.capabilities.len(), 2);
    assert_eq!(found.unread.malformed_signals, 1);
}

#[test]
fn a_capability_list_cut_short_is_counted() {
    let mut cut = media_codec(CODEC_SBC, &SBC_BYTES);
    cut.extend(&media_codec(CODEC_AAC, &AAC_BYTES)[..4]);
    let mut trace = streaming();
    signal(&mut trace, PODS, 1, ACCEPT, GET_CAPABILITIES, &cut);
    signal(
        &mut trace,
        PODS,
        2,
        ACCEPT,
        GET_CAPABILITIES,
        &capability(MEDIA_CODEC, &SBC_BYTES[..1]),
    );
    trace.send(
        PODS,
        SIGNALLING_CIDS,
        &avdtp(1, ACCEPT, GET_CAPABILITIES, &[])[..1],
    );
    trace.send(PODS, SIGNALLING_CIDS, &[]);
    let found = read(&trace);
    assert_eq!(found.capabilities, [standard(CODEC_SBC, &SBC_BYTES)]);
    assert_eq!(found.unread.malformed_signals, 4);
}

#[test]
fn a_fragmented_signal_is_counted_and_not_read() {
    let mut start = vec![START_PACKET_ACCEPT, SIGNAL_PACKETS, GET_CAPABILITIES];
    start.extend(media_codec(CODEC_SBC, &SBC_BYTES));
    let mut trace = streaming();
    trace.send(PODS, SIGNALLING_CIDS, &start);
    let end = [END_PACKET_ACCEPT, GET_CAPABILITIES, MEDIA_CODEC];
    trace.send(PODS, SIGNALLING_CIDS, &end);
    trace.send(
        PODS,
        SIGNALLING_CIDS,
        &[CONTINUE_PACKET_ACCEPT, GET_CAPABILITIES],
    );
    let found = read(&trace);
    assert_eq!(found.capabilities, []);
    let counted = Unread {
        fragmented_signals: 3,
        ..Unread::default()
    };
    assert_eq!(found.unread, counted);
}

#[test]
fn avdtp_suspend_and_start_are_counted() {
    let mut trace = streaming();
    trace.at_ms(100);
    signal(&mut trace, HOST, 1, COMMAND, AVDTP_START, &[FIRST_SEID]);
    signal(&mut trace, PODS, 1, ACCEPT, AVDTP_START, &[]);
    trace.at_ms(1_200);
    signal(&mut trace, HOST, 2, COMMAND, AVDTP_SUSPEND, &[FIRST_SEID]);
    signal(&mut trace, PODS, 2, ACCEPT, AVDTP_SUSPEND, &[]);
    signal(&mut trace, PODS, 3, COMMAND, AVDTP_START, &[FIRST_SEID]);
    signal(&mut trace, PODS, 4, COMMAND, AVDTP_SUSPEND, &[FIRST_SEID]);
    signal(&mut trace, HOST, 4, REJECT, AVDTP_SUSPEND, &REFUSED_SUSPEND);
    signal(&mut trace, HOST, 5, COMMAND, AVDTP_SUSPEND, &[FIRST_SEID]);
    trace.at_ms(5_000);
    signal(&mut trace, HOST, 6, COMMAND, AVDTP_START, &[FIRST_SEID]);
    trace.send(
        HOST,
        MEDIA_CIDS,
        &avdtp(7, COMMAND, AVDTP_START, &[FIRST_SEID]),
    );
    let rows: Vec<_> = read(&trace)
        .seconds
        .iter()
        .map(|second| (second.at_s - START_SECOND, second.suspends, second.starts))
        .collect();
    assert_eq!(rows, [(0, 0, 1), (1, 3, 1), (5, 0, 1)]);
    assert!(read(&trace).seconds.iter().all(|row| row.packets == 0));
}

#[test]
fn rtp_gaps_are_counted_across_the_16_bit_wrap() {
    let send = |sequences: &[u16]| {
        let mut trace = streaming();
        for sequence in sequences {
            trace.send(HOST, MEDIA_CIDS, &rtp(*sequence));
        }
        let rows = read(&trace).seconds;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].packets, u64::try_from(sequences.len()).unwrap());
        rows[0].gaps
    };
    assert_eq!(send(&[65_533, 65_534, 65_535, 0, 1, 2]), 0);
    assert_eq!(send(&[65_534, 65_535, 1, 2]), 1);
    assert_eq!(send(&[65_535, 0, 0, 1]), 1);
    assert_eq!(send(&[7, 8, 6, 7]), 1);
    assert_eq!(send(&[10, 12, 14, 15, 0]), 3);
    assert_eq!(send(&[9]), 0);
}

#[test]
fn both_sides_may_wait_for_an_answer_with_one_label() {
    let aac = choice(&media_codec(CODEC_AAC, &AAC_BYTES));
    let sbc = choice(&media_codec(CODEC_SBC, &SBC_BYTES));
    let crossed = |answered_first: Direction| {
        let mut trace = streaming();
        signal(&mut trace, HOST, 3, COMMAND, SET_CONFIGURATION, &aac);
        signal(&mut trace, PODS, 3, COMMAND, SET_CONFIGURATION, &sbc);
        signal(
            &mut trace,
            answered_first,
            3,
            ACCEPT,
            SET_CONFIGURATION,
            &[],
        );
        let other = match answered_first {
            HOST => PODS,
            PODS => HOST,
        };
        signal(
            &mut trace,
            other,
            3,
            REJECT,
            SET_CONFIGURATION,
            &REFUSED_CONFIGURATION,
        );
        read(&trace).configuration
    };
    assert_eq!(crossed(PODS), Some(standard(CODEC_AAC, &AAC_BYTES)));
    assert_eq!(crossed(HOST), Some(standard(CODEC_SBC, &SBC_BYTES)));
}

#[test]
fn a_start_command_lets_the_source_begin_a_new_sequence() {
    let gaps = |trace: &Trace| -> u64 { read(trace).seconds.iter().map(|row| row.gaps).sum() };
    let mut trace = streaming();
    trace.send(HOST, MEDIA_CIDS, &rtp(100));
    trace.send(HOST, MEDIA_CIDS, &rtp(101));
    signal(&mut trace, HOST, 1, COMMAND, AVDTP_SUSPEND, &[FIRST_SEID]);
    signal(&mut trace, PODS, 1, ACCEPT, AVDTP_SUSPEND, &[]);
    trace.send(HOST, MEDIA_CIDS, &rtp(500));
    signal(&mut trace, HOST, 2, COMMAND, AVDTP_START, &[FIRST_SEID]);
    trace.send(HOST, MEDIA_CIDS, &rtp(0));
    trace.send(HOST, MEDIA_CIDS, &rtp(1));
    trace.send(HOST, MEDIA_CIDS, &rtp(3));
    assert_eq!(gaps(&trace), 2);

    let mut elsewhere = streaming();
    elsewhere.send(HOST, MEDIA_CIDS, &rtp(100));
    elsewhere.on(KEYBOARD_HANDLE);
    elsewhere.open(HOST, AVDTP_PSM, SIGNALLING_CIDS);
    signal(&mut elsewhere, HOST, 2, COMMAND, AVDTP_START, &[FIRST_SEID]);
    elsewhere.on(PODS_HANDLE);
    elsewhere.send(HOST, MEDIA_CIDS, &rtp(0));
    assert_eq!(gaps(&elsewhere), 1);
    let starts = |handle: u16| -> Vec<u64> {
        let links = on(&elsewhere, handle);
        let of_link = |music: &Music| music.seconds.iter().map(|row| row.starts).sum();
        links.iter().map(of_link).collect()
    };
    assert_eq!(starts(PODS_HANDLE), [0]);
    assert_eq!(starts(KEYBOARD_HANDLE), [1]);
}

#[test]
fn a_start_counts_from_just_after_one_packet_up_to_the_next() {
    let gaps = |start_ms: u64| -> u64 {
        let mut trace = streaming();
        trace.at_ms(5_000);
        trace.send(HOST, MEDIA_CIDS, &rtp(100));
        trace.at_ms(start_ms);
        signal(&mut trace, HOST, 2, COMMAND, AVDTP_START, &[FIRST_SEID]);
        trace.at_ms(5_020);
        trace.send(HOST, MEDIA_CIDS, &rtp(0));
        read(&trace).seconds.iter().map(|row| row.gaps).sum()
    };
    assert_eq!(gaps(4_999), 1, "a start before the first packet");
    assert_eq!(gaps(5_000), 1, "a start stamped with the first packet");
    assert_eq!(gaps(5_001), 0, "a start just after the first packet");
    assert_eq!(gaps(5_020), 0, "a start stamped with the second packet");
    assert_eq!(gaps(5_021), 1, "a start after the second packet");
}

#[test]
fn a_clock_that_steps_back_between_two_packets_hides_no_gap() {
    let mut trace = streaming();
    trace.at_ms(5_000);
    trace.send(HOST, MEDIA_CIDS, &rtp(100));
    trace.at_ms(4_000);
    signal(&mut trace, HOST, 2, COMMAND, AVDTP_START, &[FIRST_SEID]);
    trace.at_ms(3_000);
    trace.send(HOST, MEDIA_CIDS, &rtp(0));
    let rows: Vec<_> = read(&trace)
        .seconds
        .iter()
        .map(|row| {
            (
                row.at_s - START_SECOND,
                row.packets,
                row.gaps,
                row.largest_interval_ns,
            )
        })
        .collect();
    let two_seconds = 2 * NANOS_PER_SECOND;
    assert_eq!(rows, [(3, 1, 1, 0), (4, 0, 0, 0), (5, 1, 0, two_seconds)]);
}

#[test]
fn a_gap_is_counted_in_the_second_of_the_packet_that_shows_it() {
    let mut trace = streaming();
    trace.at_ms(5_990);
    trace.send(HOST, MEDIA_CIDS, &rtp(100));
    trace.at_ms(6_010);
    trace.send(HOST, MEDIA_CIDS, &rtp(102));
    let rows: Vec<_> = read(&trace)
        .seconds
        .iter()
        .map(|row| (row.at_s - START_SECOND, row.gaps))
        .collect();
    assert_eq!(rows, [(5, 0), (6, 1)]);
}

#[test]
fn a_new_media_channel_starts_a_new_sequence() {
    let mut trace = streaming();
    trace.send(HOST, MEDIA_CIDS, &rtp(100));
    let mut closing = MEDIA_CIDS.1.to_le_bytes().to_vec();
    closing.extend(MEDIA_CIDS.0.to_le_bytes());
    trace.signal(HOST, DISCONNECTION_REQUEST, &closing);
    trace.open(HOST, AVDTP_PSM, THIRD_CIDS);
    trace.send(HOST, THIRD_CIDS, &rtp(0));
    trace.send(HOST, THIRD_CIDS, &rtp(1));
    let found = read(&trace);
    let rows = found.seconds;
    assert_eq!((rows[0].packets, rows[0].gaps), (3, 0));
    assert_eq!(found.unread.other_channels, 0);
}

#[test]
fn media_is_counted_per_second_with_its_largest_interval() {
    let rows = read(&session()).seconds;
    let packet = u64::try_from(RTP_HEADER_LEN + MEDIA_PAYLOAD_LEN).unwrap();
    let expected = [
        Second {
            at_s: START_SECOND + 1,
            starts: 1,
            ..Second::default()
        },
        Second {
            at_s: START_SECOND + 2,
            packets: 6,
            bytes: 6 * packet,
            gaps: 1,
            largest_interval_ns: 20_000_000,
            suspends: 1,
            starts: 0,
        },
        Second {
            at_s: START_SECOND + 3,
            packets: 2,
            bytes: 2 * packet,
            gaps: 0,
            largest_interval_ns: 1_100_000_000,
            suspends: 0,
            starts: 1,
        },
    ];
    assert_eq!(rows, expected);
}

#[test]
fn only_rtp_packets_the_host_sent_are_media() {
    let mut trace = streaming();
    trace.send(HOST, MEDIA_CIDS, &rtp(1));
    trace.send(PODS, MEDIA_CIDS, &rtp(9));
    trace.send(HOST, MEDIA_CIDS, &rtp(2)[..RTP_HEADER_LEN - 1]);
    for first_byte in [RTP_VERSION_1, RTP_VERSION_3] {
        let mut other_version = rtp(2);
        other_version[0] = first_byte;
        trace.send(HOST, MEDIA_CIDS, &other_version);
    }
    trace.send(HOST, MEDIA_CIDS, &rtp(2)[..RTP_HEADER_LEN]);
    let found = read(&trace);
    let rows = found.seconds;
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].packets, rows[0].gaps), (2, 0));
    let bytes = u64::try_from(2 * RTP_HEADER_LEN + MEDIA_PAYLOAD_LEN).unwrap();
    assert_eq!(rows[0].bytes, bytes);
    assert_eq!(found.unread.not_rtp, 3);
}

#[test]
fn channels_are_told_apart_by_the_order_they_open_in() {
    let sbc = media_codec(CODEC_SBC, &SBC_BYTES);
    let mut trace = streaming();
    trace.open(HOST, AVDTP_PSM, THIRD_CIDS);
    trace.send(HOST, THIRD_CIDS, &rtp(1));
    trace.send(PODS, THIRD_CIDS, &avdtp(1, ACCEPT, GET_CAPABILITIES, &sbc));
    let found = read(&trace);
    assert_eq!(found.seconds, []);
    assert_eq!(found.capabilities, []);
    assert_eq!(found.unread.other_channels, 1);

    trace.end_link(PODS_HANDLE);
    trace.open(HOST, AVDTP_PSM, THIRD_CIDS);
    trace.send(PODS, THIRD_CIDS, &avdtp(1, ACCEPT, GET_CAPABILITIES, &sbc));
    trace.open(HOST, AVDTP_PSM, MEDIA_CIDS);
    trace.send(HOST, MEDIA_CIDS, &rtp(1));
    let [ended, after] = <[Music; 2]>::try_from(on(&trace, PODS_HANDLE)).unwrap();
    assert_eq!(ended, found, "the link that ended keeps what it held");
    assert_eq!(after.capabilities, [standard(CODEC_SBC, &SBC_BYTES)]);
    assert_eq!(after.seconds.len(), 1);
    assert_eq!(after.unread.other_channels, 0);
}

#[test]
fn a_trace_with_no_avdtp_channel_has_no_rows() {
    let mut trace = Trace::new();
    trace.open(HOST, CONTROL_PSM, SIGNALLING_CIDS);
    trace.open(HOST, CONTROL_PSM, MEDIA_CIDS);
    trace.send(
        PODS,
        SIGNALLING_CIDS,
        &avdtp(
            1,
            ACCEPT,
            GET_CAPABILITIES,
            &media_codec(CODEC_SBC, &SBC_BYTES),
        ),
    );
    trace.send(HOST, MEDIA_CIDS, &rtp(1));
    let found = read(&trace);
    assert_eq!(found.capabilities, []);
    assert_eq!(found.configuration, None);
    assert_eq!(found.seconds, []);
    assert_eq!(found.unread, Unread::default());
}

proptest! {
    #[test]
    fn never_panics(
        packets in proptest::collection::vec(
            (any::<bool>(), any::<bool>(), proptest::collection::vec(any::<u8>(), 0..40)),
            0..48,
        ),
    ) {
        let mut trace = streaming();
        for (from_pods, on_media, packet) in &packets {
            let sender = if *from_pods { PODS } else { HOST };
            let cids = if *on_media { MEDIA_CIDS } else { SIGNALLING_CIDS };
            trace.send(sender, cids, packet);
        }
        let found = read(&trace);
        let unread = found.unread;
        let read = [found.capabilities.len(), found.seconds.len(), usize::from(found.configuration.is_some())];
        prop_assert!(read.into_iter().sum::<usize>() <= packets.len().saturating_mul(20));
        prop_assert!(unread.other_channels == 0);
    }

    #[test]
    fn never_panics_on_a_signal_with_a_known_header(
        message in 0_u8..4,
        id in prop_oneof![Just(GET_CAPABILITIES), Just(SET_CONFIGURATION), Just(GET_ALL_CAPABILITIES)],
        body in proptest::collection::vec(prop_oneof![Just(MEDIA_CODEC), Just(CODEC_VENDOR), Just(VENDOR_IDENTIFIERS_LEN), any::<u8>()], 0..24),
    ) {
        let mut trace = streaming();
        signal(&mut trace, PODS, 1, message, id, &body);
        signal(&mut trace, HOST, 1, ACCEPT, id, &[]);
        let found = read(&trace);
        let listed = [found.capabilities.len(), usize::from(found.configuration.is_some())];
        prop_assert!(listed.into_iter().sum::<usize>() <= body.len());
    }
}
