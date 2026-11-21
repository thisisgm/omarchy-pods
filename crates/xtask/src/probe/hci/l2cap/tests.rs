//! Reassembly and the signalling map, driven with traces the fixture builds.

use pods_proto::packets::{ATT_PSM, CONTROL_PSM};
use pods_proto::sdulog::{Direction, Record};

use super::super::fixture::{
    AVDTP_PSM, CONFIGURATION_REQUEST, CONNECTION_REQUEST, CONNECTION_RESPONSE, CONTINUATION,
    EVENT_DISCONNECTION_COMPLETE, KEYBOARD_HANDLE, PODS_HANDLE, SIGNALLING_CID, START_UNIX_MICROS,
    Trace, counting_bytes, mode_option, receiver_cid, start_flag,
};
use super::super::records;
use super::*;

const HOST: Direction = Direction::ToPods;
const PODS: Direction = Direction::FromPods;
/// The host's identifier, then the pods', of the channel most tests open first.
const FIRST: (u16, u16) = (0x0040, 0x0050);
/// The identifiers of a second channel.
const SECOND: (u16, u16) = (0x0041, 0x0051);
/// The L2CAP modes the tests ask for: basic, enhanced retransmission and streaming.
const BASIC: u8 = 0x00;
const ENHANCED_RETRANSMISSION: u8 = 0x03;
const STREAMING: u8 = 0x04;
/// `L2CAP_DISCONNECTION_REQ`.
const DISCONNECTION_REQUEST: u8 = 0x06;
/// The connection response result for a request still being worked on.
const PENDING: u16 = 0x0001;
/// The connection response result for a PSM nobody listens on.
const PSM_NOT_SUPPORTED: u16 = 0x0002;

fn read(trace: &Trace) -> Channels {
    channels(&records(&trace.bytes()).unwrap().kept)
}

/// The stamp of a record written `ms` milliseconds into a synthetic trace.
fn at_ns(ms: u64) -> u64 {
    START_UNIX_MICROS
        .saturating_add(ms.saturating_mul(1_000))
        .saturating_mul(1_000)
}

/// The first bytes of a PDU: its length, then the identifier of the receiving end.
fn pdu_header(len: u16, cid: u16) -> Vec<u8> {
    let mut header = len.to_le_bytes().to_vec();
    header.extend(cid.to_le_bytes());
    header
}

fn sdus(channel: &Channel) -> Vec<(Direction, Vec<u8>)> {
    channel
        .sdus
        .iter()
        .map(|record| (record.direction, record.sdu.clone()))
        .collect()
}

#[test]
fn acl_fragments_are_reassembled() {
    let from_pods = counting_bytes(600);
    let from_host = counting_bytes(300);
    let mut trace = Trace::new();
    trace.open(HOST, CONTROL_PSM, FIRST);
    trace.fragments(PODS, FIRST.0, &from_pods, &[200, 250]);
    trace.fragments(HOST, FIRST.1, &from_host, &[10, 3, 100]);
    trace.send(PODS, FIRST, &[0xaa]);
    let found = read(&trace);
    let expected = [
        Record {
            at_ns: at_ns(4),
            direction: PODS,
            sdu: from_pods.clone(),
        },
        Record {
            at_ns: at_ns(8),
            direction: HOST,
            sdu: from_host.clone(),
        },
        Record {
            at_ns: at_ns(9),
            direction: PODS,
            sdu: vec![0xaa],
        },
    ];
    assert_eq!(found.list[0].sdus, expected);
    let counts = Counts {
        acl_packets: 10,
        ..Counts::default()
    };
    assert_eq!(found.counts, counts);
}

