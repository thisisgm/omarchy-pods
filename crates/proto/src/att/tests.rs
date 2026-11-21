//! Requests, replies and unasked values, every byte built here from the layouts the specification gives.

use super::*;

/// The range that reads a whole attribute table.
const FIRST_HANDLE: u16 = 0x0001;
const LAST_HANDLE: u16 = 0xffff;
/// Stands in for a vendor's 128 bit identifier; no real one is a run of counting bytes.
const FAKE_LONG_UUID: [u8; LONG_UUID_LEN] = [
    0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd, 0xfe, 0xff,
];
/// A second made up 128 bit identifier, so two entries cannot be told apart by position alone.
const OTHER_FAKE_LONG_UUID: [u8; LONG_UUID_LEN] = [
    0xe0, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xeb, 0xec, 0xed, 0xee, 0xef,
];
/// One declaration entry: handle 0x0002, notify and write without response, value handle 0x0003, identifier 0x2b2a.
const SHORT_DECLARATION: [u8; 7] = [0x02, 0x00, 0x14, 0x03, 0x00, 0x2a, 0x2b];
/// One handle and 16 bit identifier pair: handle 0x0001, identifier 0x2800.
const SHORT_PAIR: [u8; 4] = [0x01, 0x00, 0x00, 0x28];
/// How many whole entries the cut lists carry before their cut one.
const WHOLE_ENTRIES: usize = 2;
/// How many of the 31 rows of the opcode summary are requests.
const REQUEST_COUNT: usize = 12;

/// Vol 3, Part F, section 3.4.8, Table 3.43: every PDU name and its opcode; a name ending in REQ is a request.
const OPCODE_SUMMARY: [(&str, u8); 31] = [
    ("ATT_ERROR_RSP", 0x01),
    ("ATT_EXCHANGE_MTU_REQ", 0x02),
    ("ATT_EXCHANGE_MTU_RSP", 0x03),
    ("ATT_FIND_INFORMATION_REQ", 0x04),
    ("ATT_FIND_INFORMATION_RSP", 0x05),
    ("ATT_FIND_BY_TYPE_VALUE_REQ", 0x06),
    ("ATT_FIND_BY_TYPE_VALUE_RSP", 0x07),
    ("ATT_READ_BY_TYPE_REQ", 0x08),
    ("ATT_READ_BY_TYPE_RSP", 0x09),
    ("ATT_READ_REQ", 0x0a),
    ("ATT_READ_RSP", 0x0b),
    ("ATT_READ_BLOB_REQ", 0x0c),
    ("ATT_READ_BLOB_RSP", 0x0d),
    ("ATT_READ_MULTIPLE_REQ", 0x0e),
    ("ATT_READ_MULTIPLE_RSP", 0x0f),
    ("ATT_READ_BY_GROUP_TYPE_REQ", 0x10),
    ("ATT_READ_BY_GROUP_TYPE_RSP", 0x11),
    ("ATT_WRITE_REQ", 0x12),
    ("ATT_WRITE_RSP", 0x13),
    ("ATT_WRITE_CMD", 0x52),
    ("ATT_PREPARE_WRITE_REQ", 0x16),
    ("ATT_PREPARE_WRITE_RSP", 0x17),
    ("ATT_EXECUTE_WRITE_REQ", 0x18),
    ("ATT_EXECUTE_WRITE_RSP", 0x19),
    ("ATT_READ_MULTIPLE_VARIABLE_REQ", 0x20),
    ("ATT_READ_MULTIPLE_VARIABLE_RSP", 0x21),
    ("ATT_MULTIPLE_HANDLE_VALUE_NTF", 0x23),
    ("ATT_HANDLE_VALUE_NTF", 0x1b),
    ("ATT_HANDLE_VALUE_IND", 0x1d),
    ("ATT_HANDLE_VALUE_CFM", 0x1e),
    ("ATT_SIGNED_WRITE_CMD", 0xd2),
];

fn information(pdu: &[u8]) -> Information<'_> {
    match AttPdu::parse(pdu) {
        Ok(AttPdu::Information(pairs)) => pairs,
        other => panic!("not a Find Information reply: {other:?}"),
    }
}

fn by_type(pdu: &[u8]) -> ByType<'_> {
    match AttPdu::parse(pdu) {
        Ok(AttPdu::ByType(entries)) => entries,
        other => panic!("not a Read By Type reply: {other:?}"),
    }
}

/// A handle and 128 bit identifier pair as it sits in a format 2 reply.
fn long_pair(handle: u16, uuid: &[u8; LONG_UUID_LEN]) -> Vec<u8> {
    let mut pair = handle.to_le_bytes().to_vec();
    pair.extend(uuid);
    pair
}

