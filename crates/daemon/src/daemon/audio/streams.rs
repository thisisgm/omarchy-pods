//! The record streams on the microphone's source, as the audio server lists them.

use pods_core::policy::audio::Recorders;
use pods_core::policy::route::DICTATION_RECORDERS;

use super::wire::{self, Malformed, Tags};

/// A desktop holds a few dozen record streams; a longer list is refused whole.
pub(super) const MAX_RECORD_STREAMS: usize = 256;

const NODE_NAME: &[u8] = b"node.name";

/// Index, name, owner module and client, before a record stream's source.
const BEFORE_SOURCE: usize = 4;

/// Sample spec, channel map, two latencies, resample method and driver, between the source and the properties.
const BEFORE_PROPERTIES: usize = 6;

/// Corked, volume, mute, two volume flags and the format.
const AFTER_PROPERTIES: usize = 6;

/// The index of the source named `name`, a source being laid out as a sink is; sample: one `Tags::sink` after another.
pub(super) fn source_index(body: &[u8], name: &[u8]) -> Result<Option<u32>, Malformed> {
    for source in wire::sinks(body) {
        let source = source?;
        if source.name == name {
            return Ok(Some(source.index));
        }
    }
    Ok(None)
}

/// The record streams on `source`, those whose node `DICTATION_RECORDERS` names apart, none when the list passes `MAX_RECORD_STREAMS`; sample: one `stream` after another.
pub(super) fn recorders(body: &[u8], source: u32) -> Result<Recorders, Malformed> {
    let mut tags = Tags(body);
    let mut counted = Recorders::default();
    for _ in 0..MAX_RECORD_STREAMS {
        if tags.0.is_empty() {
            return Ok(counted);
        }
        let (on, node) = stream(&mut tags)?;
        if on != source {
            continue;
        }
        if node.is_some_and(|node| {
            DICTATION_RECORDERS
                .iter()
                .any(|known| node == known.as_bytes())
        }) {
            counted.dictation = counted.dictation.saturating_add(1);
        } else {
            counted.other = counted.other.saturating_add(1);
        }
    }
    if tags.0.is_empty() {
        Ok(counted)
    } else {
        Ok(Recorders::default())
    }
}

/// One record stream at protocol 35, its source and its node's name; sample `L 3 t "recorder\0" L m L c L 5 a m U U N N P t "node.name\0" L 00000015 x 00000015 "superwhisper-capture\0" N 0 v 01 00010000 0 1 1 f B 01 P N`.
fn stream<'a>(tags: &mut Tags<'a>) -> Result<(u32, Option<&'a [u8]>), Malformed> {
    tags.skip_n(BEFORE_SOURCE)?;
    let source = tags.u32()?;
    tags.skip_n(BEFORE_PROPERTIES)?;
    let node = tags.property(NODE_NAME)?;
    tags.skip_n(AFTER_PROPERTIES)?;
    Ok((source, node))
}

#[cfg(test)]
pub(super) mod tests;
