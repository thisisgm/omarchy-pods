//! Built here: the protocol bytes, and frames of a table of contents byte then counting bytes.

use super::*;
use crate::att::AttPdu;
use crate::doap;
use crate::fixture::hex;

/// Counter 0x2a, declared length 8, one frame: flag 0x80, level 0xd5, counter 0x0104, length 3.
const ONE_FRAME_NOTIFICATION: &str = "1b 0d 00 01 00 2a 08 00 80 d5 04 01 03 b8 10 11";
/// The two values a flag byte is known to take.
const FLAGS: [u8; 2] = [0x80, 0x00];
/// Level byte of the first generated frame; later frames count up from it.
const FIRST_LEVEL: u8 = 0xb9;
/// Offset of the low handle byte in an ATT PDU; the high byte follows it.
const HANDLE_AT: usize = 1;
/// Offset of the value in a notification; also the shortest notification's length.
const VALUE_AT: usize = 3;
const PACKET_COUNTER_AT: usize = 5;
/// Offset of the low byte of the declared length in an audio notification.
const DECLARED_LEN_AT: usize = 6;
const KEPT_DATA_BYTES: usize = 3;
/// The longest frame one length byte can say.
const LONGEST_FRAME: usize = 255;
/// The most frame bytes the two length bytes can say.
const LONGEST_BODY: usize = 65535;
/// Longest frames, 260 bytes each with the prefix, that fit under `LONGEST_BODY` with 15 to spare.
const FULL_FRAMES: usize = 252;
/// Data bytes of the last frame that bring the body to exactly `LONGEST_BODY`.
const LAST_DATA_AT_THE_LIMIT: usize = 10;
/// Data bytes of the last frame that bring the body one byte past `LONGEST_BODY`.
const LAST_DATA_PAST_THE_LIMIT: usize = 11;
/// One more longest frame than fits: 253 of them are 65780 frame bytes, past `LONGEST_BODY`.
const FRAMES_PAST_THE_LIMIT: usize = FULL_FRAMES + 1;

// Sample output for (0x10, 3): `b8 10 11`, the table of contents byte then counting bytes.
fn codec_bytes(seed: u8, len: usize) -> Vec<u8> {
    let counting = std::iter::successors(Some(seed), |byte| Some(byte.wrapping_add(1)));
    std::iter::once(TOC_CELT_WIDEBAND_20MS_MONO)
        .chain(counting)
        .take(len)
        .collect()
}

/// Counters from `first_counter`, wrapping; flags alternate, levels rise, so every prefix differs.
fn frames(first_counter: u16, payloads: &[Vec<u8>]) -> Vec<Frame<'_>> {
    let counters = std::iter::successors(Some(first_counter), |c| Some(c.wrapping_add(1)));
    payloads
        .iter()
        .zip(counters)
        .zip(FIRST_LEVEL..)
        .zip(FLAGS.into_iter().cycle())
        .map(|(((data, counter), level), flag)| Frame {
            flag,
            level,
            counter,
            data,
        })
        .collect()
}

/// The bytes one frame takes on the wire: flag, level, counter, length, data.
fn frame_record(frame: Frame<'_>) -> Vec<u8> {
    encode_audio(&Handles::OBSERVED, 0, &[frame])
        .unwrap()
        .split_off(AUDIO_PREFIX_LEN)
}

fn notification_cut_into_the_last_frame(whole: &[Frame<'_>; 4], kept: usize) -> Vec<u8> {
    let [first, second, third, last] = *whole;
    let mut pdu = encode_audio(&Handles::OBSERVED, 0, &[first, second, third]).unwrap();
    pdu.extend_from_slice(&frame_record(last)[..kept]);
    pdu
}

/// One frame with the flag and level bytes of `ONE_FRAME_NOTIFICATION`.
fn frame(counter: u16, data: &[u8]) -> Frame<'_> {
    Frame {
        flag: 0x80,
        level: 0xd5,
        counter,
        data,
    }
}