#[test]
fn fragments_of_both_directions_and_two_links_interleave() {
    let mut trace = Trace::new();
    trace.open(HOST, CONTROL_PSM, FIRST);
    trace.on(KEYBOARD_HANDLE);
    trace.open(HOST, CONTROL_PSM, FIRST);
    let whole = |payload: &[u8], cid: u16| {
        let mut pdu = pdu_header(u16::try_from(payload.len()).unwrap(), cid);
        pdu.extend(payload);
        pdu
    };
    let pods_pdu = whole(&[1, 2, 3, 4, 5, 6], FIRST.0);
    let host_pdu = whole(&[7, 8, 9, 10], FIRST.1);
    let keyboard_pdu = whole(&[11, 12, 13, 14, 15], FIRST.0);
    trace.on(PODS_HANDLE);
    trace.acl(PODS, start_flag(PODS), &pods_pdu[..6]);
    trace.acl(HOST, start_flag(HOST), &host_pdu[..5]);
    trace.on(KEYBOARD_HANDLE);
    trace.acl(PODS, start_flag(PODS), &keyboard_pdu[..7]);
    trace.on(PODS_HANDLE);
    trace.acl(HOST, CONTINUATION, &host_pdu[5..]);
    trace.acl(PODS, CONTINUATION, &pods_pdu[6..]);
    trace.on(KEYBOARD_HANDLE);
    trace.acl(PODS, CONTINUATION, &keyboard_pdu[7..]);
    let found = read(&trace);
    assert_eq!(
        sdus(&found.list[0]),
        [(HOST, vec![7, 8, 9, 10]), (PODS, vec![1, 2, 3, 4, 5, 6])]
    );
    assert_eq!(sdus(&found.list[1]), [(PODS, vec![11, 12, 13, 14, 15])]);
    assert_eq!(found.list[1].handle, KEYBOARD_HANDLE);
    assert_eq!(found.counts.unfinished_pdus, 0);
}

#[test]
fn a_continuation_with_no_start_is_dropped_and_counted() {
    let mut trace = Trace::new();
    trace.open(HOST, CONTROL_PSM, FIRST);
    trace.acl(PODS, CONTINUATION, &[1, 2, 3, 4, 5, 6]);
    trace.acl(HOST, start_flag(HOST), &pdu_header(8, FIRST.1));
    trace.acl(PODS, CONTINUATION, &[0xee; 8]);
    trace.acl(HOST, CONTINUATION, &[0x11; 8]);
    trace.send(PODS, FIRST, &[0xaa]);
    trace.acl(PODS, CONTINUATION, &[7]);
    let found = read(&trace);
    assert_eq!(found.counts.continuations_without_start, 3);
    assert_eq!(
        sdus(&found.list[0]),
        [(HOST, vec![0x11; 8]), (PODS, vec![0xaa])]
    );
    assert_eq!(found.counts.malformed_pdus, 0);
    assert_eq!(found.counts.unfinished_pdus, 0);
}

#[test]
fn a_start_that_never_completes_is_counted() {
    let mut trace = Trace::new();
    trace.open(HOST, CONTROL_PSM, FIRST);
    let mut start = pdu_header(500, FIRST.0);
    start.extend([0x77; 10]);
    trace.acl(PODS, start_flag(PODS), &start);
    trace.send(PODS, FIRST, &[0xaa]);
    trace.acl(PODS, start_flag(PODS), &start);
    trace.acl(PODS, CONTINUATION, &[0x77; 100]);
    trace.acl(HOST, start_flag(HOST), &pdu_header(65_535, FIRST.1));
    let found = read(&trace);
    assert_eq!(found.counts.unfinished_pdus, 3);
    assert_eq!(sdus(&found.list[0]), [(PODS, vec![0xaa])]);
    assert_eq!(found.counts.malformed_pdus, 0);
}

#[test]
fn a_pdu_longer_than_its_length_or_shorter_than_its_header_is_counted() {
    let mut trace = Trace::new();
    trace.open(HOST, CONTROL_PSM, FIRST);
    let mut too_long = pdu_header(2, FIRST.0);
    too_long.extend([1, 2, 3]);
    trace.acl(PODS, start_flag(PODS), &too_long);
    trace.acl(PODS, start_flag(PODS), &[0x04, 0x00, 0x40]);
    trace.acl(PODS, start_flag(PODS), &[]);
    let mut start = pdu_header(4, FIRST.0);
    start.extend([1, 2]);
    trace.acl(PODS, start_flag(PODS), &start);
    trace.acl(PODS, CONTINUATION, &[3, 4, 5]);
    trace.acl(PODS, CONTINUATION, &[6]);
    let found = read(&trace);
    assert_eq!(found.counts.malformed_pdus, 4);
    assert_eq!(found.counts.continuations_without_start, 1);
    assert_eq!(found.list[0].sdus, []);
}

#[test]
fn an_acl_packet_with_a_wrong_length_is_counted() {
    let mut trace = Trace::new();
    trace.open(HOST, CONTROL_PSM, FIRST);
    let mut whole = pdu_header(1, FIRST.0);
    whole.push(0xaa);
    let mut packet = (PODS_HANDLE | (0b10 << 12)).to_le_bytes().to_vec();
    packet.extend(6_u16.to_le_bytes());
    packet.extend(&whole);
    for bad in [&packet[..], &packet[..3], &[][..]] {
        trace.record(5, bad);
    }
    let found = read(&trace);
    assert_eq!(found.counts.malformed_acl_packets, 3);
    assert_eq!(found.counts.acl_packets, 5);
    assert_eq!(found.list[0].sdus, []);
}

