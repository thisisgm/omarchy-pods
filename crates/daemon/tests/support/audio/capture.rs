//! The fake audio server's sources, sink monitors among them, and the record streams on them as `pipewire-pulse` 1.4 lists them.

use std::ffi::CString;

use pulseaudio::protocol::{
    ChannelMap, ChannelVolume, FormatEncoding, FormatInfo, Props, SampleFormat, SampleSpec,
    SourceInfo, SourceOutputInfo,
};

/// What the server lists as the source of a record stream linked to none.
const NO_SOURCE: u32 = u32::MAX;
const MONITOR_SUFFIX: &str = ".monitor";
/// The format `parec` records in by default.
const RECORD_SPEC: SampleSpec = SampleSpec {
    format: SampleFormat::S16Le,
    channels: 2,
    sample_rate: 44_100,
};

/// A record stream as a desktop opens one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Recorder {
    Program,
    /// A level meter, which `PA_STREAM_PEAK_DETECT` marks `stream.monitor`.
    PeakMeter,
    /// A recording of a sink's output through the sink's monitor source.
    Monitor,
    /// A program holding a source open without recording.
    Corked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecorderId(pub(crate) u32);

struct Source {
    index: u32,
    name: String,
    /// The index and name of the sink this source monitors.
    monitor_of: Option<(u32, String)>,
}

struct Recording {
    index: u32,
    /// A sink for a `Monitor`, a source for every other kind.
    target: String,
    kind: Recorder,
}

/// What the server names a sink's monitor source: `bluez_output.X.1` gives `bluez_output.X.1.monitor`.
pub(super) fn monitor_name(sink: &str) -> String {
    format!("{sink}{MONITOR_SUFFIX}")
}

#[derive(Default)]
pub(super) struct Capture {
    sources: Vec<Source>,
    recordings: Vec<Recording>,
    next_source: u32,
    next_recording: u32,
}

impl Capture {
    pub(super) fn add_source(&mut self, name: &str) {
        let taken = self.sources.iter().any(|source| source.name == name);
        assert!(!taken, "a second source named {name}");
        self.push_source(name.to_owned(), None);
    }

    /// The index of the monitor source opened with the sink `sink` at `index`.
    pub(super) fn add_monitor(&mut self, sink: &str, index: u32) -> u32 {
        self.push_source(monitor_name(sink), Some((index, sink.to_owned())))
    }

    pub(super) fn remove_monitor(&mut self, sink: u32) {
        self.sources
            .retain(|source| source.monitor_of.as_ref().map(|(index, _)| *index) != Some(sink));
    }

    pub(super) fn add(&mut self, target: &str, kind: Recorder) -> RecorderId {
        let index = self.next_recording;
        self.next_recording = self.next_recording.saturating_add(1);
        self.recordings.push(Recording {
            index,
            target: target.to_owned(),
            kind,
        });
        RecorderId(index)
    }

    /// The removed stream's index.
    pub(super) fn remove(&mut self, id: RecorderId) -> u32 {
        let position = self
            .recordings
            .iter()
            .position(|recording| recording.index == id.0)
            .unwrap_or_else(|| panic!("no recorder {}", id.0));
        self.recordings.remove(position).index
    }

    pub(super) fn sources(&self) -> Vec<SourceInfo> {
        self.sources.iter().map(Source::info).collect()
    }

    pub(super) fn recorders(&self) -> Vec<SourceOutputInfo> {
        let info = |recording: &Recording| recording.info(self.source_of(recording));
        self.recordings.iter().map(info).collect()
    }

    fn push_source(&mut self, name: String, monitor_of: Option<(u32, String)>) -> u32 {
        let index = self.next_source;
        self.next_source = self.next_source.saturating_add(1);
        self.sources.push(Source {
            index,
            name,
            monitor_of,
        });
        index
    }

    /// The source the stream is linked to: a sink's monitor for a `Monitor`, a source by name otherwise.
    fn source_of(&self, recording: &Recording) -> Option<u32> {
        let linked = self.sources.iter().find(|source| {
            let monitored = source.monitor_of.as_ref().map(|(_, sink)| sink.as_str());
            match recording.kind {
                Recorder::Monitor => monitored == Some(recording.target.as_str()),
                Recorder::Program | Recorder::PeakMeter | Recorder::Corked => {
                    source.name == recording.target
                }
            }
        });
        linked.map(|source| source.index)
    }
}

impl Source {
    fn info(&self) -> SourceInfo {
        let (monitor_index, monitor_name) = match &self.monitor_of {
            Some((index, sink)) => (Some(*index), Some(CString::new(sink.as_str()).unwrap())),
            None => (None, None),
        };
        SourceInfo {
            index: self.index,
            name: CString::new(self.name.as_str()).unwrap(),
            sample_spec: RECORD_SPEC,
            channel_map: ChannelMap::stereo(),
            cvolume: ChannelVolume::norm(RECORD_SPEC.channels),
            monitor_of_sink_index: monitor_index,
            monitor_of_sink_name: monitor_name,
            ..SourceInfo::default()
        }
    }
}

impl Recording {
    fn info(&self, source: Option<u32>) -> SourceOutputInfo {
        let corked = self.kind == Recorder::Corked;
        SourceOutputInfo {
            index: self.index,
            name: c"recorder".to_owned(),
            source_index: source.unwrap_or(NO_SOURCE),
            sample_spec: RECORD_SPEC,
            channel_map: ChannelMap::stereo(),
            cvolume: ChannelVolume::norm(RECORD_SPEC.channels),
            props: self.props(corked),
            corked,
            has_volume: true,
            volume_writable: true,
            format: FormatInfo::new(FormatEncoding::Pcm),
            ..SourceOutputInfo::default()
        }
    }

    /// The keys that tell the kinds apart, as `pipewire-pulse` sets them on a stream a pulse client opens.
    fn props(&self, corked: bool) -> Props {
        let kind = match self.kind {
            Recorder::PeakMeter => Some(("stream.monitor", "true")),
            Recorder::Monitor => Some(("stream.capture.sink", "true")),
            Recorder::Program | Recorder::Corked => None,
        };
        let mut props = Props::new();
        let common = [
            ("client.api", "pipewire-pulse"),
            ("media.class", "Stream/Input/Audio"),
            ("pulse.corked", if corked { "true" } else { "false" }),
            ("target.object", self.target.as_str()),
        ];
        for (key, value) in common.into_iter().chain(kind) {
            let value = CString::new(value).unwrap();
            props.set_bytes(CString::new(key).unwrap(), value.as_bytes_with_nul());
        }
        props
    }
}
