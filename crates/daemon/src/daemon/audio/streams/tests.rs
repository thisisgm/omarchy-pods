//! Support tests: the readers of the source list and of the record stream list against replies the protocol crate writes, cut and arbitrary input; the suite through the built daemon is `tests/recorders.rs`.

use std::ffi::{CStr, CString};

use proptest::collection::vec;
use proptest::prelude::*;
use pulseaudio::protocol::{
    ChannelMap, ChannelVolume, FormatEncoding, FormatInfo, Props, SampleFormat, SampleSpec,
    SourceInfo, SourceOutputInfo,
};

use super::super::wire::tests::{body, reply};
use super::*;

const MICROPHONE: &CStr = c"omarchypods_input.a3048";
/// Apart from list positions, so a reader counting them is caught.
pub(in crate::daemon::audio) const MICROPHONE_INDEX: u32 = 57;
const BUILT_IN_INDEX: u32 = 40;
/// What the server lists as the source of a record stream linked to none.
const NO_SOURCE: u32 = u32::MAX;
const SPEC: SampleSpec = SampleSpec {
    format: SampleFormat::S16Le,
    channels: 2,
    sample_rate: 48_000,
};
/// Longer than any list a test here builds.
const ANY_BYTES_MAX: usize = 2048;

pub(in crate::daemon::audio) fn source(
    index: u32,
    name: &CStr,
    monitor_of: Option<&CStr>,
) -> SourceInfo {
    SourceInfo {
        index,
        name: name.to_owned(),
        sample_spec: SPEC,
        channel_map: ChannelMap::stereo(),
        cvolume: ChannelVolume::norm(SPEC.channels),
        monitor_of_sink_index: monitor_of.map(|_| 0),
        monitor_of_sink_name: monitor_of.map(CStr::to_owned),
        ..SourceInfo::default()
    }
}

fn source_list() -> Vec<u8> {
    let list = vec![
        source(
            39,
            c"alsa_output.speakers.monitor",
            Some(c"alsa_output.speakers"),
        ),
        source(BUILT_IN_INDEX, c"alsa_input.built-in", None),
        source(MICROPHONE_INDEX, MICROPHONE, None),
    ];
    body(&reply(&list)).to_vec()
}

/// A record stream on `source`, its node named `node` when one is given.
pub(in crate::daemon::audio) fn stream_on(source: u32, node: Option<&str>) -> SourceOutputInfo {
    let mut props = Props::new();
    let keys = [
        ("media.class", Some("Stream/Input/Audio")),
        ("node.name", node),
    ];
    for (key, value) in keys {
        if let Some(value) = value {
            let value = CString::new(value).unwrap();
            props.set_bytes(CString::new(key).unwrap(), value.as_bytes_with_nul());
        }
    }
    SourceOutputInfo {
        name: c"recorder".to_owned(),
        source_index: source,
        sample_spec: SPEC,
        channel_map: ChannelMap::stereo(),
        cvolume: ChannelVolume::norm(SPEC.channels),
        props,
        has_volume: true,
        volume_writable: true,
        format: FormatInfo::new(FormatEncoding::Pcm),
        ..SourceOutputInfo::default()
    }
}

fn stream_list(streams: &[SourceOutputInfo]) -> Vec<u8> {
    body(&reply(&streams.to_vec())).to_vec()
}

fn counted(list: &[u8]) -> Result<Recorders, Malformed> {
    recorders(list, MICROPHONE_INDEX)
}

#[test]
fn a_source_is_found_by_its_whole_name() {
    let list = source_list();
    let found = source_index(&list, MICROPHONE.to_bytes());
    assert_eq!(found, Ok(Some(MICROPHONE_INDEX)));
    assert_eq!(source_index(&list, b"omarchypods_input.a304"), Ok(None));
    assert_eq!(
        source_index(&list, b"omarchypods_input.a3048.monitor"),
        Ok(None)
    );
    assert_eq!(source_index(&[], MICROPHONE.to_bytes()), Ok(None));
}

