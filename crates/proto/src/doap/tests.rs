//! Support, not proof: the arm against a table built here from the specification's layouts and the service's published identifiers.

use proptest::prelude::*;

use super::*;
use crate::att;
use crate::dictation::{DictationPacket, Event, STARTED, STOPPED};
use crate::error::{ParseError, Parser};
use crate::fixture::hex;

mod arming;

/// The six identifiers as a UUID is written, most significant octet first, in the order of `Handles`.
const SERVICE: [&str; 6] = [
    "f195b8fba9e24401858b2f87a06928a8",
    "e1f9b8357e47413daf94c68e574b8f7e",
    "a08ce5ef698a42a2b9807f3ac00b3845",
    "6288ea2d7b8947ad890b9fa6bf3cfc58",
    "3f1c161d6473474691f56d27610780c6",
    "c7c6947d31654bcb8eafb328896cb531",
];
/// Stands in for another vendor characteristic; no real identifier is a run of counting bytes.
const FAKE_LONG_UUID: [u8; 16] = [
    0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd, 0xfe, 0xff,
];
/// What the pods answered the codec read with.
const CODECS: &str = "01 01 00 04 07 02 01 01";
/// How far the second table stands from the first.
const SHIFT: u16 = 0x20;
/// Two declarations with 128 bit identifiers fill one reply at the default MTU of 48.
const DECLARATIONS_A_REPLY: usize = 2;
/// Eleven pairs with 16 bit identifiers fill one reply at the default MTU of 48.
const SHORT_PAIRS_A_REPLY: usize = 11;
/// `ATT_WRITE_NOT_PERMITTED` (Bluetooth Core Specification 6.1, Vol 3, Part F, 3.4.1.1).
const WRITE_NOT_PERMITTED: u8 = 0x03;
/// More steps than any arm takes, so a test of a loop ends.
const STEP_LIMIT: usize = 1000;
/// The longest packet the property test feeds, one receive buffer.
const LONGEST_PACKET: usize = 512;
/// A declaration with a 128 bit identifier: its handle, the property byte, the value handle and the identifier.
const LONG_DECLARATION_LEN: u8 = 21;
/// The type a request for declarations ends with, as the wire holds it.
const DECLARATION_TYPE: [u8; 2] = att::CHARACTERISTIC_DECLARATION.to_le_bytes();
/// The four descriptor writes and the codec read, every request an arm sends after its pass.
const REQUESTS_AFTER_THE_PASS: u32 = 5;
/// The most replies that continue the walk a generated case opens with, twice what an arm may ask.
const CONTINUED_BOUND: u32 = ARM_MAX_REQUESTS.saturating_mul(2);
/// One more than the most packets a generated case feeds after those.
const ARRIVED_BOUND: usize = 300;
/// One more than the longest generated value, longer than every value seen.
const VALUE_BOUND: usize = 24;
/// One more than the longest generated codec list, past the eight bytes of the one seen.
const CODEC_LIST_BOUND: usize = 12;
/// A table whose audio, event and stop characteristics share one handle, which the parser must read without a fault too.
const ONE_HANDLE_FOR_THREE: Handles = Handles {
    send_data: Handles::OBSERVED.event,
    stop: Handles::OBSERVED.event,
    ..Handles::OBSERVED
};

// Sample input: `f195...28a8` gives the sixteen octets least significant first, as the wire holds them.
fn wire(text: &str) -> [u8; 16] {
    u128::from_str_radix(text, 16).unwrap().to_le_bytes()
}

/// A reply that lists declarations with 128 bit identifiers: declaration handle, value handle and identifier of each.
fn declarations_reply(listed: &[(u16, u16, [u8; 16])]) -> Vec<u8> {
    let mut reply = vec![att::READ_BY_TYPE_RESPONSE, LONG_DECLARATION_LEN];
    for (at, value, uuid) in listed {
        reply.extend(at.to_le_bytes());
        reply.push(att::PROPERTY_NOTIFY);
        reply.extend(value.to_le_bytes());
        reply.extend(uuid);
    }
    reply
}

fn read_response(value: &[u8]) -> Vec<u8> {
    [&[att::READ_RESPONSE][..], value].concat()
}

