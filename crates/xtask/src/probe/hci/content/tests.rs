//! The mapping by content, driven with `PacketLogger` traces the fixture builds.

use super::super::fixture::{
    ATT_CIDS, CONTROL_CIDS, KEYBOARD_HANDLE, MEDIA_CIDS, PODS_HANDLE, SIGNALLING_CIDS, Trace,
    audio_notification, mac_accessory_packets, mac_att_packets, mac_session,
};
use super::super::{channels, channels_by_content, records_pklg};
use super::*;

const HOST: Direction = Direction::ToPods;
const PODS: Direction = Direction::FromPods;
/// An accessory packet: the header, an opcode and one byte.
const ACCESSORY_PACKET: [u8; 7] = [0x04, 0x00, 0x04, 0x00, 0x4b, 0x00, 0x03];
/// A packet no content rule reads.
const OTHER_PACKET: [u8; 3] = [0x08, 0x09, 0x0a];
/// An audio notification cut inside its prefix, which the dictation parser refuses.
const CUT_AUDIO: [u8; 5] = [0x1b, 0x0d, 0x00, 0x01, 0x00];
/// The first dynamic channel identifier.
const FIRST_CID: u16 = 0x0040;

fn read(trace: &Trace) -> Channels {
    channels_by_content(&records_pklg(&trace.bytes()).unwrap())
}

fn half(cid: u16, direction: Direction, pdus: u64) -> Half {
    Half {
        cid,
        direction,
        pdus,
    }
}

fn unmapped(handle: u16, cid: u16, direction: Direction, pdus: u64) -> Unmapped {
    Unmapped {
        handle,
        cid,
        direction,
        pdus,
    }
}

/// The direction and bytes of every PDU one mapped kind holds.
fn packets(carried: &Carried) -> Vec<(Direction, Vec<u8>)> {
    let sdus = carried.sdus.iter();
    sdus.map(|sdu| (sdu.direction, sdu.sdu.clone())).collect()
}

#[test]
fn a_trace_without_connection_requests_is_mapped_by_content() {
    let trace = mac_session();
    let found = read(&trace);
    assert_eq!(found.list, []);
    assert_eq!(found.requests, []);
    let [accessory, attributes] = found.by_content.as_slice() else {
        panic!("{:?}", found.by_content);
    };
    assert_eq!(
        (accessory.handle, accessory.link, accessory.carries),
        (PODS_HANDLE, 1, Carries::Accessory)
    );
    assert_eq!(
        accessory.halves,
        [half(CONTROL_CIDS.1, HOST, 3), half(CONTROL_CIDS.0, PODS, 2)]
    );
    assert_eq!(packets(accessory), mac_accessory_packets());
    assert_eq!(
        (attributes.handle, attributes.link, attributes.carries),
        (PODS_HANDLE, 1, Carries::Attributes)
    );
    assert_eq!(
        attributes.halves,
        [half(ATT_CIDS.0, PODS, 5), half(ATT_CIDS.1, HOST, 3)]
    );
    assert_eq!(packets(attributes), mac_att_packets());
    assert!(attributes.sdus.is_sorted_by_key(|sdu| sdu.at_ns));
    assert_eq!(
        found.unmapped,
        [
            unmapped(KEYBOARD_HANDLE, CONTROL_CIDS.0, PODS, 1),
            unmapped(PODS_HANDLE, SIGNALLING_CIDS.1, HOST, 1),
            unmapped(PODS_HANDLE, MEDIA_CIDS.1, HOST, 2),
        ]
    );

    // The signalling alone maps nothing of such a trace and keeps none of its packets.
    let by_signalling = channels(&records_pklg(&trace.bytes()).unwrap());
    assert_eq!(by_signalling.by_content, []);
    assert_eq!(by_signalling.unmapped.len(), 7);
    assert_eq!(by_signalling.counts, found.counts);
}

#[test]
fn a_channel_that_matches_nothing_is_unmapped_not_guessed() {
    let mut trace = Trace::packet_logger();
    trace.send(HOST, CONTROL_CIDS, &OTHER_PACKET);
    trace.send(HOST, CONTROL_CIDS, &ACCESSORY_PACKET);
    trace.send(PODS, CONTROL_CIDS, &ACCESSORY_PACKET[..3]);
    trace.send(PODS, ATT_CIDS, &dictation::START);
    trace.send(PODS, ATT_CIDS, &dictation::STARTED);
    trace.send(PODS, ATT_CIDS, &CUT_AUDIO);
    trace.send(HOST, ATT_CIDS, &audio_notification(0));
    trace.send(HOST, ATT_CIDS, &dictation::STOP);
    let found = read(&trace);
    assert_eq!(found.by_content, []);
    assert_eq!(
        found.unmapped,
        [
            unmapped(PODS_HANDLE, CONTROL_CIDS.1, HOST, 2),
            unmapped(PODS_HANDLE, CONTROL_CIDS.0, PODS, 1),
            unmapped(PODS_HANDLE, ATT_CIDS.0, PODS, 3),
            unmapped(PODS_HANDLE, ATT_CIDS.1, HOST, 2),
        ]
    );
}

#[test]
fn halves_past_the_limit_are_counted_and_keep_no_packet() {
    let over = 3_u16;
    let listed = u16::try_from(MAX_UNMAPPED).unwrap();
    let mut trace = Trace::packet_logger();
    for cid in (FIRST_CID..).take(MAX_UNMAPPED + usize::from(over)) {
        trace.pdu(HOST, cid, &ACCESSORY_PACKET);
        trace.pdu(HOST, cid, &OTHER_PACKET);
    }
    let found = read(&trace);
    assert_eq!(found.counts.unmapped_over_limit, u64::from(over) * 2);
    let [accessory] = found.by_content.as_slice() else {
        panic!("{} kinds", found.by_content.len());
    };
    assert_eq!(accessory.halves.len(), MAX_UNMAPPED);
    assert_eq!(
        accessory.halves.last(),
        Some(&half(FIRST_CID + listed - 1, HOST, 2))
    );
    assert_eq!(accessory.sdus.len(), MAX_UNMAPPED * 2);
}
