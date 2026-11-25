//! Capture activity and the profile-routing counts from the native audio server.

use std::ffi::CString;

use pods_core::policy::audio::{AudioFact, Recorders, SinkFact};
use pods_core::policy::route::DICTATION_RECORDERS;
use pulseaudio::protocol::{Command, SubscriptionMask};

use super::wire::{self, Malformed, Tags};
use super::{Connection, Ended, Follower, Lost, log};

/// Only routing counts are refused when the record stream list exceeds this cap.
pub(super) const MAX_RECORD_STREAMS: usize = 256;

const NODE_NAME: &[u8] = b"node.name";

/// Index, name, owner module and client, before a record stream's source.
const BEFORE_SOURCE: usize = 4;

/// Sample spec, channel map, two latencies, resample method and driver, between the source and the properties.
const BEFORE_PROPERTIES: usize = 6;

/// Volume, mute, two volume flags and the format, after the corked flag.
const AFTER_CORKED: usize = 5;

/// The protocol's invalid index means a source has no sink to monitor.
const NO_MONITORED_SINK: u32 = u32::MAX;

pub(super) struct CaptureList<'a> {
    pub(super) name: String,
    sources: Sources,
    default_source_attempted: &'a mut bool,
}

impl<'a> CaptureList<'a> {
    pub(super) fn new(default_source_attempted: &'a mut bool) -> Self {
        Self {
            name: String::new(),
            sources: Sources::default(),
            default_source_attempted,
        }
    }
}

impl Follower<'_> {
    pub(super) async fn refresh_recorders(
        &mut self,
        connection: &mut Connection,
        refresh: SubscriptionMask,
        stale: &mut SubscriptionMask,
        capture: &mut CaptureList<'_>,
        sink: Option<SinkFact>,
    ) -> Result<(), Ended> {
        let name = self.state.borrow().mic_source();
        let reread_source = refresh.contains(SubscriptionMask::SOURCE) || name != capture.name;
        if reread_source {
            capture.name = name;
            let listed = connection.call(&Command::GetSourceInfoList, stale, |body| {
                Sources::listed(body, capture.name.as_bytes())
            });
            capture.sources = self.requests.watched(listed).await?;
            self.restore(connection, sink, stale).await?;
            if capture.sources.microphone.is_some() && !*capture.default_source_attempted {
                *capture.default_source_attempted = true;
                self.set_default_source(connection, stale, &capture.name)
                    .await?;
            }
        }
        if reread_source || refresh.contains(SubscriptionMask::SOURCE_OUTPUT) {
            let recording = if capture.sources.has_targets() {
                let listed = connection.call(&Command::GetSourceOutputInfoList, stale, |body| {
                    capture.sources.recorders(body)
                });
                let recording = self.requests.watched(listed).await?;
                self.restore(connection, sink, stale).await?;
                recording
            } else {
                Recording::default()
            };
            if recording.active != self.recording.active {
                self.tell(AudioFact::Recording(recording.active)).await;
            }
            if recording.microphone != self.recording.microphone {
                self.tell(AudioFact::Recorders(recording.microphone)).await;
            }
            self.recording = recording;
        }
        Ok(())
    }

    async fn set_default_source(
        &mut self,
        connection: &mut Connection,
        stale: &mut SubscriptionMask,
        name: &str,
    ) -> Result<(), Ended> {
        let name = CString::new(name).map_err(|error| Lost::Io(error.into()))?;
        let command = Command::SetDefaultSource(name);
        let setting = connection.call(&command, stale, |_| Ok(()));
        match self.requests.watched(setting).await {
            Ok(()) => log("mic: default source set"),
            Err(Ended::Lost(lost @ Lost::Refused { .. })) => {
                log(&format!("mic: could not set default source: {lost}"));
            }
            Err(ended) => return Err(ended),
        }
        Ok(())
    }
}

#[derive(Default)]
struct Sources {
    microphone: Option<u32>,
    capture: Vec<u32>,
}

#[derive(Default)]
pub(super) struct Recording {
    pub(super) microphone: Recorders,
    pub(super) active: bool,
}

impl Sources {
    fn has_targets(&self) -> bool {
        self.microphone.is_some() || !self.capture.is_empty()
    }

    /// Source indices from a protocol 35 source list, with the same tag counts as `Tags::sink`.
    fn listed(body: &[u8], microphone: &[u8]) -> Result<Self, Malformed> {
        let mut sources = Self::default();
        for source in wire::sinks(body) {
            let source = source?;
            if source.name == microphone {
                sources.microphone = Some(source.index);
            }
            if source.monitor_index == NO_MONITORED_SINK {
                sources.capture.push(source.index);
            }
        }
        sources.capture.sort_unstable();
        Ok(sources)
    }

    /// One `stream` after another, up to `MAX_RECORD_STREAMS` for routing counts.
    fn recorders(&self, body: &[u8]) -> Result<Recording, Malformed> {
        let mut tags = Tags(body);
        let mut counted = Recording::default();
        for _ in 0..MAX_RECORD_STREAMS {
            if tags.0.is_empty() {
                return Ok(counted);
            }
            let (on, node, corked) = stream(&mut tags)?;
            counted.active |= !corked && self.capture.binary_search(&on).is_ok();
            if Some(on) != self.microphone {
                continue;
            }
            if node.is_some_and(|node| {
                DICTATION_RECORDERS
                    .iter()
                    .any(|known| node == known.as_bytes())
            }) {
                counted.microphone.dictation = counted.microphone.dictation.saturating_add(1);
            } else {
                counted.microphone.other = counted.microphone.other.saturating_add(1);
            }
        }
        // The frame already bounds the list. Keep checking capture when routing refuses its counts.
        if !tags.0.is_empty() {
            counted.microphone = Recorders::default();
        }
        while !counted.active && !tags.0.is_empty() {
            let (on, _, corked) = stream(&mut tags)?;
            counted.active = !corked && self.capture.binary_search(&on).is_ok();
        }
        Ok(counted)
    }
}

/// One record stream at protocol 35; sample `L 3 t "recorder\0" L m L c L 5 a m U U N N P N 0 v 01 00010000 0 1 1 f B 01 P N`.
fn stream<'a>(tags: &mut Tags<'a>) -> Result<(u32, Option<&'a [u8]>, bool), Malformed> {
    tags.skip_n(BEFORE_SOURCE)?;
    let source = tags.u32()?;
    tags.skip_n(BEFORE_PROPERTIES)?;
    let node = tags.property(NODE_NAME)?;
    let corked = tags.boolean()?;
    tags.skip_n(AFTER_CORKED)?;
    Ok((source, node, corked))
}

#[cfg(test)]
pub(super) mod tests;
