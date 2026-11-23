//! Support tests: the reader of the audio server's bytes against frames the protocol crate writes, cut, oversized and arbitrary input.

use std::ffi::CStr;
use std::time::Duration;

use proptest::collection::vec;
use proptest::prelude::*;
use pulseaudio::protocol::{
    AuthReply, ChannelVolume, Command, CommandReply, FormatEncoding, FormatInfo, Prop, Props,
    PulseError, ServerInfo, SinkInfo, SubscriptionEvent, SubscriptionEventFacility,
    SubscriptionEventType, read_command_message, write_command_message, write_error,
    write_reply_message,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::runtime::Builder;
use tokio::time::timeout;

use super::*;

/// Unlike `NO_ENTITY`, so a reader that swaps a refusal's sequence and code is seen.
const SEQ: u32 = 7;
/// The sequence a server gives an event, which answers no request.
const EVENT_SEQ: u32 = u32::MAX;
/// A read that has not ended by now is waiting for bytes that never come.
const READ_LIMIT: Duration = Duration::from_secs(1);
/// Half of `Volume::NORM`, which reads as 50 %.
const HALF: u32 = 0x8000;
/// One and a half `Volume::NORM`, a boosted sink.
const BOOSTED: u32 = 0x18000;
/// The protocol's number for a refusal of something that does not exist.
const NO_ENTITY: u32 = 5;
/// The bytes of a tagged `u32` and the payload's command and sequence before a reply's values.
const HEADER_LEN: usize = 10;
/// Just under 1.5 % of `Volume::NORM`, which rounds down to 1 %.
const UNDER_ONE_AND_A_HALF: u32 = 982;
/// Just over 1.6 % of `Volume::NORM`, which rounds up to 2 % where truncation gives 1 %.
const OVER_ONE_AND_A_HALF: u32 = 1049;
/// A cut inside the server name, the first value of a server info reply.
const INSIDE_SERVER_NAME: usize = 10;
/// Lengths this close to the cap land on it and one past it in every run.
const NEAR_THE_CAP: u32 = 4;

fn block_on<F: Future>(future: F) -> F::Output {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// A whole message as the crate writes it, its descriptor stripped.
fn stripped(bytes: &[u8]) -> Frame {
    Frame(bytes[DESCRIPTOR_SIZE..].to_vec())
}

fn reply(reply: &impl CommandReply) -> Frame {
    let mut bytes = Vec::new();
    write_reply_message(&mut bytes, SEQ, reply, PROTOCOL_VERSION).unwrap();
    stripped(&bytes)
}

fn body(frame: &Frame) -> &[u8] {
    match message(frame).unwrap() {
        Message::Reply { body, .. } => body,
        other @ (Message::Refused { .. } | Message::Event | Message::Other) => {
            panic!("not a reply: {other:?}")
        }
    }
}

fn descriptor(length: u32, channel: u32) -> [u8; DESCRIPTOR_SIZE] {
    let mut bytes = [0; DESCRIPTOR_SIZE];
    let flags = DescriptorFlags::empty();
    encode_descriptor(
        &mut bytes,
        &Descriptor {
            length,
            channel,
            offset: 0,
            flags,
        },
    );
    bytes
}

fn props() -> Props {
    let mut props = Props::new();
    props.set(Prop::DeviceDescription, c"Speakers");
    props.set(Prop::MediaName, c"Music");
    props
}

/// A sink as a server describes it, with properties, a port and a format that has properties of its own.
pub(in crate::daemon::audio) fn sink(index: u32, name: &CStr, volumes: &[u32]) -> SinkInfo {
    let mut cvolume = ChannelVolume::empty();
    for volume in volumes {
        cvolume.push(Volume::from_u32_clamped(*volume));
    }
    let format = FormatInfo {
        encoding: FormatEncoding::Pcm,
        props: props(),
    };
    SinkInfo {
        name: name.to_owned(),
        cvolume,
        props: props(),
        formats: vec![format],
        ..SinkInfo::new_dummy(index)
    }
}

/// The index `sink_list` gives its first sink, apart from list positions so a reader counting them is caught.
pub(in crate::daemon::audio) const FIRST_INDEX: u32 = 40;

/// The body of a sink list reply holding `sinks` in order.
pub(in crate::daemon::audio) fn sink_list(sinks: &[(&CStr, &[u32])]) -> Vec<u8> {
    let list: Vec<SinkInfo> = (FIRST_INDEX..)
        .zip(sinks)
        .map(|(index, (name, volumes))| sink(index, name, volumes))
        .collect();
    body(&reply(&list)).to_vec()
}

fn percent(value: u8) -> Percent {
    Percent::new(value).unwrap()
}

/// Every frame `read_frame` takes from `bytes` until its first error, which it gives too.
fn frames_of(bytes: &[u8]) -> (Vec<Frame>, io::Error) {
    block_on(async {
        let (mut ours, mut theirs) = UnixStream::pair().unwrap();
        theirs.write_all(bytes).await.unwrap();
        drop(theirs);
        let mut frames = Vec::new();
        let mut pending = Vec::new();
        loop {
            match timeout(READ_LIMIT, read_frame(&mut ours, &mut pending))
                .await
                .unwrap()
            {
                Ok(frame) => frames.push(frame),
                Err(error) => return (frames, error),
            }
        }
    })
}

/// A length on either side of the cap half the time, any length the other half.
fn around_the_cap() -> impl Strategy<Value = u32> {
    let cap = u32::try_from(AUDIO_FRAME_MAX).unwrap();
    prop_oneof![
        cap.saturating_sub(NEAR_THE_CAP)..=cap.saturating_add(NEAR_THE_CAP),
        any::<u32>()
    ]
}

#[test]
fn a_control_descriptor_gives_its_payload_length() {
    let largest = u32::try_from(AUDIO_FRAME_MAX).unwrap();
    assert_eq!(payload_length(&descriptor(10, CONTROL_CHANNEL)), Ok(10));
    assert_eq!(
        payload_length(&descriptor(largest, CONTROL_CHANNEL)),
        Ok(AUDIO_FRAME_MAX)
    );
    let past = largest.saturating_add(1);
    assert_eq!(
        payload_length(&descriptor(past, CONTROL_CHANNEL)),
        Err(Malformed::Oversized(past))
    );
    assert_eq!(
        payload_length(&descriptor(u32::MAX, CONTROL_CHANNEL)),
        Err(Malformed::Oversized(u32::MAX))
    );
    assert_eq!(
        payload_length(&descriptor(10, 0)),
        Err(Malformed::StreamData(0))
    );
}

#[test]
fn an_oversized_frame_is_refused_before_its_payload_is_awaited() {
    let refused = block_on(async {
        let (mut ours, mut theirs) = UnixStream::pair().unwrap();
        theirs
            .write_all(&descriptor(u32::MAX, CONTROL_CHANNEL))
            .await
            .unwrap();
        // Kept open, so a reader that trusted the length would wait here.
        let refused = timeout(READ_LIMIT, read_frame(&mut ours, &mut Vec::new()))
            .await
            .unwrap();
        drop(theirs);
        refused
    });
    let error = refused.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
    assert_eq!(
        error.to_string(),
        "a frame of 4294967295 bytes, past 262144"
    );
}

#[test]
fn a_frame_cut_short_is_a_closed_connection() {
    let whole = [descriptor(4, CONTROL_CHANNEL).as_slice(), &[1, 2, 3, 4]].concat();
    for cut in [
        0,
        DESCRIPTOR_SIZE.saturating_sub(1),
        whole.len().saturating_sub(1),
    ] {
        let (frames, error) = frames_of(&whole[..cut]);
        assert!(frames.is_empty(), "{cut}");
        assert_eq!(error.kind(), ErrorKind::UnexpectedEof, "{cut}");
        assert_eq!(error.to_string(), "the server closed the connection");
    }
    let (frames, _) = frames_of(&whole);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].0, [1, 2, 3, 4]);
}