#[test]
fn every_connection_request_is_listed_with_its_side_and_result() {
    let mut trace = Trace::new();
    trace.open(HOST, CONTROL_PSM, FIRST);
    trace.open(PODS, ATT_PSM, SECOND);
    trace.connect(HOST, AVDTP_PSM, (0x0042, 0x0000), PSM_NOT_SUPPORTED);
    trace.connect(PODS, 0x0003, (0x0000, 0x0053), PENDING);
    trace.signal(HOST, CONNECTION_REQUEST, &[0x11, 0x00, 0x44, 0x00]);
    let found = read(&trace);
    let request = |ms, direction, psm, source_cid, result, channel| Request {
        at_ns: at_ns(ms),
        handle: PODS_HANDLE,
        direction,
        psm,
        source_cid,
        result,
        channel,
    };
    assert_eq!(
        found.requests,
        [
            request(0, HOST, CONTROL_PSM, 0x0040, Some(RESULT_SUCCESS), Some(0)),
            request(2, PODS, ATT_PSM, 0x0051, Some(RESULT_SUCCESS), Some(1)),
            request(4, HOST, AVDTP_PSM, 0x0042, Some(PSM_NOT_SUPPORTED), None),
            request(6, PODS, 0x0003, 0x0053, Some(PENDING), None),
            request(8, HOST, 0x0011, 0x0044, None, None),
        ]
    );
    assert_eq!(found.list.len(), 2);
    assert_eq!(found.list[0].psm, CONTROL_PSM);
    assert_eq!((found.list[0].host_cid, found.list[0].pods_cid), FIRST);
    assert_eq!(found.list[0].opened, 1);
    assert_eq!(found.list[1].psm, ATT_PSM);
    assert_eq!((found.list[1].host_cid, found.list[1].pods_cid), SECOND);
    assert_eq!(found.counts.malformed_signals, 0);
}

#[test]
fn a_pending_response_is_followed_by_the_final_one() {
    let mut trace = Trace::new();
    trace.connect(HOST, CONTROL_PSM, FIRST, PENDING);
    let mut granted = FIRST.1.to_le_bytes().to_vec();
    granted.extend(FIRST.0.to_le_bytes());
    granted.extend([0x00; 4]);
    trace.signal(PODS, CONNECTION_RESPONSE, &granted);
    trace.signal(HOST, CONNECTION_RESPONSE, &granted);
    trace.send(PODS, FIRST, &[0xaa]);
    let found = read(&trace);
    assert_eq!(found.requests[0].result, Some(RESULT_SUCCESS));
    assert_eq!(found.requests[0].channel, Some(0));
    assert_eq!(sdus(&found.list[0]), [(PODS, vec![0xaa])]);
    assert_eq!(found.counts.responses_without_request, 1);
}

#[test]
fn the_mtu_option_each_side_sends_is_kept() {
    let mut trace = Trace::new();
    trace.open(PODS, ATT_PSM, FIRST);
    assert_eq!(read(&trace).list[0].host_mtu, None);
    trace.offer_mtu(HOST, FIRST, 672);
    trace.offer_mtu(PODS, FIRST, 185);
    let found = read(&trace);
    assert_eq!(found.list[0].host_mtu, Some(672));
    assert_eq!(found.list[0].pods_mtu, Some(185));

    let mut request = FIRST.0.to_le_bytes().to_vec();
    request.extend([0x00, 0x00]);
    request.extend([0x02, 0x02, 0xff, 0xff]);
    request.extend([0x81, 0x02, 0x00, 0x04]);
    request.extend([0x04, 0x09, 0x00]);
    trace.signal(PODS, CONFIGURATION_REQUEST, &request);
    let found = read(&trace);
    assert_eq!(found.list[0].pods_mtu, Some(1024));
    assert_eq!(found.list[0].host_mtu, Some(672));
    assert_eq!(found.counts.malformed_signals, 1);

    let mut odd = FIRST.0.to_le_bytes().to_vec();
    odd.extend([0x00, 0x00, 0x01, 0x03, 0x01, 0x02, 0x03]);
    trace.signal(PODS, CONFIGURATION_REQUEST, &odd);
    trace.offer_mtu(PODS, (0x0049, 0x0059), 48);
    trace.signal(PODS, CONFIGURATION_REQUEST, &[0x40, 0x00, 0x00]);
    let found = read(&trace);
    assert_eq!(found.list[0].pods_mtu, Some(1024));
    assert_eq!(found.counts.malformed_signals, 2);
}

