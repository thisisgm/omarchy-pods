//! Random input never panics a parser, and a parsed control command survives a round trip.

use pods_proto::addr::BdAddr;
use pods_proto::att::{self, AttPdu, HandleInfo, Uuid};
use pods_proto::battery::BatteryReport;
use pods_proto::control::ControlCommand;
use pods_proto::dictation::{self, DictationPacket, Frame, STARTED, STOPPED, Status};
use pods_proto::ear::EarReport;
use pods_proto::error::{ParseError, Parser};
use pods_proto::sdulog::{Direction, format_line, parse_line};
use pods_proto::uplink::{
    AUDIO_HEADER_LEN, AccessUnit, OPAQUE_LEN, UNIT_PREFIX_LEN, UplinkPacket, encode_audio,
};
use proptest::prelude::*;

// Sample output: `04 00 04 00 09 00 <id> <v0> <v1> <v2> <v3>`, the control channel frame.
fn sample_frame(id: u8, v: [u8; 4]) -> Vec<u8> {
    let mut frame = vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, id];
    frame.extend(v);
    frame
}

const CONTROL_HEADER: [u8; 6] = [0x04, 0x00, 0x04, 0x00, 0x09, 0x00];
const BATTERY_HEADER: [u8; 6] = [0x04, 0x00, 0x04, 0x00, 0x04, 0x00];
const EAR_HEADER: [u8; 6] = [0x04, 0x00, 0x04, 0x00, 0x06, 0x00];
const UPLINK_AUDIO_GATE: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x01, 0x00];
const UPLINK_CONTROL_GATE: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x00, 0x00];
/// Length bound of a generated SDU, room for sixteen access units of the longest kind.
const MAX_SDU: usize = 4200;
/// Opcode of a Find Information reply, which its format byte follows.
const ATT_FIND_INFORMATION_REPLY: u8 = 0x05;
/// The format byte for pairs with 16 bit identifiers, and the length of one such pair.
const ATT_SHORT_FORMAT: (u8, usize) = (0x01, 4);
/// The format byte for pairs with 128 bit identifiers, and the length of one such pair.
const ATT_LONG_FORMAT: (u8, usize) = (0x02, 18);
/// Opcode of a Read By Type reply, which its entry length follows.
const ATT_READ_BY_TYPE_REPLY: u8 = 0x09;
/// The shortest entry a Read By Type reply may declare, one handle.
const ATT_ENTRY_MIN_LEN: u8 = 2;
/// Entry lengths of a characteristic declaration with a 16 bit and with a 128 bit identifier.
const ATT_DECLARATION_LENS: [usize; 2] = [7, 21];
/// Opcode of a notification, which a handle and a value follow.
const ATT_NOTIFICATION: u8 = 0x1b;
/// Opcode of an indication, which a handle and a value follow.
const ATT_INDICATION: u8 = 0x1d;
/// A handle, the fixed part behind the opcode of a notification or an indication.
const ATT_HANDLE_LEN: usize = 2;
/// Opcode and handle, the shortest notification or indication.
const ATT_HANDLE_VALUE_MIN_LEN: usize = 3;
/// Opcode of an error response, and the only length one has.
const ATT_ERROR: (u8, usize) = (0x01, 5);
/// Length bound of a generated error response, twice the only valid length and more.
const ATT_ERROR_BOUND: usize = 12;
/// Bytes 8 and 9 of an uplink control packet, its declared length.
const DECLARED_LEN_BYTES: usize = 2;
/// The gate plus the declared length, the shortest uplink control packet.
const UPLINK_CONTROL_MIN_LEN: usize = 10;
/// Where the length byte sits in an access unit, after the four stamp bytes.
const UNIT_LENGTH_AT: usize = 4;
/// One more than the most access units a generated SDU carries; also the stamps drawn.
const UNIT_COUNT_BOUND: usize = 8;
/// Length bound of a generated capture line, longer than any line `parse_line` accepts.
const MAX_LINE_CHARS: usize = 9000;
/// Short so the one changed character often lands in the stamp or the direction.
const CHANGED_SDU_MAX: usize = 16;
/// Digits, both marks, a space, signs, hex in both cases, and text no field holds.
const CHANGED_TO: &[char] = &[
    '0', '9', ' ', '<', '>', '+', '-', 'a', 'f', 'A', 'F', 'g', '\n', '\u{e9}',
];
/// Notification opcode and audio handle 0x000d, little endian: a dictation audio gate.
const DICTATION_AUDIO_GATE: [u8; 3] = [0x1b, 0x0d, 0x00];
/// The two bytes every dictation audio value starts with.
const DICTATION_AUDIO_MARKER: [u8; 2] = [0x01, 0x00];
/// Notification opcode and the status handle 0x0010, little endian.
const DICTATION_STATUS_GATE: [u8; 3] = [0x1b, 0x10, 0x00];
/// Opcodes the ATT reader refuses when malformed: error response, list replies, indication.
const ATT_REFUSED_WHEN_MALFORMED: [u8; 4] = [
    ATT_ERROR.0,
    ATT_FIND_INFORMATION_REPLY,
    ATT_READ_BY_TYPE_REPLY,
    ATT_INDICATION,
];
/// After the flag, the level and the two counter bytes.
const FRAME_LENGTH_AT: usize = 4;
/// One more than the most frames a generated notification carries.
const FRAME_COUNT_BOUND: usize = 8;
/// Length bound of a generated value that is noise, longer than both known status replies.
const VALUE_NOISE_BOUND: usize = 16;
/// After a cut audio notification's marker: packet counter, two length bytes, a frame prefix.
const CUT_NOISE_LEN: usize = 8;