#[test]
fn a_request_goes_out_as_the_crate_reads_it() {
    let sent = block_on(async {
        let (mut ours, mut theirs) = UnixStream::pair().unwrap();
        let request = Frame::request(SEQ, &Command::GetSinkInfoList).unwrap();
        write_frame(&mut ours, &request).await.unwrap();
        drop(ours);
        let mut sent = Vec::new();
        theirs.read_to_end(&mut sent).await.unwrap();
        sent
    });
    let (seq, command) = read_command_message(&mut sent.as_slice(), PROTOCOL_VERSION).unwrap();
    assert_eq!(seq, SEQ);
    assert_eq!(command, Command::GetSinkInfoList);
}

#[test]
fn message_tells_replies_refusals_events_and_the_rest_apart() {
    let auth = reply(&AuthReply::default());
    assert!(matches!(
        message(&auth),
        Ok(Message::Reply { seq: SEQ, .. })
    ));

    let mut refusal = Vec::new();
    write_error(&mut refusal, SEQ, &PulseError::NoEntity).unwrap();
    assert_eq!(
        message(&stripped(&refusal)),
        Ok(Message::Refused {
            seq: SEQ,
            code: NO_ENTITY
        })
    );

    let event = Command::SubscribeEvent(SubscriptionEvent {
        event_facility: SubscriptionEventFacility::Sink,
        event_type: SubscriptionEventType::Changed,
        index: Some(0),
    });
    let mut announced = Vec::new();
    write_command_message(&mut announced, EVENT_SEQ, &event, PROTOCOL_VERSION).unwrap();
    assert_eq!(message(&stripped(&announced)), Ok(Message::Event));

    let mut other = Vec::new();
    write_command_message(&mut other, SEQ, &Command::GetServerInfo, PROTOCOL_VERSION).unwrap();
    assert_eq!(message(&stripped(&other)), Ok(Message::Other));

    assert_eq!(message(&Frame(Vec::new())), Err(Malformed::Cut));
    assert_eq!(
        message(&Frame(vec![tag::STRING, 0])),
        Err(Malformed::Tag(tag::STRING))
    );
}