#[test]
fn a_channel_a_side_asks_another_mode_for_keeps_no_pdus() {
    let mut trace = Trace::new();
    trace.open(HOST, CONTROL_PSM, FIRST);
    trace.open(PODS, ATT_PSM, SECOND);
    trace.offer_mode(HOST, FIRST, BASIC);
    trace.offer_mode(PODS, FIRST, BASIC);
    trace.offer_mode(HOST, SECOND, BASIC);
    trace.offer_mode(PODS, SECOND, ENHANCED_RETRANSMISSION);
    trace.offer_mtu(PODS, SECOND, 185);
    trace.send(PODS, FIRST, &[1]);
    trace.send(PODS, SECOND, &[2]);
    trace.send(HOST, SECOND, &[3]);
    let found = read(&trace);
    let modes = |channel: &Channel| (channel.host_mode, channel.pods_mode, channel.is_basic());
    assert_eq!(modes(&found.list[0]), (Some(BASIC), Some(BASIC), true));
    assert_eq!(sdus(&found.list[0]), [(PODS, vec![1])]);
    assert_eq!(found.list[0].not_basic_pdus, 0);
    let asked = (Some(BASIC), Some(ENHANCED_RETRANSMISSION), false);
    assert_eq!(modes(&found.list[1]), asked);
    assert_eq!(found.list[1].sdus, []);
    assert_eq!(found.list[1].not_basic_pdus, 2);
    assert_eq!(found.list[1].pods_mtu, Some(185));
    assert_eq!(found.unmapped, []);

    trace.offer_mode(PODS, SECOND, BASIC);
    trace.send(PODS, SECOND, &[4]);
    let found = read(&trace);
    assert_eq!(modes(&found.list[1]), (Some(BASIC), Some(BASIC), true));
    assert_eq!(sdus(&found.list[1]), [(PODS, vec![4])]);
    assert_eq!(found.list[1].not_basic_pdus, 2);
}

#[test]
fn the_mode_is_read_from_a_whole_option_of_either_side_and_no_other() {
    let configured = |sender: Direction, options: &[u8]| {
        let mut trace = Trace::new();
        trace.open(HOST, ATT_PSM, FIRST);
        let mut request = receiver_cid(sender, FIRST).to_le_bytes().to_vec();
        request.extend([0x00, 0x00]);
        request.extend(options);
        trace.signal(sender, CONFIGURATION_REQUEST, &request);
        read(&trace).list.remove(0)
    };
    let asked = |sender: Direction, options: &[u8]| {
        let channel = configured(sender, options);
        (channel.host_mode, channel.pods_mode, channel.is_basic())
    };
    let streaming = mode_option(STREAMING);
    assert_eq!(asked(HOST, &streaming), (Some(STREAMING), None, false));
    assert_eq!(asked(PODS, &streaming), (None, Some(STREAMING), false));
    assert_eq!(asked(PODS, &[]), (None, None, true));

    let mut after_an_mtu = vec![0x01, 0x02, 0xa0, 0x02];
    after_an_mtu.extend(&streaming);
    let both = configured(PODS, &after_an_mtu);
    assert_eq!(
        (both.pods_mtu, both.pods_mode),
        (Some(672), Some(STREAMING))
    );
    let mut hinted = streaming.clone();
    hinted[0] |= 0x80;
    assert_eq!(asked(PODS, &hinted), (None, Some(STREAMING), false));

    let mut one_byte_short = streaming.clone();
    one_byte_short[1] = 8;
    one_byte_short.pop();
    assert_eq!(asked(PODS, &one_byte_short), (None, None, true));
    let mut one_byte_long = streaming.clone();
    one_byte_long[1] = 10;
    one_byte_long.push(0x00);
    assert_eq!(asked(PODS, &one_byte_long), (None, None, true));
    let mut another_option = streaming;
    another_option[0] = 0x05;
    assert_eq!(asked(PODS, &another_option), (None, None, true));
}

