//! Support, not proof: every way an arm ends, against the table of `super::Pods`.

use super::*;

#[test]
fn arming_finds_the_observed_table() {
    let (end, sent) = arm(&Pods::at(&Handles::OBSERVED));
    assert_eq!(end, done(Handles::OBSERVED));
    assert_eq!(sent[0], hex("08 01 00 ff ff 03 28"));
    // Declarations two a reply, then the next request starts past the last one listed.
    assert_eq!(sent[1], hex("08 05 00 ff ff 03 28"));
    let tail: Vec<_> = sent[sent.len() - 5..].to_vec();
    assert_eq!(
        tail,
        [
            hex("12 08 00 01 00"),
            hex("12 0b 00 01 00"),
            hex("12 0e 00 01 00"),
            hex("12 11 00 01 00"),
            hex("0a 03 00"),
        ]
    );
    let descriptors = &sent[sent.len() - 6];
    assert_eq!(descriptors[0], att::FIND_INFORMATION_REQUEST);
}

#[test]
fn the_descriptor_pass_asks_only_inside_the_service() {
    let (end, sent) = arm(&Pods::at(&Handles::OBSERVED));
    assert_eq!(end, done(Handles::OBSERVED));
    let passes: Vec<_> = sent
        .iter()
        .filter(|request| request[0] == att::FIND_INFORMATION_REQUEST)
        .collect();
    // From the handle after the first notifying value to the end of the table, since no declaration follows the last.
    assert_eq!(passes[0], &hex("04 08 00 ff ff"));
    assert!(passes.len() > 1, "a reply ends at each 128 bit identifier");
}

#[test]
fn a_table_at_other_handles_is_found_by_uuid() {
    let handles = shifted(SHIFT);
    assert_eq!(handles.start, 0x0027);
    let mut pods = Pods::at(&handles);
    // Another service in front, with a descriptor of its own that is not the stream's.
    pods.declarations
        .insert(0, (0x0002, 0x0003, FAKE_LONG_UUID));
    pods.descriptors.insert(0, 0x0004);
    let (end, _) = arm(&pods);
    assert_eq!(end, done(handles));
}

#[test]
fn a_table_without_the_service_is_no_service() {
    let mut pods = Pods::at(&Handles::OBSERVED);
    for (_, _, uuid) in &mut pods.declarations {
        *uuid = FAKE_LONG_UUID;
    }
    assert_eq!(arm(&pods).0, ArmStep::Failed(ArmFailure::NoService));

    pods.declarations.clear();
    let (end, sent) = arm(&pods);
    assert_eq!(end, ArmStep::Failed(ArmFailure::NoService));
    assert_eq!(sent.len(), 1);
}

#[test]
fn a_table_with_part_of_the_service_is_missing_characteristic() {
    for (missing, uuid) in SERVICE.iter().enumerate() {
        let mut pods = Pods::at(&Handles::OBSERVED);
        pods.declarations.remove(missing);
        let end = arm(&pods).0;
        let expected = ArmStep::Failed(ArmFailure::MissingCharacteristic);
        assert_eq!(end, expected, "without {uuid}");
    }
}

#[test]
fn a_missing_cccd_is_no_cccd() {
    for missing in 0..Handles::OBSERVED.cccds.len() {
        let mut pods = Pods::at(&Handles::OBSERVED);
        let gone = pods.descriptors.remove(missing);
        let end = arm(&pods).0;
        assert_eq!(end, ArmStep::Failed(ArmFailure::NoCccd), "{gone:#06x}");
    }
}

#[test]
fn a_descriptor_behind_the_next_declaration_is_not_this_characteristics() {
    let mut pods = Pods::at(&Handles::OBSERVED);
    // The event characteristic's descriptor is gone, and one more stands behind a declaration that follows it.
    pods.descriptors.pop();
    pods.declarations.push((0x0011, 0x0012, FAKE_LONG_UUID));
    pods.descriptors.push(0x0013);
    assert_eq!(arm(&pods).0, ArmStep::Failed(ArmFailure::NoCccd));
}

#[test]
fn a_codec_list_without_7_is_no_codec() {
    for codecs in [
        "01 01 00 04 06 02 01 01",
        "01 02 00 04 07 02 01 01",
        "01 01 00 00",
        "00",
        "02 02 00 02 07 00 01 00 02 06 00",
    ] {
        let mut pods = Pods::at(&Handles::OBSERVED);
        pods.codecs = hex(codecs);
        let end = arm(&pods).0;
        assert_eq!(end, ArmStep::Failed(ArmFailure::NoCodec), "{codecs}");
    }
}