/// The reply of pods whose table has a declaration at every handle, so no walk reaches its end.
fn endless(request: &[u8]) -> Vec<u8> {
    let [att::READ_BY_TYPE_REQUEST, low, high, ..] = *request else {
        panic!("the walk sent {request:02x?}");
    };
    let at = u16::from_le_bytes([low, high]);
    declarations_reply(&[(at, at, FAKE_LONG_UUID)])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Short(u16),
    Long([u8; 16]),
}

/// A stand in for the pods' attribute server: it answers each request of the arm from its table.
#[derive(Debug, Clone)]
struct Pods {
    /// Declaration handle, value handle and identifier, in rising order.
    declarations: Vec<(u16, u16, [u8; 16])>,
    descriptors: Vec<u16>,
    codecs: Vec<u8>,
    /// The code every Write Request is refused with.
    refuse_writes: Option<u8>,
}

impl Pods {
    /// Each characteristic one handle after its declaration, as the pods lay them out.
    fn at(handles: &Handles) -> Self {
        let values = [
            handles.supported_codecs,
            handles.select_codec,
            handles.start,
            handles.stop,
            handles.send_data,
            handles.event,
        ];
        let declare = |(value, uuid): (u16, &str)| (value.wrapping_sub(1), value, wire(uuid));
        Self {
            declarations: values.into_iter().zip(SERVICE).map(declare).collect(),
            descriptors: handles.cccds.to_vec(),
            codecs: hex(CODECS),
            refuse_writes: None,
        }
    }

    /// Where the Supported Codec characteristic holds its value, the only handle a host reads.
    fn codec_list_at(&self) -> Option<u16> {
        let [supported_codecs, ..] = SERVICE;
        let listed = wire(supported_codecs);
        let found = self.declarations.iter().find(|(.., uuid)| *uuid == listed);
        found.map(|(_, value, _)| *value)
    }

    fn attributes(&self) -> Vec<(u16, Kind)> {
        let declared = self.declarations.iter().flat_map(|(at, value, uuid)| {
            let declaration = Kind::Short(att::CHARACTERISTIC_DECLARATION);
            [(*at, declaration), (*value, Kind::Long(*uuid))]
        });
        let configuration = Kind::Short(att::CLIENT_CHARACTERISTIC_CONFIGURATION);
        let described = self.descriptors.iter().map(|at| (*at, configuration));
        let mut all: Vec<_> = declared.chain(described).collect();
        all.sort_by_key(|(at, _)| *at);
        all
    }

    fn answer(&self, request: &[u8]) -> Vec<u8> {
        let not_found = |opcode: u8, start: u16| {
            att::error_response(opcode, start, att::ATTRIBUTE_NOT_FOUND).to_vec()
        };
        match *request {
            [att::READ_BY_TYPE_REQUEST, s0, s1, e0, e1, t0, t1] if [t0, t1] == DECLARATION_TYPE => {
                let asked = u16::from_le_bytes([s0, s1])..=u16::from_le_bytes([e0, e1]);
                let listed = self.declarations.iter().copied();
                let listed = listed.filter(|(at, ..)| asked.contains(at));
                let listed: Vec<_> = listed.take(DECLARATIONS_A_REPLY).collect();
                if listed.is_empty() {
                    return not_found(att::READ_BY_TYPE_REQUEST, *asked.start());
                }
                declarations_reply(&listed)
            }
            [att::FIND_INFORMATION_REQUEST, s0, s1, e0, e1] => {
                let asked = u16::from_le_bytes([s0, s1])..=u16::from_le_bytes([e0, e1]);
                let attributes = self.attributes();
                let listed = attributes.iter().filter(|(at, _)| asked.contains(at));
                let mut listed = listed.peekable();
                match listed.peek() {
                    None => not_found(att::FIND_INFORMATION_REQUEST, *asked.start()),
                    Some((at, Kind::Long(uuid))) => {
                        let mut reply = vec![att::FIND_INFORMATION_RESPONSE, att::FORMAT_LONG];
                        reply.extend(at.to_le_bytes());
                        reply.extend(uuid);
                        reply
                    }
                    // A reply holds identifiers of one width, so it ends at the first 128 bit one.
                    Some((_, Kind::Short(_))) => {
                        let mut reply = vec![att::FIND_INFORMATION_RESPONSE, att::FORMAT_SHORT];
                        let short = listed.map_while(|(at, kind)| match kind {
                            Kind::Short(kind) => Some((at, kind)),
                            Kind::Long(_) => None,
                        });
                        for (at, kind) in short.take(SHORT_PAIRS_A_REPLY) {
                            reply.extend(at.to_le_bytes());
                            reply.extend(kind.to_le_bytes());
                        }
                        reply
                    }
                }
            }
            [att::WRITE_REQUEST, h0, h1, n0, n1] if [n0, n1] == NOTIFY => {
                let handle = u16::from_le_bytes([h0, h1]);
                assert!(self.descriptors.contains(&handle), "{handle:#06x}");
                match self.refuse_writes {
                    Some(code) => att::error_response(att::WRITE_REQUEST, handle, code).to_vec(),
                    None => vec![att::WRITE_RESPONSE],
                }
            }
            [att::READ_REQUEST, h0, h1] => {
                let handle = u16::from_le_bytes([h0, h1]);
                assert_eq!(
                    Some(handle),
                    self.codec_list_at(),
                    "a read of {handle:#06x}"
                );
                read_response(&self.codecs)
            }
            _ => panic!("the arm sent {request:02x?}, which the pods do not answer"),
        }
    }
}