#[test]
fn two_channels_on_one_psm_are_kept_apart() {
    let mut trace = Trace::new();
    trace.open(HOST, CONTROL_PSM, FIRST);
    trace.open(HOST, CONTROL_PSM, SECOND);
    trace.send(HOST, FIRST, &[1]);
    trace.send(HOST, SECOND, &[2]);
    trace.send(PODS, SECOND, &[3]);
    trace.send(PODS, FIRST, &[4]);
    let mut closing = FIRST.1.to_le_bytes().to_vec();
    closing.extend(FIRST.0.to_le_bytes());
    trace.signal(HOST, DISCONNECTION_REQUEST, &closing);
    trace.send(PODS, FIRST, &[5]);
    trace.send(PODS, SECOND, &[6]);
    let found = read(&trace);
    assert_eq!(found.list.len(), 2);
    assert!(found.list.iter().all(|channel| channel.psm == CONTROL_PSM));
    assert_eq!(sdus(&found.list[0]), [(HOST, vec![1]), (PODS, vec![4])]);
    assert_eq!(
        sdus(&found.list[1]),
        [(HOST, vec![2]), (PODS, vec![3]), (PODS, vec![6])]
    );
    assert_eq!(found.list[0].closed, Some(8));
    assert_eq!(found.list[1].closed, None);
    let unmapped = Unmapped {
        handle: PODS_HANDLE,
        cid: FIRST.0,
        direction: PODS,
        pdus: 1,
    };
    assert_eq!(found.unmapped, [unmapped]);
}

#[test]
fn a_disconnection_request_that_names_the_wrong_end_closes_nothing() {
    let mut trace = Trace::new();
    trace.open(HOST, CONTROL_PSM, FIRST);
    let mut wrong = FIRST.1.to_le_bytes().to_vec();
    wrong.extend(SECOND.0.to_le_bytes());
    trace.signal(HOST, DISCONNECTION_REQUEST, &wrong);
    trace.signal(HOST, DISCONNECTION_REQUEST, &[0x50, 0x00, 0x40]);
    trace.send(PODS, FIRST, &[1]);
    let found = read(&trace);
    assert_eq!(found.list[0].closed, None);
    assert_eq!(sdus(&found.list[0]), [(PODS, vec![1])]);
    assert_eq!(found.counts.malformed_signals, 1);
}

#[test]
fn a_trace_that_starts_after_the_connection_reports_unmapped_channels() {
    let mut trace = Trace::new();
    for _ in 0..3 {
        trace.pdu(HOST, 0x0050, &[1, 2, 3]);
    }
    for _ in 0..2 {
        trace.pdu(PODS, 0x0040, &[4, 5]);
    }
    trace.pdu(PODS, 0x0050, &[6]);
    let mut granted = FIRST.1.to_le_bytes().to_vec();
    granted.extend(FIRST.0.to_le_bytes());
    granted.extend([0x00; 4]);
    trace.signal(PODS, CONNECTION_RESPONSE, &granted);
    trace.pdu(HOST, 0x0050, &[7]);
    let found = read(&trace);
    assert_eq!(found.list, []);
    assert_eq!(found.requests, []);
    let unmapped = |cid, direction, pdus| Unmapped {
        handle: PODS_HANDLE,
        cid,
        direction,
        pdus,
    };
    assert_eq!(
        found.unmapped,
        [
            unmapped(0x0050, HOST, 4),
            unmapped(0x0040, PODS, 2),
            unmapped(0x0050, PODS, 1),
        ]
    );
    assert_eq!(found.counts.responses_without_request, 1);
}

#[test]
fn channels_end_with_their_link_and_identifiers_belong_to_one_link() {
    let mut trace = Trace::new();
    trace.open(HOST, CONTROL_PSM, FIRST);
    trace.on(KEYBOARD_HANDLE);
    trace.open(HOST, 0x0013, FIRST);
    trace.send(PODS, FIRST, &[0x6b]);
    trace.on(PODS_HANDLE);
    trace.send(PODS, FIRST, &[1]);
    let mut start = pdu_header(9, FIRST.0);
    start.push(0x77);
    trace.acl(PODS, start_flag(PODS), &start);
    let mut ended = vec![0x00];
    ended.extend(PODS_HANDLE.to_le_bytes());
    ended.push(0x13);
    trace.event(EVENT_DISCONNECTION_COMPLETE, &ended);
    trace.send(PODS, FIRST, &[2]);
    trace.acl(PODS, CONTINUATION, &[0x77; 8]);
    trace.open(HOST, ATT_PSM, FIRST);
    trace.send(PODS, FIRST, &[3]);
    trace.on(KEYBOARD_HANDLE);
    trace.send(PODS, FIRST, &[0x6c]);
    let found = read(&trace);
    let seen: Vec<_> = found
        .list
        .iter()
        .map(|channel| (channel.handle, channel.psm, channel.closed, sdus(channel)))
        .collect();
    assert_eq!(
        seen,
        [
            (PODS_HANDLE, CONTROL_PSM, Some(7), vec![(PODS, vec![1])]),
            (
                KEYBOARD_HANDLE,
                0x0013,
                None,
                vec![(PODS, vec![0x6b]), (PODS, vec![0x6c])]
            ),
            (PODS_HANDLE, ATT_PSM, None, vec![(PODS, vec![3])]),
        ]
    );
    assert_eq!(found.unmapped.len(), 1);
    assert_eq!(found.counts.unfinished_pdus, 1);
    assert_eq!(found.counts.continuations_without_start, 1);
}