// Sample output: `[04, 01, 55, 02, 01]`, kind, spacer, level, status, end; marks mostly 0x01.
fn battery_entry() -> impl Strategy<Value = [u8; 5]> {
    let kind = prop_oneof![
        Just(0x01_u8),
        Just(0x02),
        Just(0x04),
        Just(0x08),
        any::<u8>()
    ];
    let status = prop_oneof![Just(0x01_u8), Just(0x02), Just(0x04), any::<u8>()];
    let mark = prop_oneof![9 => Just(0x01_u8), 1 => any::<u8>()];
    (kind, mark.clone(), any::<u8>(), status, mark)
        .prop_map(|(kind, spacer, level, status, end)| [kind, spacer, level, status, end])
}

/// Walks whatever list the PDU holds to its end, reading every entry as a declaration.
fn walk_att(pdu: &[u8]) {
    match AttPdu::parse(pdu) {
        Ok(AttPdu::Information(pairs)) => assert!(pairs.count() <= pdu.len()),
        Ok(AttPdu::ByType(entries)) => {
            assert!(entries.map(att::characteristic).count() <= pdu.len());
        }
        Ok(_) | Err(_) => {}
    }
}

// Sample output: `[01, 00, 00, 28]` for handle 0x0001 and identifier 0x2800.
fn att_pair_bytes(pair: HandleInfo<'_>) -> Vec<u8> {
    let mut bytes = pair.handle.to_le_bytes().to_vec();
    match pair.uuid {
        Uuid::Short(uuid) => bytes.extend(uuid.to_le_bytes()),
        Uuid::Long(uuid) => bytes.extend(uuid),
    }
    bytes
}

// Sample output: `(AttFindInformation, [05, 01], 4)`, reader, opening bytes, entry length.
fn att_list_prefix() -> impl Strategy<Value = (Parser, [u8; 2], usize)> {
    prop_oneof![
        Just((
            Parser::AttFindInformation,
            [ATT_FIND_INFORMATION_REPLY, ATT_SHORT_FORMAT.0],
            ATT_SHORT_FORMAT.1
        )),
        Just((
            Parser::AttFindInformation,
            [ATT_FIND_INFORMATION_REPLY, ATT_LONG_FORMAT.0],
            ATT_LONG_FORMAT.1
        )),
        (ATT_ENTRY_MIN_LEN..=u8::MAX).prop_map(|len| {
            (
                Parser::AttReadByType,
                [ATT_READ_BY_TYPE_REPLY, len],
                usize::from(len),
            )
        }),
    ]
}

// Sample output: `AA:bb:CC:dd:EE:ff`, about one part in ten replaced by text not allowed.
fn address_text() -> impl Strategy<Value = String> {
    let pair = || prop_oneof![9 => "[0-9a-fA-F]{2}", 1 => "[ -~]{0,3}"];
    let colon = || prop_oneof![9 => Just(":".to_owned()), 1 => "[ -~]{0,2}"];
    (
        [pair(), pair(), pair(), pair(), pair(), pair()],
        [colon(), colon(), colon(), colon(), colon()],
    )
        .prop_map(|(pairs, colons)| {
            let mut text = String::new();
            for (at, pair) in pairs.iter().enumerate() {
                text.push_str(pair);
                text.push_str(colons.get(at).map_or("", String::as_str));
            }
            text
        })
}

// Checked one byte at a time: colons at 2, 5, 8, 11 and 14, hex digits elsewhere.
fn octets_of(text: &str) -> Option<Vec<u8>> {
    let shaped = text.len() == 17
        && text.bytes().enumerate().all(|(at, byte)| {
            if at % 3 == 2 {
                byte == b':'
            } else {
                byte.is_ascii_hexdigit()
            }
        });
    if !shaped {
        return None;
    }
    text.split(':')
        .map(|pair| u8::from_str_radix(pair, 16).ok())
        .collect()
}

