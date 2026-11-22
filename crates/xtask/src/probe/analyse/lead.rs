//! How far the first frame of a stream lies before its start command, and the burst that delivers the backlog.

use super::load::Loaded;
use super::stats::{NS_PER_MS, millis, to_f64};

/// The earliest arrival line is taken from the packets of this long after the start.
pub(crate) const LEAD_WINDOW_NS: u64 = 10_000_000_000;
/// No lead is given from fewer packets than this inside the window.
pub(crate) const LEAD_MIN_PACKETS: usize = 50;
/// A packet is part of the burst while it finds more audio than this owed: a steady uplink packet finds 30 ms and a steady dictation packet 20 ms.
const BURST_OWED_NS: f64 = 52_500_000.0;

/// When a packet arrived and how many frames the stream had delivered once it had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Arrival {
    pub(crate) at_ns: u64,
    pub(crate) frames_done: u64,
}

impl Arrival {
    /// Every audio SDU with a whole unit as an arrival: when it came and the units delivered by then; one with none brings no audio and cannot place its start.
    pub(crate) fn of(loaded: &Loaded) -> Vec<Self> {
        let mut frames_done = 0_u64;
        let heard = loaded.audio.iter().filter(|sdu| !sdu.units.is_empty());
        let arrivals = heard.map(|sdu| {
            // No Rust target has a usize wider than 64 bits, so the fallback is never taken.
            let frames = u64::try_from(sdu.units.len()).unwrap_or(u64::MAX);
            frames_done = frames_done.saturating_add(frames);
            let at_ns = sdu.at_ns;
            Self { at_ns, frames_done }
        });
        arrivals.collect()
    }
}

/// Where a stream starts against its start command.
#[derive(Debug, Clone, Copy, PartialEq)]
#[expect(
    clippy::struct_field_names,
    reason = "each field is named as the key of the row it is printed in"
)]
pub(crate) struct Lead {
    /// The start command minus the earliest arrival line; positive when the first frame is older than the command.
    pub(crate) lead_ms: f64,
    /// Packets of the leading run that each found more than `BURST_OWED_NS` of audio owed: how far it came behind the line and the frames it brought.
    pub(crate) burst_packets: u32,
    /// Frames delivered by the end of that run.
    pub(crate) burst_frames: u32,
    /// From the first packet of the run to its last.
    pub(crate) burst_ms: f64,
}

/// The lead of `arrivals` against the start at `start_ns`, each frame lasting `frame_ns`.
pub(crate) fn lead(arrivals: &[Arrival], start_ns: u64, frame_ns: f64) -> Option<Lead> {
    if !(frame_ns.is_finite() && frame_ns > 0.0) {
        return None;
    }
    // A packet cannot arrive before its last frame ends, so arrival less the frames is never before the first frame.
    let delay = |arrival: &Arrival| to_f64(arrival.at_ns) - to_f64(arrival.frames_done) * frame_ns;
    let window_end_ns = start_ns.saturating_add(LEAD_WINDOW_NS);
    let in_window = arrivals
        .iter()
        .filter(|arrival| arrival.at_ns < window_end_ns);
    if in_window.clone().count() < LEAD_MIN_PACKETS {
        return None;
    }
    let zero = in_window.map(delay).reduce(f64::min)?;
    let owed =
        |at_ns: u64, frames_before: u64| to_f64(at_ns) - to_f64(frames_before) * frame_ns - zero;
    let frames_before = [0]
        .into_iter()
        .chain(arrivals.iter().map(|arrival| arrival.frames_done));
    let burst: Vec<&Arrival> = arrivals
        .iter()
        .zip(frames_before)
        .take_while(|(arrival, frames_before)| owed(arrival.at_ns, *frames_before) > BURST_OWED_NS)
        .map(|(arrival, _)| arrival)
        .collect();
    let (burst_frames, burst_ms) = match (burst.first(), burst.last()) {
        (Some(first), Some(last)) => (
            u32::try_from(last.frames_done).unwrap_or(u32::MAX),
            millis(last.at_ns.saturating_sub(first.at_ns)),
        ),
        _ => (0, 0.0),
    };
    Some(Lead {
        lead_ms: (to_f64(start_ns) - zero) / NS_PER_MS,
        burst_packets: u32::try_from(burst.len()).unwrap_or(u32::MAX),
        burst_frames,
        burst_ms,
    })
}

#[cfg(test)]
mod tests;