#[test]
fn only_streams_on_the_source_are_counted_and_only_superwhispers_as_dictation() {
    let list = stream_list(&[
        stream_on(MICROPHONE_INDEX, Some("superwhisper-capture")),
        stream_on(BUILT_IN_INDEX, Some("superwhisper-capture")),
        stream_on(MICROPHONE_INDEX, Some("superwhisper-history")),
        stream_on(MICROPHONE_INDEX, Some("superwhisper-capture2")),
        stream_on(MICROPHONE_INDEX, None),
        stream_on(NO_SOURCE, Some("superwhisper-history")),
        stream_on(MICROPHONE_INDEX, Some("Chromium input")),
    ]);
    let both = Recorders {
        dictation: 2,
        other: 3,
    };
    assert_eq!(counted(&list), Ok(both));
    assert_eq!(counted(&[]), Ok(Recorders::default()));
}

#[test]
fn the_longest_list_is_read_and_one_stream_more_refuses_it_whole() {
    let mut streams = vec![stream_on(MICROPHONE_INDEX, Some("superwhisper-capture")); 2];
    streams.resize(MAX_RECORD_STREAMS, stream_on(BUILT_IN_INDEX, None));
    let both = Recorders {
        dictation: 2,
        other: 0,
    };
    assert_eq!(counted(&stream_list(&streams)), Ok(both));
    streams.push(stream_on(BUILT_IN_INDEX, None));
    assert_eq!(counted(&stream_list(&streams)), Ok(Recorders::default()));
}

#[test]
fn more_streams_than_a_count_holds_read_as_its_most() {
    let streams = vec![stream_on(MICROPHONE_INDEX, None); MAX_RECORD_STREAMS];
    let most = Recorders {
        dictation: 0,
        other: u8::MAX,
    };
    assert_eq!(counted(&stream_list(&streams)), Ok(most));
}

#[test]
fn a_list_cut_anywhere_inside_a_stream_is_malformed() {
    let list = stream_list(&[stream_on(MICROPHONE_INDEX, Some("superwhisper-capture"))]);
    for cut in 1..list.len() {
        assert_eq!(counted(&list[..cut]), Err(Malformed::Cut), "cut at {cut}");
    }
    let sources = source_list();
    let cut = &sources[..sources.len() - 1];
    assert_eq!(
        source_index(cut, MICROPHONE.to_bytes()),
        Err(Malformed::Cut)
    );
}

proptest! {
    #[test]
    fn neither_reader_panics_on_any_bytes(
        bytes in vec(any::<u8>(), 0..ANY_BYTES_MAX),
        source in any::<u32>(),
    ) {
        let name = MICROPHONE.to_bytes();
        if let Ok(Some(_)) = source_index(&bytes, name) {
            prop_assert!(bytes.windows(name.len()).any(|window| window == name));
        }
        if let Ok(counted) = recorders(&bytes, source) {
            let named = |node: &&[u8]| bytes.windows(node.len()).any(|window| window == *node);
            prop_assert!(counted.dictation == 0 || DICTATION_RECORDERS.iter().any(named));
        }
    }

    #[test]
    fn any_bytes_after_a_list_never_panic_and_keep_what_was_read(
        tail in vec(any::<u8>(), 0..ANY_BYTES_MAX),
    ) {
        let mut streams = stream_list(&[stream_on(MICROPHONE_INDEX, Some("superwhisper-capture"))]);
        streams.extend(&tail);
        if let Ok(counted) = counted(&streams) {
            prop_assert!(counted.dictation >= 1);
        }
        let mut sources = source_list();
        sources.extend(&tail);
        let found = source_index(&sources, MICROPHONE.to_bytes());
        prop_assert_eq!(found, Ok(Some(MICROPHONE_INDEX)));
    }
}