fn four_payloads() -> [Vec<u8>; 4] {
    [
        codec_bytes(0x00, 74),
        codec_bytes(0x40, 80),
        codec_bytes(0x80, 81),
        codec_bytes(0xc0, 94),
    ]
}

fn audio(pdu: &[u8]) -> AudioNotification<'_> {
    match DictationPacket::parse(pdu, &Handles::OBSERVED) {
        Ok(Some(DictationPacket::Audio(audio))) => audio,
        other => panic!("not an audio notification: {other:?}"),
    }
}

// Sample input: `52 07 00 01 00`, whose handle is the little endian 0x0007 after the opcode.
fn handle_of(pdu: &[u8]) -> u16 {
    u16::from_le_bytes([pdu[HANDLE_AT], pdu[VALUE_AT - 1]])
}

#[test]
fn the_markers_and_the_two_replies_are_the_documented_bytes() {
    assert_eq!(
        SESSION_ACTIVE.to_vec(),
        hex("04 00 04 00 4b 00 03 00 03 01 03")
    );
    assert_eq!(
        SESSION_IDLE.to_vec(),
        hex("04 00 04 00 4b 00 03 00 03 00 00")
    );
    assert_eq!(
        STARTED.to_vec(),
        hex("1b 10 00 01 00 02 08 00 00 80 bf 00 00 00 00")
    );
    assert_eq!(STOPPED.to_vec(), hex("1b 10 00 01 00 03 00"));
}

#[test]
fn every_pdu_is_on_its_handle_of_the_observed_table() {
    let audio = encode_audio(&Handles::OBSERVED, 0, &[]).unwrap();
    for (pdu, handle) in [
        (&STARTED[..], Handles::OBSERVED.event),
        (&STOPPED[..], Handles::OBSERVED.event),
        (&audio[..], Handles::OBSERVED.send_data),
    ] {
        assert_eq!(handle_of(pdu), handle, "{pdu:02x?}");
    }
}

fn event(value: &str) -> Option<DictationPacket<'static>> {
    let mut pdu = hex("1b 10 00");
    pdu.extend(hex(value));
    match DictationPacket::parse(&pdu, &Handles::OBSERVED) {
        Ok(Some(DictationPacket::Event(event))) => Some(DictationPacket::Event(event)),
        Ok(Some(DictationPacket::Unknown { handle, value })) => {
            assert_eq!((handle, value), (0x0010, &pdu[VALUE_AT..]));
            None
        }
        other => panic!("neither an event nor unknown: {other:?}"),
    }
}

#[test]
fn an_event_reads_its_code_and_what_the_code_carries() {
    for (value, expected) in [
        ("01 00 01 01 02", Event::SiriActivation(0x02)),
        (
            "01 00 02 08 00 00 80 bf 00 00 00 00",
            Event::Score { bits: 0xbf80_0000 },
        ),
        (
            "01 00 02 04 11 22 33 44",
            Event::Score { bits: 0x4433_2211 },
        ),
        ("01 00 03 00", Event::StreamingEnd),
        ("01 00 04 01 07", Event::SiriState(0x07)),
        ("01 00 05 00", Event::SiriCancel),
        ("01 00 06 01 02", Event::Button(0x02)),
        ("01 00 07 00", Event::Other { code: 0x07 }),
        ("01 00 ff 02 aa bb", Event::Other { code: 0xff }),
        // Bytes behind the declared data are not the event's.
        ("01 00 03 00 99", Event::StreamingEnd),
        ("01 00 06 01 01 99", Event::Button(0x01)),
    ] {
        assert_eq!(
            event(value),
            Some(DictationPacket::Event(expected)),
            "{value}"
        );
    }
}