// Sample output: `(0x80, 0xd5, 0x0104, [b8, 10, 11])`; counters and lengths lean on their edges.
fn dictation_frame() -> impl Strategy<Value = (u8, u8, u16, Vec<u8>)> {
    let longest = usize::from(u8::MAX);
    let counter = prop_oneof![Just(0_u16), Just(u16::MAX), any::<u16>()];
    let data = prop_oneof![
        Just(Vec::new()),
        proptest::collection::vec(any::<u8>(), longest),
        proptest::collection::vec(any::<u8>(), 0..=longest),
    ];
    (any::<u8>(), any::<u8>(), counter, data)
}

// Sample output: `01 00 03 00`: a known reply, one bit flipped, a byte longer or shorter, or noise.
fn dictation_status_value() -> impl Strategy<Value = Vec<u8>> {
    let value_of = |reply: &[u8]| -> Vec<u8> {
        let after_the_gate = reply.iter().skip(DICTATION_STATUS_GATE.len());
        after_the_gate.copied().collect()
    };
    let known = move || prop_oneof![Just(value_of(&STARTED)), Just(value_of(&STOPPED))];
    let flipped = (known(), any::<proptest::sample::Index>(), 0..u8::BITS).prop_map(
        |(mut value, at, bit)| {
            let at = at.index(value.len());
            if let Some(byte) = value.get_mut(at) {
                *byte ^= 1_u8.rotate_left(bit);
            }
            value
        },
    );
    let longer = (known(), any::<u8>()).prop_map(|(mut value, more)| {
        value.push(more);
        value
    });
    let shorter = known().prop_map(|mut value| {
        value.pop();
        value
    });
    let noise = proptest::collection::vec(any::<u8>(), 0..VALUE_NOISE_BOUND);
    prop_oneof![known(), flipped, longer, shorter, noise]
}

// Sample output: `1b 0d 00 01 00 2a`, cut anywhere up to one frame prefix past the audio prefix.
fn dictation_cut_audio_notification() -> impl Strategy<Value = Vec<u8>> {
    let noise = proptest::collection::vec(any::<u8>(), CUT_NOISE_LEN);
    (noise, any::<proptest::sample::Index>()).prop_map(|(noise, kept)| {
        let mut pdu = DICTATION_AUDIO_GATE.to_vec();
        pdu.extend(DICTATION_AUDIO_MARKER);
        pdu.extend(noise);
        pdu.truncate(kept.index(pdu.len().saturating_add(1)));
        pdu
    })
}

// Sample output: [01, 80], the audio marker with one of its sixteen bits flipped.
fn dictation_near_marker() -> impl Strategy<Value = [u8; 2]> {
    (0..u16::BITS).prop_map(|bit| {
        let flipped = u16::from_le_bytes(DICTATION_AUDIO_MARKER) ^ 1_u16.rotate_left(bit);
        flipped.to_le_bytes()
    })
}

