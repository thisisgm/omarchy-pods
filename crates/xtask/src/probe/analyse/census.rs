//! Counts of the packets that are not audio, the control replies, the battery and the start trials.

use std::collections::BTreeMap;

use pods_proto::battery::BatteryReport;

use super::input::{Mark, MarkKind};
use super::load::{Loaded, UPLINK, windows};
use super::stats::millis;

/// How many packets that are not uplink packets carry each opcode, by opcode.
pub(crate) fn census(loaded: &Loaded) -> Vec<(u16, u32)> {
    let mut counts: BTreeMap<u16, u32> = BTreeMap::new();
    let others = loaded.seen.iter().filter(|packet| !packet.uplink);
    for opcode in others.filter_map(|packet| packet.opcode) {
        let seen = counts.entry(opcode).or_insert(0);
        *seen = seen.saturating_add(1);
    }
    counts.into_iter().collect()
}

/// How many packets from the pods are too short to hold an opcode.
pub(crate) fn without_opcode(loaded: &Loaded) -> usize {
    let short = loaded.seen.iter().filter(|packet| packet.opcode.is_none());
    short.count()
}

/// The opcode and the length of the largest packet from the pods, the first of equals; `None` when none has an opcode.
pub(crate) fn largest_packet(loaded: &Loaded) -> Option<(u16, usize)> {
    let mut largest: Option<(u16, usize)> = None;
    for packet in &loaded.seen {
        if let Some(opcode) = packet.opcode
            && largest.is_none_or(|(_, len)| packet.len > len)
        {
            largest = Some((opcode, packet.len));
        }
    }
    largest
}

/// The first and the last battery report.
pub(crate) fn battery(loaded: &Loaded) -> Option<(BatteryReport, BatteryReport)> {
    let first = loaded.batteries.first()?;
    let last = loaded.batteries.last()?;
    Some((*first, *last))
}

/// How long one START took to be answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_field_names,
    reason = "every field is a time in nanoseconds, and each name says so"
)]
pub(crate) struct Trial {
    pub(crate) start_ns: u64,
    /// To the first control packet from the pods; `None` when the mark that ends the start comes first.
    pub(crate) control_after_ns: Option<u64>,
    /// To the first audio SDU, the same way.
    pub(crate) audio_after_ns: Option<u64>,
}

/// One trial for each start mark, in order.
pub(crate) fn trials(loaded: &Loaded, marks: &[Mark]) -> Vec<Trial> {
    let controls = || loaded.controls.iter().map(|control| control.at_ns);
    let audio = || loaded.audio.iter().map(|sdu| sdu.at_ns);
    windows(marks, UPLINK)
        .into_iter()
        .map(|window| {
            let after = |at_ns: u64| at_ns.saturating_sub(window.start_ns);
            Trial {
                start_ns: window.start_ns,
                control_after_ns: controls().find(|&at_ns| window.holds(at_ns)).map(after),
                audio_after_ns: audio().find(|&at_ns| window.holds(at_ns)).map(after),
            }
        })
        .collect()
}

/// The start or stop mark closest in time to a packet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Nearest {
    pub(crate) kind: MarkKind,
    /// Which mark of its kind, counted from 1.
    pub(crate) ordinal: usize,
    /// The packet minus the mark; negative for a packet before the mark.
    pub(crate) offset_ms: f64,
}

/// One control packet from the pods, in full, with where it lies against the marks.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ControlReply {
    pub(crate) bytes: Vec<u8>,
    pub(crate) length_matches: bool,
    /// `None` when the run has no start or stop mark.
    pub(crate) nearest: Option<Nearest>,
}

/// The start or stop mark closest to `at_ns`, the earlier of two equally close.
fn nearest(marks: &[Mark], at_ns: u64) -> Option<Nearest> {
    let mut starts = 0_usize;
    let mut stops = 0_usize;
    let mut closest: Option<(u64, Nearest)> = None;
    for mark in marks {
        let of_kind = match mark.kind {
            MarkKind::Start => &mut starts,
            MarkKind::Stop => &mut stops,
            MarkKind::DictationStart
            | MarkKind::DictationStop
            | MarkKind::Truncated
            | MarkKind::Other => continue,
        };
        *of_kind = of_kind.saturating_add(1);
        let away_ns = at_ns.abs_diff(mark.at_ns);
        if closest.is_some_and(|(least_ns, _)| away_ns >= least_ns) {
            continue;
        }
        let offset_ms = if at_ns >= mark.at_ns {
            millis(away_ns)
        } else {
            -millis(away_ns)
        };
        let found = Nearest {
            kind: mark.kind,
            ordinal: *of_kind,
            offset_ms,
        };
        closest = Some((away_ns, found));
    }
    closest.map(|(_, found)| found)
}

/// Every control packet from the pods, in log order.
pub(crate) fn control_replies(loaded: &Loaded, marks: &[Mark]) -> Vec<ControlReply> {
    loaded
        .controls
        .iter()
        .map(|control| ControlReply {
            bytes: control.bytes.clone(),
            length_matches: control.length_matches,
            nearest: nearest(marks, control.at_ns),
        })
        .collect()
}

#[cfg(test)]
mod tests;