#[test]
fn an_event_cut_short_or_of_another_stream_is_unknown() {
    for value in [
        "01",
        "01 00",
        "01 00 03",
        "02 00 03 00",
        "01 01 03 00",
        "01 00 02 08 00 00 80 bf 00 00 00",
        "01 00 02 03 00 00 80",
        "01 00 02 00",
        "01 00 01 00",
        "01 00 04 00",
        "01 00 06 00",
        "01 00 06 01",
        "01 00 07 01",
    ] {
        assert_eq!(event(value), None, "{value}");
    }
}

#[test]
fn a_stop_notification_gives_its_reason() {
    let handles = Handles::OBSERVED;
    for reason in [0x00, 0x06, 0xff] {
        let pdu = [0x1b, 0x0a, 0x00, 0x01, 0x00, reason];
        let stopped = DictationPacket::Stopped { reason };
        assert_eq!(DictationPacket::parse(&pdu, &handles), Ok(Some(stopped)));
    }
    for other in [
        "1b 0a 00",
        "1b 0a 00 01",
        "1b 0a 00 01 00",
        "1b 0a 00 02 00 06",
        "1b 0a 00 01 00 06 00",
    ] {
        let pdu = hex(other);
        let unknown = DictationPacket::Unknown {
            handle: 0x000a,
            value: &pdu[VALUE_AT..],
        };
        assert_eq!(DictationPacket::parse(&pdu, &handles), Ok(Some(unknown)));
    }
}

#[test]
fn the_handles_given_decide_what_a_notification_is() {
    let moved = Handles {
        stop: 0x002a,
        send_data: 0x002d,
        event: 0x0030,
        ..Handles::OBSERVED
    };
    let audio = hex("1b 2d 00 01 00 2a 00 00");
    let ended = hex("1b 30 00 01 00 03 00");
    let refused = hex("1b 2a 00 01 00 06");
    assert!(matches!(
        DictationPacket::parse(&audio, &moved),
        Ok(Some(DictationPacket::Audio(_)))
    ));
    assert_eq!(
        DictationPacket::parse(&ended, &moved),
        Ok(Some(DictationPacket::Event(Event::StreamingEnd)))
    );
    assert_eq!(
        DictationPacket::parse(&refused, &moved),
        Ok(Some(DictationPacket::Stopped { reason: 0x06 }))
    );
    for pdu in [&audio, &ended, &refused] {
        assert_eq!(DictationPacket::parse(pdu, &Handles::OBSERVED), Ok(None));
    }
    let observed_refusal = hex("1b 0a 00 01 00 06");
    for observed in [
        &hex(ONE_FRAME_NOTIFICATION)[..],
        &STARTED,
        &STOPPED,
        &observed_refusal,
    ] {
        assert_eq!(DictationPacket::parse(observed, &moved), Ok(None));
    }
    assert_eq!(encode_audio(&moved, 0x2a, &[]), Ok(audio));
}

#[test]
fn accepts_an_audio_notification() {
    let data = codec_bytes(0x10, 81);
    let frame = frame(0x0104, &data);
    let pdu = encode_audio(&Handles::OBSERVED, 0x2a, &[frame]).unwrap();
    assert_eq!(pdu.len(), 94);
    let audio = audio(&pdu);
    assert_eq!(audio.packet_counter, 0x2a);
    assert_eq!(audio.declared_len, 86);
    assert!(audio.length_matches());
    let mut seen = audio.frames();
    assert_eq!(seen.by_ref().collect::<Vec<_>>(), [frame]);
    assert_eq!(seen.trailing(), 0);
}

#[test]
fn demuxes_every_frame_of_a_burst_packet() {
    let payloads = [
        codec_bytes(0x00, 74),
        codec_bytes(0x40, 80),
        codec_bytes(0x80, 81),
        codec_bytes(0xc0, 94),
        Vec::new(),
    ];
    let whole = frames(4, &payloads);
    assert_eq!(
        whole.iter().map(|f| f.counter).collect::<Vec<_>>(),
        [4, 5, 6, 7, 8]
    );
    let pdu = encode_audio(&Handles::OBSERVED, 0x07, &whole).unwrap();
    assert_eq!(
        pdu[PACKET_COUNTER_AT..AUDIO_PREFIX_LEN],
        [0x07, 0x62, 0x01],
        "counter, then 354 little endian"
    );
    let audio = audio(&pdu);
    assert_eq!(audio.declared_len, 354);
    assert!(audio.length_matches());
    let mut seen = audio.frames();
    assert_eq!(seen.by_ref().collect::<Vec<_>>(), whole);
    assert_eq!(seen.trailing(), 0);
}