/// Runs an arm against the pods to its end, and gives every request it sent.
fn arm(pods: &Pods) -> (ArmStep, Vec<Vec<u8>>) {
    let (mut arming, first) = Arming::new();
    let mut sent = vec![first];
    for _ in 0..STEP_LIMIT {
        let reply = pods.answer(sent.last().unwrap());
        match arming.on_pdu(&reply) {
            ArmStep::Send(request) => sent.push(request),
            end @ (ArmStep::Wait | ArmStep::Done { .. } | ArmStep::Failed(_)) => {
                return (end, sent);
            }
        }
    }
    panic!("the arm had not ended after {STEP_LIMIT} steps");
}

fn shifted(by: u16) -> Handles {
    let moved = |handle: u16| handle.wrapping_add(by);
    let observed = Handles::OBSERVED;
    Handles {
        supported_codecs: moved(observed.supported_codecs),
        select_codec: moved(observed.select_codec),
        start: moved(observed.start),
        stop: moved(observed.stop),
        send_data: moved(observed.send_data),
        event: moved(observed.event),
        cccds: observed.cccds.map(moved),
    }
}

fn done(handles: Handles) -> ArmStep {
    ArmStep::Done {
        select: select_codec(&handles),
        handles,
    }
}

#[test]
fn doap_mode_on_is_the_packet_the_pods_took() {
    assert_eq!(
        doap_mode_on().to_vec(),
        hex("04 00 04 00 09 00 13 01 00 00 00")
    );
}

#[test]
fn doap_mode_off_gives_the_stem_back() {
    const MODE_OFF: [u8; 11] = doap_mode_off();
    assert_eq!(MODE_OFF.to_vec(), hex("04 00 04 00 09 00 13 02 00 00 00"));
}

#[test]
fn the_builders_give_the_bytes_seen_at_the_observed_handles() {
    let handles = Handles::OBSERVED;
    let subscriptions = handles.cccds.map(cccd_on);
    assert_eq!(
        subscriptions.map(Vec::from),
        [
            hex("12 08 00 01 00"),
            hex("12 0b 00 01 00"),
            hex("12 0e 00 01 00"),
            hex("12 11 00 01 00"),
        ]
    );
    assert_eq!(read(handles.supported_codecs).to_vec(), hex("0a 03 00"));
    assert_eq!(select_codec(&handles).to_vec(), hex("52 05 00 01 01 00 07"));
    assert_eq!(
        event_write(&handles, &[0x01, 0x00, 0x04, 0x01, 0x07]).to_vec(),
        hex("52 10 00 01 00 04 01 07")
    );
    assert_eq!(start(&handles).to_vec(), hex("52 07 00 01 00"));
    assert_eq!(
        stop(&handles, STOP_REASON_HOST).to_vec(),
        hex("52 0a 00 01 00 00")
    );
}