/// A list reply: its two byte prefix, `WHOLE_ENTRIES` copies of `entry`, then the first `kept` bytes of one more.
fn cut_list(prefix: [u8; 2], entry: &[u8], kept: usize) -> Vec<u8> {
    let mut pdu = prefix.to_vec();
    for _ in 0..WHOLE_ENTRIES {
        pdu.extend(entry);
    }
    pdu.extend(&entry[..kept]);
    pdu
}

fn unknown(field: &'static str, value: u8) -> ParseError {
    ParseError::UnknownValue {
        parser: Parser::Att,
        field,
        value,
    }
}

fn empty_list(list_len: usize) -> ParseError {
    ParseError::EmptyList {
        parser: Parser::Att,
        list_len,
    }
}

fn too_short(need: usize, got: usize) -> ParseError {
    ParseError::TooShort {
        parser: Parser::Att,
        need,
        got,
    }
}

#[test]
fn requests_are_the_documented_bytes() {
    assert_eq!(
        find_information_request(FIRST_HANDLE, LAST_HANDLE),
        [0x04, 0x01, 0x00, 0xff, 0xff]
    );
    assert_eq!(
        find_information_request(0x0102, 0x0304),
        [0x04, 0x02, 0x01, 0x04, 0x03]
    );
    assert_eq!(
        read_by_type_request(FIRST_HANDLE, LAST_HANDLE, CHARACTERISTIC_DECLARATION),
        [0x08, 0x01, 0x00, 0xff, 0xff, 0x03, 0x28]
    );
    assert_eq!(
        read_by_type_request(0x0102, 0x0304, 0x0506),
        [0x08, 0x02, 0x01, 0x04, 0x03, 0x06, 0x05]
    );
}

#[test]
fn error_response_is_the_documented_bytes() {
    assert_eq!(
        error_response(0x02, 0x0000, REQUEST_NOT_SUPPORTED),
        [0x01, 0x02, 0x00, 0x00, 0x06]
    );
    assert_eq!(
        error_response(0x08, 0x0102, ATTRIBUTE_NOT_FOUND),
        [0x01, 0x08, 0x02, 0x01, 0x0a]
    );
}

#[test]
fn an_error_response_reads_back_as_the_error_it_was_built_from() {
    let built = error_response(0x08, 0x0102, ATTRIBUTE_NOT_FOUND);
    let expected = AttPdu::Error {
        request: 0x08,
        handle: 0x0102,
        code: ATTRIBUTE_NOT_FOUND,
    };
    assert_eq!(AttPdu::parse(&built), Ok(expected));
}

#[test]
fn information_with_16_bit_identifiers_lists_every_handle() {
    let pdu = [
        0x05, 0x01, 0x01, 0x00, 0x00, 0x28, 0x02, 0x00, 0x03, 0x28, 0x21, 0x43, 0x2a, 0x2b,
    ];
    let mut pairs = information(&pdu);
    let seen: Vec<_> = pairs.by_ref().collect();
    let expected = [
        (0x0001, 0x2800),
        (0x0002, CHARACTERISTIC_DECLARATION),
        (0x4321, 0x2b2a),
    ]
    .map(|(handle, uuid)| HandleInfo {
        handle,
        uuid: Uuid::Short(uuid),
    });
    assert_eq!(seen, expected);
    assert_eq!(pairs.trailing(), 0);
}

#[test]
fn information_with_128_bit_identifiers_lists_every_handle() {
    let mut pdu = vec![0x05, 0x02];
    pdu.extend(long_pair(0x0004, &FAKE_LONG_UUID));
    pdu.extend(long_pair(0x4321, &OTHER_FAKE_LONG_UUID));
    let mut pairs = information(&pdu);
    let seen: Vec<_> = pairs.by_ref().collect();
    let expected = [
        HandleInfo {
            handle: 0x0004,
            uuid: Uuid::Long(&FAKE_LONG_UUID),
        },
        HandleInfo {
            handle: 0x4321,
            uuid: Uuid::Long(&OTHER_FAKE_LONG_UUID),
        },
    ];
    assert_eq!(seen, expected);
    assert_eq!(pairs.trailing(), 0);
}

#[test]
fn an_unknown_information_format_is_an_error() {
    for format in [0x00, 0x03, 0xff] {
        let pdu = [0x05, format, 0x01, 0x00, 0x00, 0x28];
        assert_eq!(
            AttPdu::parse(&pdu),
            Err(unknown("information format", format))
        );
    }
    assert_eq!(
        unknown("information format", 0x03).to_string(),
        "attribute protocol packet has an unknown information format value 0x03"
    );
}