proptest! {
    #[test]
    fn address_parse_never_panics(text in any::<String>()) {
        let _outcome = text.parse::<BdAddr>();
    }

    #[test]
    fn address_parse_reads_exactly_the_colon_and_hex_shape(text in address_text()) {
        let parsed = text.parse::<BdAddr>().ok().map(|addr| addr.octets().to_vec());
        prop_assert_eq!(parsed, octets_of(&text));
    }

    #[test]
    fn battery_parse_never_panics(b in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _outcome = BatteryReport::parse(&b);
    }

    #[test]
    fn battery_parse_never_panics_behind_the_header(body in proptest::collection::vec(any::<u8>(), 0..64)) {
        let mut packet = BATTERY_HEADER.to_vec();
        packet.extend(body);
        let _outcome = BatteryReport::parse(&packet);
    }

    // A count agreeing with the entries and 0x01 marks parses; a nearby count or length is refused.
    #[test]
    fn battery_parse_walks_the_entries(
        entries in proptest::collection::vec(battery_entry(), 0..=4),
        count_shift in prop_oneof![8 => Just(0_u8), 1 => Just(1), 1 => Just(u8::MAX)],
        trailing in prop_oneof![8 => Just(Vec::new()), 2 => proptest::collection::vec(any::<u8>(), 1..5)],
    ) {
        let count = u8::try_from(entries.len()).unwrap().wrapping_add(count_shift);
        let mut packet = BATTERY_HEADER.to_vec();
        packet.push(count);
        packet.extend(entries.iter().flatten());
        packet.extend(&trailing);
        let marks_valid = entries.iter().all(|e| e[1] == 0x01 && e[4] == 0x01);
        let well_formed = count_shift == 0 && trailing.is_empty() && entries.len() <= 3 && marks_valid;
        prop_assert_eq!(BatteryReport::parse(&packet).is_ok(), well_formed);
    }

    #[test]
    fn ear_parse_reads_any_body_behind_the_header(body in proptest::collection::vec(any::<u8>(), 0..16)) {
        let mut packet = EAR_HEADER.to_vec();
        packet.extend(&body);
        prop_assert_eq!(EarReport::parse(&packet).is_ok(), body.len() >= 2);
    }

    #[test]
    fn ear_parse_never_panics(b in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _outcome = EarReport::parse(&b);
    }

    #[test]
    fn control_parse_never_panics(b in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _outcome = ControlCommand::parse(&b);
    }

    #[test]
    fn control_round_trips(id in any::<u8>(), v in any::<[u8; 4]>()) {
        let c = ControlCommand::parse(&sample_frame(id, v)).unwrap();
        prop_assert_eq!(c.encode(), sample_frame(id, v));
        prop_assert_eq!(ControlCommand::parse(&c.encode()).unwrap(), c);
    }

    #[test]
    fn decrypted_battery_parse_never_panics(
        b in proptest::collection::vec(any::<u8>(), 0..64),
        primary_left in any::<bool>(),
        pod_in_case in any::<bool>(),
        headset in any::<bool>(),
    ) {
        let _outcome = BatteryReport::parse_decrypted(&b, primary_left, pod_in_case, headset);
    }

    #[test]
    fn decrypted_battery_parse_reads_every_sixteen_byte_payload(
        payload in any::<[u8; 16]>(),
        primary_left in any::<bool>(),
        pod_in_case in any::<bool>(),
        headset in any::<bool>(),
    ) {
        prop_assert!(BatteryReport::parse_decrypted(&payload, primary_left, pod_in_case, headset).is_ok());
    }

    // tst_stress parseActive_fuzzShape: random, header plus tail, and a short prefix of the header.
    #[test]
    fn parse_active_fuzz_shape(
        random in proptest::collection::vec(any::<u8>(), 0..=20),
        tail in proptest::collection::vec(any::<u8>(), 0..=16),
        prefix_len in 0..=6_usize,
    ) {
        let _random = ControlCommand::parse(&random);
        let mut with_header = CONTROL_HEADER.to_vec();
        with_header.extend(tail);
        let _with_header = ControlCommand::parse(&with_header);
        let _prefix = ControlCommand::parse(&CONTROL_HEADER[..prefix_len]);
    }

    // tst_stress parseState_fuzzValueByte: state is Some only for 0x01 and 0x02.
    #[test]
    fn parse_state_fuzz_value_byte(
        id in any::<u8>(),
        value in any::<u8>(),
        tail in proptest::collection::vec(any::<u8>(), 0..8),
    ) {
        let mut buf = CONTROL_HEADER.to_vec();
        buf.extend([id, value]);
        buf.extend(tail);
        let state = ControlCommand::parse(&buf).unwrap().state();
        match value {
            0x01 => prop_assert_eq!(state, Some(true)),
            0x02 => prop_assert_eq!(state, Some(false)),
            _ => prop_assert_eq!(state, None),
        }
    }

    #[test]
    fn att_parse_never_panics(b in proptest::collection::vec(any::<u8>(), 0..MAX_SDU)) {
        walk_att(&b);
    }

    // Every whole entry is yielded in order and a short tail counted; no whole entry is refused.
    #[test]
    fn att_list_replies_account_for_every_byte(
        (parser, prefix, entry_len) in att_list_prefix(),
        body in proptest::collection::vec(any::<u8>(), 0..MAX_SDU),
    ) {
        let mut pdu = prefix.to_vec();
        pdu.extend(&body);
        let whole: Vec<&[u8]> = body.chunks_exact(entry_len).collect();
        let tail = body.chunks_exact(entry_len).remainder().len();
        match AttPdu::parse(&pdu) {
            Ok(AttPdu::Information(mut pairs)) => {
                let seen: Vec<Vec<u8>> = pairs.by_ref().map(att_pair_bytes).collect();
                prop_assert!(!seen.is_empty());
                prop_assert_eq!(seen, whole);
                prop_assert_eq!(pairs.trailing(), tail);
            }
            Ok(AttPdu::ByType(mut entries)) => {
                let seen: Vec<&[u8]> = entries.by_ref().collect();
                prop_assert!(!seen.is_empty());
                prop_assert_eq!(&seen, &whole);
                prop_assert_eq!(entries.trailing(), tail);
                for entry in seen {
                    prop_assert_eq!(att::characteristic(entry).is_some(), ATT_DECLARATION_LENS.contains(&entry_len));
                }
            }
            other => {
                prop_assert!(whole.is_empty(), "a list reply did not parse as one: {:?}", other);
                let empty = ParseError::EmptyList { parser, list_len: body.len() };
                prop_assert_eq!(other, Err(empty));
            }
        }
    }

    // A reply whose list holds no whole entry is refused, so no walk learns a handle from it.
    #[test]
    fn att_list_reply_with_no_whole_entry_is_refused(
        ((parser, prefix, entry_len), body) in att_list_prefix().prop_flat_map(|list| {
            (Just(list), proptest::collection::vec(any::<u8>(), 0..list.2))
        }),
    ) {
        prop_assert!(body.len() < entry_len);
        let mut pdu = prefix.to_vec();
        pdu.extend(&body);
        let empty = ParseError::EmptyList { parser, list_len: body.len() };
        prop_assert_eq!(AttPdu::parse(&pdu), Err(empty));
    }

    #[test]
    fn att_find_information_with_an_undefined_format_is_refused(
        format in any::<u8>().prop_filter("the format is a defined one", |format| ![ATT_SHORT_FORMAT.0, ATT_LONG_FORMAT.0].contains(format)),
        body in proptest::collection::vec(any::<u8>(), 0..MAX_SDU),
    ) {
        let mut pdu = vec![ATT_FIND_INFORMATION_REPLY, format];
        pdu.extend(&body);
        let refused = ParseError::UnknownValue { parser: Parser::AttFindInformation, field: "format", value: format };
        prop_assert_eq!(AttPdu::parse(&pdu), Err(refused));
    }

    // The zero entry length, which would never end, is included.
    #[test]
    fn att_read_by_type_with_an_entry_shorter_than_a_handle_is_refused(
        entry_len in 0..ATT_ENTRY_MIN_LEN,
        body in proptest::collection::vec(any::<u8>(), 0..MAX_SDU),
    ) {
        let mut pdu = vec![ATT_READ_BY_TYPE_REPLY, entry_len];
        pdu.extend(&body);
        let refused = ParseError::UnknownValue { parser: Parser::AttReadByType, field: "entry length", value: entry_len };
        prop_assert_eq!(AttPdu::parse(&pdu), Err(refused));
    }

    #[test]
    fn att_handle_values_keep_every_byte_behind_the_handle(
        (opcode, parser) in prop_oneof![
            Just((ATT_NOTIFICATION, Parser::AttNotification)),
            Just((ATT_INDICATION, Parser::AttIndication)),
        ],
        body in prop_oneof![
            proptest::collection::vec(any::<u8>(), 0..ATT_HANDLE_VALUE_MIN_LEN),
            proptest::collection::vec(any::<u8>(), 0..MAX_SDU),
        ],
    ) {
        let mut pdu = vec![opcode];
        pdu.extend(&body);
        match AttPdu::parse(&pdu) {
            Ok(AttPdu::Notification { handle, value }) => {
                prop_assert_eq!(opcode, ATT_NOTIFICATION);
                prop_assert_eq!(handle, u16::from_le_bytes([body[0], body[1]]));
                prop_assert_eq!(value, &body[ATT_HANDLE_LEN..]);
            }
            Ok(AttPdu::Indication { handle, value }) => {
                prop_assert_eq!(opcode, ATT_INDICATION);
                prop_assert_eq!(handle, u16::from_le_bytes([body[0], body[1]]));
                prop_assert_eq!(value, &body[ATT_HANDLE_LEN..]);
            }
            other => {
                prop_assert!(body.len() < ATT_HANDLE_LEN, "{:?}", other);
                let too_short = ParseError::TooShort { parser, need: ATT_HANDLE_VALUE_MIN_LEN, got: pdu.len() };
                prop_assert_eq!(other, Err(too_short));
            }
        }
    }

    #[test]
    fn att_error_response_round_trips(request in any::<u8>(), handle in any::<u16>(), code in any::<u8>()) {
        let built = att::error_response(request, handle, code);
        prop_assert_eq!(AttPdu::parse(&built), Ok(AttPdu::Error { request, handle, code }));
    }

    #[test]
    fn att_error_response_is_read_at_one_length_only(body in proptest::collection::vec(any::<u8>(), 0..ATT_ERROR_BOUND)) {
        let mut pdu = vec![ATT_ERROR.0];
        pdu.extend(&body);
        let wrong_length = ParseError::WrongLength { parser: Parser::AttError, expected: ATT_ERROR.1, got: pdu.len() };
        match AttPdu::parse(&pdu) {
            Ok(AttPdu::Error { .. }) => prop_assert_eq!(pdu.len(), ATT_ERROR.1),
            other => prop_assert_eq!(other, Err(wrong_length)),
        }
    }

    #[test]
    fn uplink_parse_never_panics(b in proptest::collection::vec(any::<u8>(), 0..MAX_SDU)) {
        let _outcome = UplinkPacket::parse(&b);
    }

    // Yielded units encode back to the SDU's front; trailing counts a tail of no whole unit.
    #[test]
    fn uplink_parse_past_the_header_never_panics(body in proptest::collection::vec(any::<u8>(), 0..MAX_SDU)) {
        let mut sdu = UPLINK_AUDIO_GATE.to_vec();
        sdu.extend(&body);
        match UplinkPacket::parse(&sdu) {
            Ok(Some(UplinkPacket::Audio(audio))) => {
                let mut units = audio.units();
                let whole: Vec<_> = units.by_ref().collect();
                let rebuilt = encode_audio(audio.opaque, &whole).unwrap();
                prop_assert!(sdu.starts_with(&rebuilt));
                let tail = &sdu[rebuilt.len()..];
                prop_assert_eq!(units.trailing(), tail.len());
                let cut_short = tail.get(UNIT_PREFIX_LEN..).is_none_or(|data| data.len() < usize::from(tail[UNIT_LENGTH_AT]));
                prop_assert!(cut_short, "a whole unit was left in the tail {:02x?}", tail);
            }
            other => {
                prop_assert!(body.len() < OPAQUE_LEN, "{:?}", other);
                let too_short = ParseError::TooShort { parser: Parser::UplinkAudio, need: AUDIO_HEADER_LEN, got: sdu.len() };
                prop_assert_eq!(other, Err(too_short));
            }
        }
    }

    #[test]
    fn uplink_control_keeps_every_byte_behind_the_gate(body in proptest::collection::vec(any::<u8>(), 0..MAX_SDU)) {
        let mut sdu = UPLINK_CONTROL_GATE.to_vec();
        sdu.extend(&body);
        match UplinkPacket::parse(&sdu) {
            Ok(Some(UplinkPacket::Control(control))) => {
                prop_assert_eq!(control.declared_len, u16::from_le_bytes([body[0], body[1]]));
                prop_assert_eq!(control.payload, &body[DECLARED_LEN_BYTES..]);
                prop_assert_eq!(control.length_matches(), usize::from(control.declared_len) == control.payload.len());
            }
            other => {
                prop_assert!(body.len() < DECLARED_LEN_BYTES, "{:?}", other);
                let too_short = ParseError::TooShort { parser: Parser::UplinkControl, need: UPLINK_CONTROL_MIN_LEN, got: sdu.len() };
                prop_assert_eq!(other, Err(too_short));
            }
        }
    }

    // Whole units survive a round trip; a cut SDU yields every unit that fits and counts the rest.
    #[test]
    fn uplink_audio_round_trips_and_counts_a_cut_tail(
        opaque in any::<[u8; OPAQUE_LEN]>(),
        payloads in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..=usize::from(u8::MAX)), 0..UNIT_COUNT_BOUND),
        stamps in any::<[u32; UNIT_COUNT_BOUND]>(),
        cut in any::<proptest::sample::Index>(),
    ) {
        let units: Vec<AccessUnit<'_>> = stamps.iter().zip(&payloads).map(|(&stamp, data)| AccessUnit { stamp, data }).collect();
        let sdu = encode_audio(&opaque, &units).unwrap();
        let Ok(Some(UplinkPacket::Audio(audio))) = UplinkPacket::parse(&sdu) else {
            return Err(TestCaseError::fail("an encoded SDU did not parse as audio"));
        };
        prop_assert_eq!(audio.opaque, &opaque);
        let mut seen = audio.units();
        prop_assert_eq!(&seen.by_ref().collect::<Vec<_>>(), &units);
        prop_assert_eq!(seen.trailing(), 0);

        let kept = cut.index(sdu.len()).max(AUDIO_HEADER_LEN);
        let Ok(Some(UplinkPacket::Audio(cut_audio))) = UplinkPacket::parse(&sdu[..kept]) else {
            return Err(TestCaseError::fail("a cut SDU with a whole header did not parse as audio"));
        };
        let mut cut_units = cut_audio.units();
        let whole: Vec<_> = cut_units.by_ref().collect();
        prop_assert_eq!(whole.as_slice(), &units[..whole.len()]);
        let rebuilt = encode_audio(&opaque, &whole).unwrap();
        prop_assert_eq!(kept.checked_sub(rebuilt.len()), Some(cut_units.trailing()));
        if let Some(one_more) = units.get(..=whole.len()) {
            prop_assert!(encode_audio(&opaque, one_more).unwrap().len() > kept, "a unit that fits was not yielded");
        }
    }

    #[test]
    fn parse_line_never_panics(chars in proptest::collection::vec(any::<char>(), 0..=MAX_LINE_CHARS)) {
        let line: String = chars.into_iter().collect();
        let _outcome = parse_line(&line);
    }

    // A changed line is refused, or it is exactly what format_line writes for the record read.
    #[test]
    fn parse_line_reads_a_changed_line_only_as_format_line_writes_it(
        at_ns in any::<u64>(),
        direction in prop_oneof![Just(Direction::FromPods), Just(Direction::ToPods)],
        sdu in proptest::collection::vec(any::<u8>(), 1..=CHANGED_SDU_MAX),
        at in any::<proptest::sample::Index>(),
        changed_to in proptest::sample::select(CHANGED_TO),
    ) {
        let mut chars: Vec<char> = format_line(at_ns, direction, &sdu).unwrap().chars().collect();
        let at = at.index(chars.len());
        chars[at] = changed_to;
        let line: String = chars.into_iter().collect();
        if let Ok(record) = parse_line(&line) {
            prop_assert_eq!(format_line(record.at_ns, record.direction, &record.sdu), Ok(line));
        }
    }

    // Uniform lengths rarely cut a notification inside its prefix, so half the cases are such cuts.
    #[test]
    fn dictation_parse_never_panics(b in prop_oneof![
        proptest::collection::vec(any::<u8>(), 0..MAX_SDU),
        dictation_cut_audio_notification(),
    ]) {
        let _outcome = DictationPacket::parse(&b);
    }

    // Yielded frames encode back to the body's front; trailing counts a tail of no whole frame.
    #[test]
    fn dictation_parse_past_the_prefix_never_panics(
        packet_counter in any::<u8>(),
        declared_shift in prop_oneof![Just(0_u16), any::<u16>()],
        body in proptest::collection::vec(any::<u8>(), 0..MAX_SDU),
    ) {
        let declared_len = u16::try_from(body.len()).unwrap().wrapping_add(declared_shift);
        let mut pdu = DICTATION_AUDIO_GATE.to_vec();
        pdu.extend(DICTATION_AUDIO_MARKER);
        pdu.push(packet_counter);
        pdu.extend(declared_len.to_le_bytes());
        pdu.extend(&body);
        let Ok(Some(DictationPacket::Audio(audio))) = DictationPacket::parse(&pdu) else {
            return Err(TestCaseError::fail("a notification with the whole audio prefix did not parse as audio"));
        };
        prop_assert_eq!(audio.packet_counter, packet_counter);
        prop_assert_eq!(audio.declared_len, declared_len);
        prop_assert_eq!(audio.length_matches(), declared_shift == 0);
        let mut frames = audio.frames();
        let whole: Vec<_> = frames.by_ref().collect();
        let used: usize = whole.iter().map(|f| f.data.len().saturating_add(dictation::FRAME_PREFIX_LEN)).sum();
        prop_assert_eq!(used.checked_add(frames.trailing()), Some(body.len()));
        let rebuilt = dictation::encode_audio(packet_counter, &whole).unwrap();
        let rebuilt_frames = &rebuilt[dictation::AUDIO_PREFIX_LEN..];
        prop_assert!(body.starts_with(rebuilt_frames));
        let tail = &body[rebuilt_frames.len()..];
        prop_assert_eq!(frames.trailing(), tail.len());
        let cut_short = tail.get(dictation::FRAME_PREFIX_LEN..).is_none_or(|data| data.len() < usize::from(tail[FRAME_LENGTH_AT]));
        prop_assert!(cut_short, "a whole frame was left in the tail {:02x?}", tail);
    }

    // Audio handle: too short before the length bytes end, unknown without the marker, else audio.
    #[test]
    fn dictation_audio_handle_sorts_every_value(
        marker in prop_oneof![Just(DICTATION_AUDIO_MARKER), dictation_near_marker(), any::<[u8; 2]>()],
        rest in proptest::collection::vec(any::<u8>(), 0..VALUE_NOISE_BOUND),
        kept in any::<proptest::sample::Index>(),
    ) {
        let mut value = marker.to_vec();
        value.extend(&rest);
        value.truncate(kept.index(value.len().saturating_add(1)));
        let mut pdu = DICTATION_AUDIO_GATE.to_vec();
        pdu.extend(&value);
        let outcome = DictationPacket::parse(&pdu);
        if pdu.len() < dictation::AUDIO_PREFIX_LEN {
            let too_short = ParseError::TooShort { parser: Parser::DictationAudio, need: dictation::AUDIO_PREFIX_LEN, got: pdu.len() };
            prop_assert_eq!(outcome, Err(too_short));
        } else if value.starts_with(&DICTATION_AUDIO_MARKER) {
            prop_assert!(matches!(outcome, Ok(Some(DictationPacket::Audio(_)))), "{:?}", outcome);
        } else {
            let unknown = DictationPacket::Unknown { value: &value };
            prop_assert_eq!(outcome, Ok(Some(unknown)));
        }
    }

    // Any other opcode is not dictation, valid ATT or not, even on the two handles.
    #[test]
    fn dictation_reads_nothing_but_a_notification(
        opcode in prop_oneof![proptest::sample::select(&ATT_REFUSED_WHEN_MALFORMED), any::<u8>()]
            .prop_filter("the opcode is a notification", |opcode| *opcode != ATT_NOTIFICATION),
        gate in prop_oneof![Just(DICTATION_AUDIO_GATE), Just(DICTATION_STATUS_GATE), any::<[u8; 3]>()],
        rest in proptest::collection::vec(any::<u8>(), 0..VALUE_NOISE_BOUND),
        kept in any::<proptest::sample::Index>(),
    ) {
        let mut pdu = vec![opcode];
        pdu.extend(gate.iter().skip(1));
        pdu.extend(&rest);
        pdu.truncate(kept.index(pdu.len()).saturating_add(1));
        prop_assert_eq!(DictationPacket::parse(&pdu), Ok(None));
    }

    #[test]
    fn dictation_status_is_a_known_reply_only_when_every_byte_agrees(value in dictation_status_value()) {
        let mut pdu = DICTATION_STATUS_GATE.to_vec();
        pdu.extend(&value);
        let expected = if pdu == STARTED {
            Status::Started
        } else if pdu == STOPPED {
            Status::Stopped
        } else {
            Status::Other(&value)
        };
        prop_assert_eq!(DictationPacket::parse(&pdu), Ok(Some(DictationPacket::Status(expected))));
    }

    // Whole frames round trip; a cut notification yields each frame that fits and counts the rest.
    #[test]
    fn dictation_audio_round_trips_and_counts_a_cut_tail(
        packet_counter in prop_oneof![Just(0_u8), Just(u8::MAX), any::<u8>()],
        parts in proptest::collection::vec(dictation_frame(), 0..FRAME_COUNT_BOUND),
        cut in any::<proptest::sample::Index>(),
    ) {
        let frames: Vec<Frame<'_>> = parts.iter().map(|(flag, level, counter, data)| Frame { flag: *flag, level: *level, counter: *counter, data }).collect();
        let pdu = dictation::encode_audio(packet_counter, &frames).unwrap();
        let Ok(Some(DictationPacket::Audio(audio))) = DictationPacket::parse(&pdu) else {
            return Err(TestCaseError::fail("an encoded notification did not parse as audio"));
        };
        prop_assert_eq!(audio.packet_counter, packet_counter);
        prop_assert_eq!(usize::from(audio.declared_len).checked_add(dictation::AUDIO_PREFIX_LEN), Some(pdu.len()));
        prop_assert!(audio.length_matches());
        let mut seen = audio.frames();
        prop_assert_eq!(&seen.by_ref().collect::<Vec<_>>(), &frames);
        prop_assert_eq!(seen.trailing(), 0);

        let kept = cut.index(pdu.len()).max(dictation::AUDIO_PREFIX_LEN);
        let Ok(Some(DictationPacket::Audio(cut_audio))) = DictationPacket::parse(&pdu[..kept]) else {
            return Err(TestCaseError::fail("a cut notification with a whole prefix did not parse as audio"));
        };
        prop_assert_eq!(cut_audio.declared_len, audio.declared_len);
        prop_assert_eq!(cut_audio.length_matches(), kept == pdu.len());
        let mut cut_frames = cut_audio.frames();
        let whole: Vec<_> = cut_frames.by_ref().collect();
        prop_assert_eq!(whole.as_slice(), &frames[..whole.len()]);
        let rebuilt = dictation::encode_audio(packet_counter, &whole).unwrap();
        prop_assert_eq!(kept.checked_sub(rebuilt.len()), Some(cut_frames.trailing()));
        if let Some(one_more) = frames.get(..=whole.len()) {
            prop_assert!(dictation::encode_audio(packet_counter, one_more).unwrap().len() > kept, "a frame that fits was not yielded");
        }
    }
}