#[test]
fn stops_on_a_truncated_frame() {
    let payloads = four_payloads();
    let whole: [Frame<'_>; 4] = frames(0, &payloads).try_into().unwrap();
    let pdu = notification_cut_into_the_last_frame(&whole, FRAME_PREFIX_LEN + KEPT_DATA_BYTES);
    let seen: Vec<_> = audio(&pdu).frames().collect();
    assert_eq!(seen, whole[..3]);
}

#[test]
fn trailing_bytes_are_counted() {
    let payloads = four_payloads();
    let whole: [Frame<'_>; 4] = frames(0, &payloads).try_into().unwrap();
    let last_record_len = frame_record(whole[3]).len();
    for kept in 0..last_record_len {
        let pdu = notification_cut_into_the_last_frame(&whole, kept);
        let mut seen = audio(&pdu).frames();
        assert_eq!(seen.by_ref().count(), 3, "kept {kept}");
        assert_eq!(seen.trailing(), kept, "kept {kept}");
    }
    let mut hostile = hex(ONE_FRAME_NOTIFICATION);
    hostile.extend(hex("80 d5 05 01 ff b8 10"));
    let mut seen = audio(&hostile).frames();
    assert_eq!(seen.by_ref().count(), 1);
    assert_eq!(seen.trailing(), 7);
}

#[test]
fn trailing_before_the_end_counts_the_frames_not_yet_yielded() {
    let payloads = four_payloads();
    let whole = frames(0, &payloads);
    let record_lens: Vec<usize> = whole.iter().map(|f| frame_record(*f).len()).collect();
    let pdu = encode_audio(&Handles::OBSERVED, 0, &whole).unwrap();
    let mut seen = audio(&pdu).frames();
    for yielded in 0..=whole.len() {
        let not_yielded: usize = record_lens[yielded..].iter().sum();
        assert_eq!(seen.trailing(), not_yielded, "after {yielded} frames");
        assert_eq!(seen.next(), whole.get(yielded).copied());
    }
    assert_eq!(seen.trailing(), 0);
}

#[test]
fn emits_nothing_from_a_prefix_only_notification() {
    let pdu = encode_audio(&Handles::OBSERVED, 0x2a, &[]).unwrap();
    assert_eq!(pdu, hex("1b 0d 00 01 00 2a 00 00"));
    assert_eq!(pdu.len(), AUDIO_PREFIX_LEN);
    let audio = audio(&pdu);
    assert_eq!(audio.declared_len, 0);
    assert!(audio.length_matches());
    let mut seen = audio.frames();
    assert_eq!(seen.next(), None);
    assert_eq!(seen.trailing(), 0);
}

#[test]
fn declared_length_is_kept_beside_the_real_one() {
    let pdu = hex(ONE_FRAME_NOTIFICATION);
    let whole = audio(&pdu);
    assert_eq!(whole.declared_len, 8);
    assert!(whole.length_matches());
    let one_byte_lost = audio(&pdu[..pdu.len() - 1]);
    assert_eq!(one_byte_lost.packet_counter, 0x2a);
    assert_eq!(one_byte_lost.declared_len, 8);
    assert!(!one_byte_lost.length_matches());
    let mut frames = one_byte_lost.frames();
    assert_eq!(frames.next(), None);
    assert_eq!(frames.trailing(), 7);
    let mut one_frame_more = pdu.clone();
    one_frame_more.extend(hex("80 d5 05 01 01 b8"));
    let past_the_declared_length = audio(&one_frame_more);
    assert_eq!(past_the_declared_length.declared_len, 8);
    assert!(!past_the_declared_length.length_matches());
    let mut walked = past_the_declared_length.frames();
    let both = [frame(0x0104, &[0xb8, 0x10, 0x11]), frame(0x0105, &[0xb8])];
    assert_eq!(walked.by_ref().collect::<Vec<_>>(), both);
    assert_eq!(walked.trailing(), 0);
    let length_only = hex("1b 0d 00 01 00 2a 02 01");
    let empty = audio(&length_only);
    assert_eq!(empty.declared_len, 0x0102);
    assert!(!empty.length_matches());
}

#[test]
fn a_notification_on_another_handle_is_not_dictation() {
    let audio = hex(ONE_FRAME_NOTIFICATION);
    for handle in [
        Handles::OBSERVED.start,
        Handles::OBSERVED.supported_codecs,
        0x000c,
        0x000e,
        0x0d00,
        0x010d,
        0x1000,
        0x0110,
    ] {
        let mut pdu = audio.clone();
        pdu[HANDLE_AT..VALUE_AT].copy_from_slice(&handle.to_le_bytes());
        assert_eq!(
            DictationPacket::parse(&pdu, &Handles::OBSERVED),
            Ok(None),
            "{handle:#06x}"
        );
    }
    assert_eq!(DictationPacket::parse(&[], &Handles::OBSERVED), Ok(None));
}

#[test]
fn a_write_command_is_not_dictation() {
    let mut write_to_the_audio_handle = hex(ONE_FRAME_NOTIFICATION);
    write_to_the_audio_handle[0] = 0x52;
    let mut indication_on_the_status_handle = STARTED.to_vec();
    indication_on_the_status_handle[0] = 0x1d;
    let start = doap::start(&Handles::OBSERVED);
    let stop = doap::stop(&Handles::OBSERVED, doap::STOP_REASON_HOST);
    for pdu in [
        &start[..],
        &stop[..],
        &SESSION_ACTIVE,
        &SESSION_IDLE,
        &write_to_the_audio_handle,
        &indication_on_the_status_handle,
        &[0x52],
    ] {
        assert_eq!(
            DictationPacket::parse(pdu, &Handles::OBSERVED),
            Ok(None),
            "{pdu:02x?}"
        );
    }
}

#[test]
fn a_short_audio_notification_is_too_short() {
    let audio = hex(ONE_FRAME_NOTIFICATION);
    let unknown_marker = hex("1b 0d 00 02 00 2a 08");
    for pdu in [
        &audio[..3],
        &audio[..4],
        &audio[..5],
        &audio[..6],
        &audio[..7],
        &unknown_marker,
    ] {
        let error = DictationPacket::parse(pdu, &Handles::OBSERVED).unwrap_err();
        let expected = ParseError::TooShort {
            parser: Parser::DictationAudio,
            need: 8,
            got: pdu.len(),
        };
        assert_eq!(error, expected, "{pdu:02x?}");
    }
    assert_eq!(
        DictationPacket::parse(&audio[..7], &Handles::OBSERVED)
            .unwrap_err()
            .to_string(),
        "dictation audio notification too short: need 8 bytes, got 7"
    );
}

#[test]
fn a_notification_cut_before_its_handle_is_too_short() {
    for pdu in [&[0x1b][..], &[0x1b, 0x0d], &[0x1b, 0x10]] {
        let error = DictationPacket::parse(pdu, &Handles::OBSERVED).unwrap_err();
        let expected = ParseError::TooShort {
            parser: Parser::AttNotification,
            need: VALUE_AT,
            got: pdu.len(),
        };
        assert_eq!(error, expected, "{pdu:02x?}");
        assert_eq!(AttPdu::parse(pdu), Err(error), "{pdu:02x?}");
    }
    assert_eq!(
        DictationPacket::parse(&[0x1b, 0x0d], &Handles::OBSERVED)
            .unwrap_err()
            .to_string(),
        "attribute protocol notification too short: need 3 bytes, got 2"
    );
}

#[test]
fn a_pdu_the_att_reader_refuses_as_another_kind_is_not_dictation() {
    for refused in [
        "05",
        "05 03 0d 00 01 00",
        "05 01 0d 00 01",
        "09",
        "09 01 0d 00 01 00",
        "09 07 0d 00 01 00",
        "01 1b 0d 00",
        "01 1b 0d 00 01 00",
        "1d",
        "1d 0d",
        "1d 10",
    ] {
        let pdu = hex(refused);
        assert!(AttPdu::parse(&pdu).is_err(), "{refused} is not refused");
        assert_eq!(
            DictationPacket::parse(&pdu, &Handles::OBSERVED),
            Ok(None),
            "{refused}"
        );
    }
}

#[test]
fn an_audio_handle_value_with_another_prefix_is_unknown() {
    for pdu in [
        hex("1b 0d 00 02 00 2a 00 00"),
        hex("1b 0d 00 01 01 2a 00 00"),
        hex("1b 0d 00 00 01 2a 08 00 80 d5 04 01 03 b8 10 11"),
    ] {
        let expected = DictationPacket::Unknown {
            handle: Handles::OBSERVED.send_data,
            value: &pdu[VALUE_AT..],
        };
        assert_eq!(
            DictationPacket::parse(&pdu, &Handles::OBSERVED),
            Ok(Some(expected)),
            "{pdu:02x?}"
        );
    }
}

#[test]
fn is_expected_codec_reads_the_first_data_byte() {
    assert!(frame(0, &[0xb8]).is_expected_codec());
    assert!(frame(0, &[0xb8, 0x00]).is_expected_codec());
    assert!(!frame(0, &[0xb9]).is_expected_codec());
    assert!(!frame(0, &[0x00, 0xb8]).is_expected_codec());
    assert!(!frame(0, &[]).is_expected_codec());
}

#[test]
fn packet_counter_wraps_at_256_in_the_encoder_input() {
    let data = codec_bytes(0x10, 3);
    let last_before_the_wrap = u8::MAX;
    let first_after_the_wrap = last_before_the_wrap.wrapping_add(1);
    assert_eq!(first_after_the_wrap, 0);
    for (packet_counter, frame_counter) in
        [(last_before_the_wrap, u16::MAX), (first_after_the_wrap, 0)]
    {
        let frame = frame(frame_counter, &data);
        let pdu = encode_audio(&Handles::OBSERVED, packet_counter, &[frame]).unwrap();
        assert_eq!(pdu[PACKET_COUNTER_AT], packet_counter);
        let audio = audio(&pdu);
        assert_eq!(audio.packet_counter, packet_counter);
        assert_eq!(audio.frames().collect::<Vec<_>>(), [frame]);
    }
}

#[test]
fn encode_then_parse_round_trips() {
    let data = codec_bytes(0x10, 3);
    let one_frame = frame(0x0104, &data);
    assert_eq!(
        encode_audio(&Handles::OBSERVED, 0x2a, &[one_frame]),
        Ok(hex(ONE_FRAME_NOTIFICATION))
    );
    let literal = hex(ONE_FRAME_NOTIFICATION);
    let parsed = audio(&literal);
    assert_eq!(parsed.packet_counter, 0x2a);
    assert_eq!(parsed.frames().collect::<Vec<_>>(), [one_frame]);

    let payloads = [
        codec_bytes(0x00, 1),
        codec_bytes(0x07, LONGEST_FRAME),
        Vec::new(),
    ];
    let whole = frames(u16::MAX - 1, &payloads);
    assert_eq!(
        whole.iter().map(|f| f.counter).collect::<Vec<_>>(),
        [u16::MAX - 1, u16::MAX, 0]
    );
    let pdu = encode_audio(&Handles::OBSERVED, u8::MAX, &whole).unwrap();
    assert_eq!(
        pdu[DECLARED_LEN_AT..AUDIO_PREFIX_LEN],
        [0x0f, 0x01],
        "271 little endian"
    );
    let parsed = audio(&pdu);
    assert_eq!(parsed.packet_counter, u8::MAX);
    assert_eq!(parsed.declared_len, 271);
    assert!(parsed.length_matches());
    let mut seen = parsed.frames();
    assert_eq!(seen.by_ref().collect::<Vec<_>>(), whole);
    assert_eq!(seen.trailing(), 0);
}

#[test]
fn a_256_byte_frame_cannot_be_encoded() {
    let longest = codec_bytes(0x00, LONGEST_FRAME);
    let too_long = codec_bytes(0x00, LONGEST_FRAME + 1);
    assert!(encode_audio(&Handles::OBSERVED, 0, &[frame(0, &longest)]).is_ok());
    let error = encode_audio(
        &Handles::OBSERVED,
        0,
        &[frame(0, &longest), frame(1, &too_long)],
    )
    .unwrap_err();
    assert_eq!(error, EncodeError::FrameTooLong { index: 1, len: 256 });
    assert_eq!(
        error.to_string(),
        "dictation audio notification cannot carry frame 1: it is 256 bytes, the length byte holds at most 255"
    );
}

#[test]
fn a_packet_over_65535_bytes_cannot_be_encoded() {
    let longest = codec_bytes(0x00, LONGEST_FRAME);
    let at_the_limit = codec_bytes(0x00, LAST_DATA_AT_THE_LIMIT);
    let past_the_limit = codec_bytes(0x00, LAST_DATA_PAST_THE_LIMIT);
    let mut frames = vec![frame(0, &longest); FULL_FRAMES];
    frames.push(frame(0, &at_the_limit));
    let pdu = encode_audio(&Handles::OBSERVED, 0, &frames).unwrap();
    assert_eq!(
        pdu.len(),
        AUDIO_PREFIX_LEN + LONGEST_BODY,
        "the limit is on the frame bytes, so the notification is longer than it"
    );
    let largest = audio(&pdu);
    assert_eq!(largest.declared_len, u16::MAX);
    assert!(largest.length_matches());
    assert_eq!(largest.frames().collect::<Vec<_>>(), frames);

    frames.pop();
    frames.push(frame(0, &past_the_limit));
    let error = encode_audio(&Handles::OBSERVED, 0, &frames).unwrap_err();
    assert_eq!(error, EncodeError::PacketTooLong { len: 65536 });
    assert_eq!(
        error.to_string(),
        "dictation audio notification cannot carry 65536 bytes of frames: the length field counts the frames and their prefixes, not the whole notification, and holds at most 65535"
    );
}

#[test]
fn a_frame_too_long_is_refused_before_a_packet_too_long() {
    let longest = codec_bytes(0x00, LONGEST_FRAME);
    let too_long = codec_bytes(0x00, LONGEST_FRAME + 1);
    let mut frames = vec![frame(0, &longest); FRAMES_PAST_THE_LIMIT];
    assert_eq!(
        encode_audio(&Handles::OBSERVED, 0, &frames),
        Err(EncodeError::PacketTooLong { len: 65780 })
    );
    frames.push(frame(0, &too_long));
    let after_the_limit = EncodeError::FrameTooLong {
        index: FRAMES_PAST_THE_LIMIT,
        len: 256,
    };
    assert_eq!(
        encode_audio(&Handles::OBSERVED, 0, &frames),
        Err(after_the_limit)
    );
    frames.rotate_right(1);
    let before_the_limit = EncodeError::FrameTooLong { index: 0, len: 256 };
    assert_eq!(
        encode_audio(&Handles::OBSERVED, 0, &frames),
        Err(before_the_limit)
    );
}