#[test]
fn a_codec_list_with_7_on_the_stream_is_taken_wherever_it_stands() {
    for codecs in [
        "01 01 00 02 07 00",
        "01 01 00 06 06 02 01 01 07 00",
        "02 02 00 02 06 00 01 00 02 07 00",
        "01 01 00 04 07 02 01 01 ff",
    ] {
        let mut pods = Pods::at(&Handles::OBSERVED);
        pods.codecs = hex(codecs);
        assert_eq!(arm(&pods).0, done(Handles::OBSERVED), "{codecs}");
    }
}

#[test]
fn a_codec_list_with_a_length_past_its_end_is_garbled() {
    for codecs in [
        "01 01 00 05 07 02 01 01",
        "01 01 00 04 07 03 01 01",
        "01 01 00 03 07 02 01",
        "01 01 00 01 07",
        "02 01 00 04 07 02 01 01",
        "01 01 00",
        "01 01",
    ] {
        let mut pods = Pods::at(&Handles::OBSERVED);
        pods.codecs = hex(codecs);
        let end = arm(&pods).0;
        assert_eq!(end, ArmStep::Failed(ArmFailure::Garbled), "{codecs}");
    }
    let mut pods = Pods::at(&Handles::OBSERVED);
    pods.codecs = Vec::new();
    assert_eq!(arm(&pods).0, ArmStep::Failed(ArmFailure::Garbled));
}

#[test]
fn an_error_on_a_cccd_write_is_refused_with_its_code() {
    let mut pods = Pods::at(&Handles::OBSERVED);
    pods.refuse_writes = Some(WRITE_NOT_PERMITTED);
    let (end, sent) = arm(&pods);
    let refused = ArmFailure::Refused {
        request: att::WRITE_REQUEST,
        code: WRITE_NOT_PERMITTED,
    };
    assert_eq!(end, ArmStep::Failed(refused));
    assert_eq!(sent.last().unwrap(), &hex("12 08 00 01 00"));
}

#[test]
fn an_error_that_does_not_end_the_table_is_refused() {
    let (mut arming, _) = Arming::new();
    let refused = att::error_response(
        att::READ_BY_TYPE_REQUEST,
        0x0001,
        att::REQUEST_NOT_SUPPORTED,
    );
    let expected = ArmFailure::Refused {
        request: att::READ_BY_TYPE_REQUEST,
        code: att::REQUEST_NOT_SUPPORTED,
    };
    assert_eq!(arming.on_pdu(&refused), ArmStep::Failed(expected));
}

#[test]
fn a_table_that_never_ends_stops_at_the_bound() {
    let (mut arming, first) = Arming::new();
    let mut sent = vec![first];
    let end = loop {
        match arming.on_pdu(&endless(sent.last().unwrap())) {
            ArmStep::Send(request) => sent.push(request),
            end @ (ArmStep::Wait | ArmStep::Done { .. } | ArmStep::Failed(_)) => break end,
        }
        assert!(sent.len() <= STEP_LIMIT);
    };
    assert_eq!(end, ArmStep::Failed(ArmFailure::TooLong));
    assert_eq!(u32::try_from(sent.len()), Ok(ARM_MAX_REQUESTS));
    let last = u16::try_from(ARM_MAX_REQUESTS).unwrap().to_le_bytes();
    assert_eq!(sent.last().unwrap()[1..3], last);
}

/// How many requests of the walk and of the pass an arm sent.
fn asked(sent: &[Vec<u8>]) -> u32 {
    let walks = [att::READ_BY_TYPE_REQUEST, att::FIND_INFORMATION_REQUEST];
    let asked = sent.iter().filter(|request| walks.contains(&request[0]));
    u32::try_from(asked.count()).unwrap()
}

#[test]
fn a_table_that_takes_every_request_the_arm_may_send_is_still_armed() {
    let mut pods = Pods::at(&Handles::OBSERVED);
    // Two more declarations are one more reply of the walk, so one more request.
    let pad = |pods: &mut Pods, at: u16| {
        pods.declarations.push((at, at + 1, FAKE_LONG_UUID));
        pods.declarations.push((at + 2, at + 3, FAKE_LONG_UUID));
    };
    let mut next = 0x0100;
    loop {
        let (end, sent) = arm(&pods);
        assert_eq!(
            end,
            done(Handles::OBSERVED),
            "after {} requests",
            asked(&sent)
        );
        if asked(&sent) == ARM_MAX_REQUESTS {
            break;
        }
        pad(&mut pods, next);
        next += 4;
    }
    pad(&mut pods, next);
    let (end, sent) = arm(&pods);
    assert_eq!(end, ArmStep::Failed(ArmFailure::TooLong));
    assert_eq!(asked(&sent), ARM_MAX_REQUESTS);
}