#[test]
fn by_type_entries_are_cut_at_the_declared_length() {
    let second = [0x04, 0x00, 0x02, 0x05, 0x00, 0x2c, 0x2d];
    let mut pdu = vec![0x09, 0x07];
    pdu.extend(SHORT_DECLARATION);
    pdu.extend(second);
    let mut entries = by_type(&pdu);
    let seen: Vec<_> = entries.by_ref().collect();
    assert_eq!(seen, [&SHORT_DECLARATION[..], &second[..]]);
    assert_eq!(entries.trailing(), 0);

    pdu[1] = 0x02;
    let pairs: Vec<_> = by_type(&pdu).collect();
    let body = [SHORT_DECLARATION, second].concat();
    assert_eq!(pairs, body.chunks(2).collect::<Vec<_>>());
}

#[test]
fn a_zero_entry_length_is_an_error_not_a_loop() {
    for entry_len in [0x00, 0x01] {
        let pdu = [0x09, entry_len, 0xaa, 0xbb, 0xcc, 0xdd];
        assert_eq!(AttPdu::parse(&pdu), Err(unknown("entry length", entry_len)));
    }
    assert_eq!(
        unknown("entry length", 0x00).to_string(),
        "attribute protocol packet has an unknown entry length value 0x00"
    );
    let shortest = [0x09, 0x02, 0xaa, 0xbb, 0xcc, 0xdd];
    let seen: Vec<_> = by_type(&shortest).collect();
    assert_eq!(seen, [[0xaa, 0xbb], [0xcc, 0xdd]]);
}

#[test]
fn a_characteristic_declaration_gives_properties_and_value_handle() {
    let mut pdu = vec![0x09, 0x07];
    pdu.extend(SHORT_DECLARATION);
    let entry = by_type(&pdu).next().unwrap();
    let short = characteristic(entry).unwrap();
    let expected = Characteristic {
        declaration: 0x0002,
        properties: 0x14,
        value_handle: 0x0003,
        uuid: Uuid::Short(0x2b2a),
    };
    assert_eq!(short, expected);
    assert_eq!(
        short.properties,
        PROPERTY_NOTIFY | PROPERTY_WRITE_WITHOUT_RESPONSE
    );

    let mut long_entry = vec![0x21, 0x43, PROPERTY_NOTIFY, 0x22, 0x43];
    long_entry.extend(FAKE_LONG_UUID);
    let expected = Characteristic {
        declaration: 0x4321,
        properties: PROPERTY_NOTIFY,
        value_handle: 0x4322,
        uuid: Uuid::Long(&FAKE_LONG_UUID),
    };
    assert_eq!(characteristic(&long_entry), Some(expected));
}

#[test]
fn an_entry_of_another_length_is_not_a_characteristic() {
    let mut long_entry = vec![0x21, 0x43, PROPERTY_NOTIFY, 0x22, 0x43];
    long_entry.extend(FAKE_LONG_UUID);
    long_entry.push(0x00);
    for len in 0..=long_entry.len() {
        let is_a_declaration = len == SHORT_DECLARATION.len() || len == long_entry.len() - 1;
        assert_eq!(
            characteristic(&long_entry[..len]).is_some(),
            is_a_declaration,
            "{len} bytes"
        );
    }
}

#[test]
fn attribute_not_found_is_an_error_pdu_not_a_parse_error() {
    let pdu = [0x01, 0x04, 0x02, 0x01, 0x0a];
    let expected = AttPdu::Error {
        request: 0x04,
        handle: 0x0102,
        code: ATTRIBUTE_NOT_FOUND,
    };
    assert_eq!(AttPdu::parse(&pdu), Ok(expected));
}

#[test]
fn an_error_response_of_another_length_is_refused() {
    let one_too_many = [0x01, 0x04, 0x02, 0x01, 0x0a, 0x00];
    for got in [1, 4, 6] {
        let refused = ParseError::WrongLength {
            parser: Parser::Att,
            expected: 5,
            got,
        };
        assert_eq!(AttPdu::parse(&one_too_many[..got]), Err(refused));
    }
}

#[test]
fn notification_and_indication_carry_handle_and_value() {
    let notification = [0x1b, 0x21, 0x43, 0xb8, 0x00, 0x01];
    let expected = AttPdu::Notification {
        handle: 0x4321,
        value: &[0xb8, 0x00, 0x01],
    };
    assert_eq!(AttPdu::parse(&notification), Ok(expected));

    let indication = [0x1d, 0x42, 0x00, 0xaa];
    let expected = AttPdu::Indication {
        handle: 0x0042,
        value: &[0xaa],
    };
    assert_eq!(AttPdu::parse(&indication), Ok(expected));

    let empty_value = AttPdu::Indication {
        handle: 0x0042,
        value: &[],
    };
    assert_eq!(AttPdu::parse(&indication[..3]), Ok(empty_value));
    assert_eq!(CONFIRMATION, [0x1e]);
}