#[test]
fn the_auth_reply_grants_its_version_without_the_memory_flags() {
    let granted = AuthReply {
        version: PROTOCOL_VERSION,
        use_memfd: true,
        use_shm: true,
    };
    assert_eq!(auth_version(body(&reply(&granted))), Ok(PROTOCOL_VERSION));
    assert_eq!(auth_version(&[]), Err(Malformed::Cut));
}

#[test]
fn the_default_sink_is_read_from_the_server_info() {
    let named = ServerInfo {
        server_name: Some(c"PulseAudio (on PipeWire 1.4.2)".to_owned()),
        default_sink_name: Some(c"alsa_output.speakers".to_owned()),
        default_source_name: Some(c"alsa_input.mic".to_owned()),
        ..ServerInfo::default()
    };
    let bytes = reply(&named);
    let named_body = body(&bytes);
    assert_eq!(
        default_sink(named_body),
        Ok(Some(b"alsa_output.speakers".as_slice()))
    );
    let empty = reply(&ServerInfo::default());
    assert_eq!(default_sink(body(&empty)), Ok(None));
    let whole = default_sink(named_body);
    for cut in 0..named_body.len() {
        // Past the name the rest goes unread, so a cut there still reads it.
        let read = default_sink(&named_body[..cut]);
        assert!(read.is_err() || read == whole, "cut at {cut}: {read:?}");
    }
    assert_eq!(
        default_sink(&named_body[..INSIDE_SERVER_NAME]),
        Err(Malformed::Cut)
    );
}

#[test]
fn every_sink_of_a_list_is_read_with_its_mean_volume() {
    let list = sink_list(&[
        (c"alsa_output.speakers", &[HALF, HALF]),
        (
            c"bluez_output.02_00_00_00_00_01.1",
            &[Volume::NORM.as_u32(), 0],
        ),
        (c"boosted", &[BOOSTED]),
        (c"quiet", &[UNDER_ONE_AND_A_HALF]),
        (c"rounded", &[OVER_ONE_AND_A_HALF]),
    ]);
    let read: Vec<Sink<'_>> = sinks(&list).map(Result::unwrap).collect();
    let expected = [
        (b"alsa_output.speakers".as_slice(), percent(50)),
        (b"bluez_output.02_00_00_00_00_01.1".as_slice(), percent(50)),
        (b"boosted".as_slice(), Percent::FULL),
        (b"quiet".as_slice(), percent(1)),
        (b"rounded".as_slice(), percent(2)),
    ];
    let names: Vec<(&[u8], Percent)> = read.iter().map(|sink| (sink.name, sink.volume)).collect();
    assert_eq!(names, expected);
    assert_eq!(sinks(&[]).count(), 0);
}