#[test]
fn what_the_pods_send_unasked_leaves_the_request_out() {
    let pods = Pods::at(&Handles::OBSERVED);
    let (mut arming, first) = Arming::new();
    let unasked = [
        hex("1b 0d 00 01 00"),
        hex("1d 21 00 01"),
        hex("02 00 02"),
        hex("03 00 02"),
    ];
    let mut request = first;
    for _ in 0..STEP_LIMIT {
        for pdu in &unasked {
            assert_eq!(arming.on_pdu(pdu), ArmStep::Wait, "{pdu:02x?}");
        }
        match arming.on_pdu(&pods.answer(&request)) {
            ArmStep::Send(next) => request = next,
            end @ (ArmStep::Wait | ArmStep::Done { .. } | ArmStep::Failed(_)) => {
                assert_eq!(end, done(Handles::OBSERVED));
                break;
            }
        }
    }
    // An arm that is over has nothing more to say, whatever arrives.
    assert_eq!(arming.on_pdu(&hex("13")), ArmStep::Wait);
    assert_eq!(arming.on_pdu(&[]), ArmStep::Wait);
}

#[test]
fn a_reply_the_arm_cannot_follow_is_garbled() {
    let pods = Pods::at(&Handles::OBSERVED);
    let garbled = ArmStep::Failed(ArmFailure::Garbled);
    // One of each reply beside the request it answers; it answers no other request of the arm.
    let replies = [
        (
            hex("09 15 02 00 10 03 00 f0 f1 f2 f3 f4 f5 f6 f7 f8 f9 fa fb fc fd fe ff"),
            att::READ_BY_TYPE_REQUEST,
        ),
        (hex("05 01 08 00 02 29"), att::FIND_INFORMATION_REQUEST),
        (hex("13"), att::WRITE_REQUEST),
        (hex("0b 01 01 00 02 07 00"), att::READ_REQUEST),
    ];
    for (wrong, answered) in &replies {
        let (mut arming, first) = Arming::new();
        let mut request = first;
        let mut offered = Vec::new();
        let mut end = None;
        for _ in 0..STEP_LIMIT {
            let right = pods.answer(&request);
            if right[0] != wrong[0] {
                assert_eq!(arming.clone().on_pdu(wrong), garbled, "{request:02x?}");
                offered.push(request[0]);
            }
            match arming.on_pdu(&right) {
                ArmStep::Send(next) => request = next,
                over @ (ArmStep::Wait | ArmStep::Done { .. } | ArmStep::Failed(_)) => {
                    end = Some(over);
                    break;
                }
            }
        }
        assert_eq!(end, Some(done(Handles::OBSERVED)), "{wrong:02x?}");
        // The wrong reply went to every kind of request the arm sends but its own.
        for (_, other) in &replies {
            let reached = offered.contains(other);
            assert!(reached || other == answered, "{wrong:02x?} at {other:#04x}");
        }
    }

    for cut in [&[][..], &hex("09"), &hex("09 07 02"), &hex("01 08 01")] {
        let (mut arming, _) = Arming::new();
        assert_eq!(arming.on_pdu(cut), garbled, "{cut:02x?}");
    }
    // A Write Response has no body, so one with a byte is not the answer to the write that is out.
    let (mut arming, first) = Arming::new();
    let mut request = first;
    while request[0] != att::WRITE_REQUEST {
        let ArmStep::Send(next) = arming.on_pdu(&pods.answer(&request)) else {
            panic!("the arm ended before its first write, after {request:02x?}");
        };
        request = next;
    }
    assert_eq!(arming.on_pdu(&hex("13 00")), garbled);
    // An entry that is no declaration gives the walk no value handle to keep.
    let (mut arming, _) = Arming::new();
    assert_eq!(arming.on_pdu(&hex("09 04 02 00 03 00")), garbled);
}

#[test]
fn a_peer_repeating_one_reply_cannot_keep_the_arm_going() {
    let pods = Pods::at(&Handles::OBSERVED);
    let (mut arming, first) = Arming::new();
    let reply = pods.answer(&first);
    assert!(matches!(arming.on_pdu(&reply), ArmStep::Send(_)));
    assert_eq!(arming.on_pdu(&reply), ArmStep::Failed(ArmFailure::Garbled));
}

#[test]
fn a_declaration_at_0xffff_ends_the_table() {
    let mut pods = Pods::at(&Handles::OBSERVED);
    pods.declarations.push((0xffff, 0xffff, FAKE_LONG_UUID));
    let (end, sent) = arm(&pods);
    assert_eq!(end, done(Handles::OBSERVED));
    let walks = sent
        .iter()
        .filter(|request| request[0] == att::READ_BY_TYPE_REQUEST);
    // Three replies of two and one of one, and no request past the last handle.
    assert_eq!(walks.count(), 4);
}