#[test]
fn a_pdu_cut_before_its_fixed_part_is_too_short() {
    assert_eq!(AttPdu::parse(&[]), Err(too_short(1, 0)));
    assert_eq!(AttPdu::parse(&[0x05]), Err(too_short(2, 1)));
    assert_eq!(AttPdu::parse(&[0x09]), Err(too_short(2, 1)));
    for opcode in [0x1b, 0x1d] {
        assert_eq!(AttPdu::parse(&[opcode]), Err(too_short(3, 1)));
        assert_eq!(AttPdu::parse(&[opcode, 0x21]), Err(too_short(3, 2)));
    }
    assert_eq!(
        too_short(2, 1).to_string(),
        "attribute protocol packet too short: need 2 bytes, got 1"
    );
}

#[test]
fn every_request_opcode_is_classed_as_a_request() {
    let requests: Vec<u8> = OPCODE_SUMMARY
        .iter()
        .filter(|(name, _)| name.ends_with("_REQ"))
        .map(|(_, opcode)| *opcode)
        .collect();
    assert_eq!(requests.len(), REQUEST_COUNT);
    for opcode in u8::MIN..=u8::MAX {
        let pdu = [opcode, 0x01, 0x00, 0xff, 0xff];
        let classed =
            matches!(AttPdu::parse(&pdu), Ok(AttPdu::Request { opcode: seen }) if seen == opcode);
        assert_eq!(classed, requests.contains(&opcode), "opcode {opcode:#04x}");
    }
}

#[test]
fn an_opcode_with_no_reader_is_kept_whole() {
    let read_reply = [0x0b, 0xaa, 0xbb];
    let expected = AttPdu::Other {
        opcode: 0x0b,
        body: &[0xaa, 0xbb],
    };
    assert_eq!(AttPdu::parse(&read_reply), Ok(expected));
    let confirmation = AttPdu::Other {
        opcode: 0x1e,
        body: &[],
    };
    assert_eq!(AttPdu::parse(&CONFIRMATION), Ok(confirmation));
}

#[test]
fn trailing_bytes_are_counted() {
    let long = long_pair(0x0004, &FAKE_LONG_UUID);
    for (prefix, entry) in [
        ([0x05, 0x01], &SHORT_PAIR[..]),
        ([0x05, 0x02], &long[..]),
        ([0x09, 0x07], &SHORT_DECLARATION[..]),
    ] {
        for kept in 0..entry.len() {
            let pdu = cut_list(prefix, entry, kept);
            let counted = match AttPdu::parse(&pdu) {
                Ok(AttPdu::Information(mut pairs)) => (pairs.by_ref().count(), pairs.trailing()),
                Ok(AttPdu::ByType(mut entries)) => (entries.by_ref().count(), entries.trailing()),
                other => panic!("not a list reply: {other:?}"),
            };
            assert_eq!(counted, (WHOLE_ENTRIES, kept), "{prefix:02x?} kept {kept}");
        }
    }
}

#[test]
fn a_find_information_reply_with_no_whole_pair_is_refused() {
    let long = long_pair(0x0004, &FAKE_LONG_UUID);
    for (prefix, pair) in [([0x05, 0x01], &SHORT_PAIR[..]), ([0x05, 0x02], &long[..])] {
        for kept in 0..pair.len() {
            let pdu = [&prefix[..], &pair[..kept]].concat();
            assert_eq!(AttPdu::parse(&pdu), Err(empty_list(kept)), "{pdu:02x?}");
        }
        let shortest = [&prefix[..], pair].concat();
        assert_eq!(information(&shortest).count(), 1);
    }
    assert_eq!(
        empty_list(3).to_string(),
        "attribute protocol packet has an empty list: its 3 bytes hold no whole entry"
    );
}

#[test]
fn a_read_by_type_reply_with_no_whole_entry_is_refused() {
    let prefix = [0x09, 0x07];
    for kept in 0..SHORT_DECLARATION.len() {
        let pdu = [&prefix[..], &SHORT_DECLARATION[..kept]].concat();
        assert_eq!(AttPdu::parse(&pdu), Err(empty_list(kept)), "{pdu:02x?}");
    }
    let shortest = [&prefix[..], &SHORT_DECLARATION[..]].concat();
    assert_eq!(by_type(&shortest).count(), 1);
}