#[test]
fn a_reused_identifier_closes_the_channel_that_held_it() {
    let mut trace = Trace::new();
    trace.open(HOST, CONTROL_PSM, FIRST);
    trace.send(PODS, FIRST, &[1]);
    trace.open(HOST, ATT_PSM, (FIRST.0, SECOND.1));
    trace.send(PODS, FIRST, &[2]);
    trace.send(HOST, FIRST, &[3]);
    trace.send(HOST, (FIRST.0, SECOND.1), &[4]);
    let found = read(&trace);
    assert_eq!(found.list[0].closed, Some(4));
    assert_eq!(sdus(&found.list[0]), [(PODS, vec![1])]);
    assert_eq!(sdus(&found.list[1]), [(PODS, vec![2]), (HOST, vec![4])]);
    let stale = Unmapped {
        handle: PODS_HANDLE,
        cid: FIRST.1,
        direction: HOST,
        pdus: 1,
    };
    assert_eq!(found.unmapped, [stale]);
}

#[test]
fn fixed_channels_are_counted_and_one_pdu_may_hold_several_commands() {
    let mut trace = Trace::new();
    trace.pdu(PODS, 0x0002, &[1, 2, 3]);
    trace.pdu(HOST, 0x003f, &[1]);
    let mut commands = vec![CONNECTION_REQUEST, 0x01, 0x04, 0x00, 0x01, 0x10, 0x40, 0x00];
    commands.extend([0x0a, 0x02, 0x02, 0x00, 0x02, 0x00]);
    commands.extend([CONNECTION_REQUEST, 0x03, 0x04, 0x00, 0x1f, 0x00, 0x41, 0x00]);
    trace.pdu(HOST, SIGNALLING_CID, &commands);
    let mut cut = vec![CONNECTION_REQUEST, 0x04, 0x04, 0x00, 0x19, 0x00, 0x42, 0x00];
    cut.extend([CONNECTION_REQUEST, 0x05, 0x04, 0x00, 0x19]);
    trace.pdu(HOST, SIGNALLING_CID, &cut);
    trace.signal(HOST, CONNECTION_REQUEST, &[0x19, 0x00, 0x43]);
    trace.signal(
        PODS,
        CONNECTION_RESPONSE,
        &[0x50, 0x00, 0x40, 0x00, 0x00, 0x00],
    );
    let found = read(&trace);
    assert_eq!(found.counts.fixed_channel_pdus, 2);
    let psms: Vec<u16> = found.requests.iter().map(|request| request.psm).collect();
    assert_eq!(psms, [CONTROL_PSM, ATT_PSM, AVDTP_PSM]);
    assert_eq!(found.counts.malformed_signals, 3);
    assert_eq!(found.list, []);
}

#[test]
fn the_limits_count_what_they_refuse() {
    let mut trace = Trace::new();
    for cid in 0..u16::try_from(MAX_REQUESTS + 3).unwrap() {
        let mut request = CONTROL_PSM.to_le_bytes().to_vec();
        request.extend((0x0040 + cid).to_le_bytes());
        trace.signal(HOST, CONNECTION_REQUEST, &request);
    }
    for cid in 0..u16::try_from(MAX_UNMAPPED + 2).unwrap() {
        trace.pdu(PODS, 0x1000 + cid, &[1]);
        trace.pdu(PODS, 0x1000 + cid, &[2]);
    }
    let found = read(&trace);
    assert_eq!(found.requests.len(), MAX_REQUESTS);
    assert_eq!(found.counts.requests_over_limit, 3);
    assert_eq!(found.unmapped.len(), MAX_UNMAPPED);
    assert!(found.unmapped.iter().all(|unmapped| unmapped.pdus == 2));
    assert_eq!(found.counts.unmapped_over_limit, 4);
}