#[test]
fn each_sinks_index_and_state_are_read() {
    let states = [
        (4, SinkState::Running),
        (9, SinkState::Idle),
        (11, SinkState::Suspended),
    ];
    let infos: Vec<SinkInfo> = states
        .into_iter()
        .map(|(index, state)| SinkInfo {
            state,
            ..sink(index, c"bluez_output.02_00_00_00_00_01.1", &[HALF])
        })
        .collect();
    let list = body(&reply(&infos)).to_vec();
    let read: Vec<(u32, SinkState)> = sinks(&list)
        .map(|sink| sink.map(|sink| (sink.index, sink.state)).unwrap())
        .collect();
    assert_eq!(read, states);
}

#[test]
fn a_cut_sink_list_never_reads_as_whole() {
    let list = sink_list(&[(c"one", &[HALF]), (c"two", &[HALF, HALF])]);
    let whole: Vec<_> = sinks(&list).collect();
    for cut in 0..list.len() {
        let read: Vec<_> = sinks(&list[..cut]).collect();
        assert_ne!(read, whole, "cut at {cut}");
        let errors = read.iter().filter(|sink| sink.is_err()).count();
        assert!(errors <= 1, "cut at {cut}: {read:?}");
    }
}

#[test]
fn a_malformed_sink_ends_the_walk() {
    let nameless = [tag::U32, 0, 0, 0, 0, tag::STRING_NULL];
    let then_whole = [nameless.as_slice(), &sink_list(&[(c"after", &[HALF])])].concat();
    assert_eq!(
        sinks(&then_whole).collect::<Vec<_>>(),
        [Err(Malformed::Nameless)]
    );
    assert_eq!(
        Tags(&[tag::CVOLUME, 0]).volume(),
        Err(Malformed::NoChannels)
    );
    assert_eq!(Tags(b"?").skip(), Err(Malformed::Tag(b'?')));
}

#[test]
fn a_property_value_declared_past_the_frame_is_cut_without_reading_it() {
    let past = u32::MAX.to_be_bytes();
    let huge = [
        [tag::PROPLIST, tag::STRING].as_slice(),
        b"k\0",
        &[tag::U32],
        &past,
        &[tag::ARBITRARY],
        &past,
    ]
    .concat();
    assert_eq!(Tags(&huge).skip(), Err(Malformed::Cut));
}

#[test]
fn a_property_list_inside_a_property_list_is_refused() {
    let nested = [tag::PROPLIST, tag::PROPLIST, tag::STRING_NULL];
    assert_eq!(Tags(&nested).skip(), Err(Malformed::Tag(tag::PROPLIST)));
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic_the_readers(bytes in vec(any::<u8>(), 0..512)) {
        if let Ok(Message::Reply { body, .. }) = message(&Frame(bytes.clone())) {
            prop_assert_eq!(body.len().saturating_add(HEADER_LEN), bytes.len());
        }
        let tagged = bytes.len() > size_of::<u32>() && bytes[0] == tag::U32;
        prop_assert_eq!(auth_version(&bytes).is_ok(), tagged);
        if let Ok(Some(name)) = default_sink(&bytes) {
            prop_assert!(name.len() < bytes.len() && !name.contains(&0));
        }
        let read: Vec<_> = sinks(&bytes).collect();
        let errors = read.iter().filter(|sink| sink.is_err()).count();
        prop_assert!(errors == 0 || (errors == 1 && read.last().is_some_and(Result::is_err)));
    }

    #[test]
    fn a_control_frame_is_taken_exactly_up_to_the_cap(length in around_the_cap()) {
        let taken = payload_length(&descriptor(length, CONTROL_CHANNEL));
        let within = usize::try_from(length).is_ok_and(|length| length <= AUDIO_FRAME_MAX);
        prop_assert_eq!(taken.is_ok(), within);
    }

    #[test]
    fn arbitrary_bytes_on_the_stream_never_panic_read_frame(
        length in 0_u32..64,
        payload in vec(any::<u8>(), 0..64),
        garbage in vec(any::<u8>(), 0..64),
    ) {
        let bytes = [descriptor(length, CONTROL_CHANNEL).as_slice(), &payload, &garbage].concat();
        let (frames, _) = frames_of(&bytes);
        let whole = usize::try_from(length).is_ok_and(|length| length <= payload.len());
        prop_assert!(!whole || !frames.is_empty(), "a whole frame was not read");
        let mut rest = bytes.as_slice();
        for frame in &frames {
            let (head, after) = rest.split_at(DESCRIPTOR_SIZE);
            let declared = payload_length(head.try_into().unwrap()).unwrap();
            prop_assert_eq!(frame.0.as_slice(), &after[..declared]);
            rest = &after[declared..];
        }
    }
}