#[test]
fn the_builders_write_to_the_handles_they_are_given() {
    let handles = shifted(0x0100);
    assert_eq!(cccd_on(0x0128).to_vec(), hex("12 28 01 01 00"));
    assert_eq!(read(0x0123).to_vec(), hex("0a 23 01"));
    assert_eq!(select_codec(&handles).to_vec(), hex("52 05 01 01 01 00 07"));
    assert_eq!(
        event_write(&handles, &[0xa0, 0xa1, 0xa2, 0xa3, 0xa4]).to_vec(),
        hex("52 10 01 a0 a1 a2 a3 a4")
    );
    assert_eq!(start(&handles).to_vec(), hex("52 07 01 01 00"));
    assert_eq!(
        stop(&handles, STOP_REASON_UNSOLICITED).to_vec(),
        hex("52 0a 01 01 00 06")
    );
}

#[test]
fn the_score_the_pods_send_at_start_is_minus_one() {
    let bits = (-1.0_f32).to_bits();
    assert_eq!(
        DictationPacket::parse(&STARTED, &Handles::OBSERVED),
        Ok(Some(DictationPacket::Event(Event::Score { bits })))
    );
}

#[test]
fn the_refusal_is_stopped_with_reason_6() {
    let refusal = hex("1b 0a 00 01 00 06");
    let reason = STOP_REASON_UNSOLICITED;
    assert_eq!(
        DictationPacket::parse(&refusal, &Handles::OBSERVED),
        Ok(Some(DictationPacket::Stopped { reason }))
    );
}

#[test]
fn the_end_is_streaming_end() {
    assert_eq!(
        DictationPacket::parse(&STOPPED, &Handles::OBSERVED),
        Ok(Some(DictationPacket::Event(Event::StreamingEnd)))
    );
}

/// Any bytes at all, and the replies of an arm, which arbitrary bytes almost never form.
fn any_pdu() -> impl Strategy<Value = Vec<u8>> {
    let bytes = proptest::collection::vec(any::<u8>(), 0..=LONGEST_PACKET);
    let uuid = proptest::sample::select(SERVICE.to_vec());
    let declarations = (any::<u16>(), any::<u16>(), uuid)
        .prop_map(|(at, value, uuid)| declarations_reply(&[(at, value, wire(uuid))]));
    let pairs = proptest::collection::vec(any::<u16>(), 1..=SHORT_PAIRS_A_REPLY);
    let descriptors = pairs.prop_map(|handles| {
        let mut reply = vec![att::FIND_INFORMATION_RESPONSE, att::FORMAT_SHORT];
        for handle in handles {
            reply.extend(handle.to_le_bytes());
            reply.extend(att::CLIENT_CHARACTERISTIC_CONFIGURATION.to_le_bytes());
        }
        reply
    });
    let not_found = prop_oneof![
        Just(att::READ_BY_TYPE_REQUEST),
        Just(att::FIND_INFORMATION_REQUEST)
    ]
    .prop_map(|request| att::error_response(request, 0, att::ATTRIBUTE_NOT_FOUND).to_vec());
    let codecs = proptest::collection::vec(any::<u8>(), 0..CODEC_LIST_BOUND);
    let codecs = codecs.prop_map(|value| read_response(&value));
    prop_oneof![
        bytes,
        declarations,
        descriptors,
        not_found,
        Just(vec![att::WRITE_RESPONSE]),
        Just(read_response(&hex(CODECS))),
        codecs,
    ]
}

/// A notification on a handle of the observed table: the three the parser sorts and the start handle, which it does not.
fn observed_notification() -> impl Strategy<Value = Vec<u8>> {
    let table = Handles::OBSERVED;
    let handle =
        proptest::sample::select(vec![table.stop, table.send_data, table.event, table.start]);
    let value = proptest::collection::vec(any::<u8>(), 0..VALUE_BOUND);
    (handle, value).prop_map(|(handle, value)| {
        let opcode = [att::HANDLE_VALUE_NOTIFICATION];
        [&opcode[..], &handle.to_le_bytes(), &value].concat()
    })
}

proptest! {
    #[test]
    fn every_notification_parses_or_refuses_by_name(
        pdu in prop_oneof![
            proptest::collection::vec(any::<u8>(), 0..=LONGEST_PACKET),
            observed_notification(),
        ],
        handles in prop_oneof![Just(Handles::OBSERVED), Just(ONE_HANDLE_FOR_THREE), any::<[u16; 10]>().prop_map(|h| Handles {
            supported_codecs: h[0], select_codec: h[1], start: h[2], stop: h[3], send_data: h[4], event: h[5],
            cccds: [h[6], h[7], h[8], h[9]],
        })],
    ) {
        let notified = match AttPdu::parse(&pdu) {
            Ok(AttPdu::Notification { handle, value }) => Some((handle, value)),
            _ => None,
        };
        match DictationPacket::parse(&pdu, &handles) {
            Ok(None) => {
                let ours = [handles.stop, handles.send_data, handles.event];
                prop_assert!(notified.is_none_or(|(handle, _)| !ours.contains(&handle)));
            }
            Ok(Some(DictationPacket::Unknown { handle, value })) => prop_assert_eq!(notified, Some((handle, value))),
            Ok(Some(DictationPacket::Audio(audio))) => {
                prop_assert_eq!(notified.map(|(handle, _)| handle), Some(handles.send_data));
                prop_assert!(audio.frames().count() <= pdu.len());
            }
            Ok(Some(DictationPacket::Stopped { reason })) => {
                let stream = STREAM_ID.to_le_bytes();
                let value = notified.filter(|(handle, _)| *handle == handles.stop).map(|(_, value)| value);
                prop_assert_eq!(value, Some(&[stream[0], stream[1], reason][..]));
            }
            Ok(Some(DictationPacket::Event(_))) => prop_assert_eq!(notified.map(|(handle, _)| handle), Some(handles.event)),
            Err(ParseError::TooShort { parser, .. }) => {
                prop_assert!([Parser::AttNotification, Parser::DictationAudio].contains(&parser));
            }
            Err(other) => prop_assert!(false, "refused with {other}"),
        }
    }

    #[test]
    fn an_arm_ends_inside_its_bound_whatever_arrives(
        continued in 0..=CONTINUED_BOUND,
        arrived in proptest::collection::vec(any_pdu(), 0..ARRIVED_BOUND),
    ) {
        let (mut arming, first) = Arming::new();
        let mut asked = 1_u32;
        let mut subscribed = 0_u32;
        let mut over = false;
        let mut last_start = u16::from_le_bytes([first[1], first[2]]);
        let mut out = first;
        // Replies that continue the walk come first, since arbitrary bytes almost never do; then whatever arrives.
        let continuing = (0..continued).map(|_| None);
        for pdu in continuing.chain(arrived.iter().map(Some)) {
            let pdu = pdu.cloned().unwrap_or_else(|| endless(&out));
            match arming.on_pdu(&pdu) {
                ArmStep::Send(request) => {
                    prop_assert!(!over);
                    match request[0] {
                        att::READ_BY_TYPE_REQUEST => {
                            let start = u16::from_le_bytes([request[1], request[2]]);
                            prop_assert!(start > last_start);
                            last_start = start;
                            asked = asked.saturating_add(1);
                        }
                        att::FIND_INFORMATION_REQUEST => asked = asked.saturating_add(1),
                        att::WRITE_REQUEST | att::READ_REQUEST => subscribed = subscribed.saturating_add(1),
                        opcode => prop_assert!(false, "sent opcode {opcode:#04x}"),
                    }
                    out = request;
                }
                ArmStep::Wait => {}
                ArmStep::Done { .. } | ArmStep::Failed(_) => {
                    prop_assert!(!over);
                    over = true;
                }
            }
        }
        prop_assert!(asked <= ARM_MAX_REQUESTS);
        prop_assert!(subscribed <= REQUESTS_AFTER_THE_PASS);
        // A walk continued as often as an arm may ask has asked all of it, and is over.
        if continued >= ARM_MAX_REQUESTS {
            prop_assert!(over);
            prop_assert_eq!(asked, ARM_MAX_REQUESTS);
        }
    }
}
