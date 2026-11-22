//! The figures of one dictation session: its skips, its timing, its lead and the rates of what follows its burst.

use std::collections::BTreeMap;
use std::time::Duration;

use pods_proto::sdulog::Record;

use super::super::lead::{Arrival, Lead, lead};
use super::super::load::{Window, segment};
use super::super::rates::{ClockEstimate, Floor, Thin, clock_of};
use super::super::stats::{NS_PER_MS, per_second, to_f64};
use super::load::{Answers, LoadedAtt, load};

/// How long one frame lasts: what the table of contents byte of every frame seen so far says.
pub(crate) const FRAME_MS: u32 = 20;
/// The sample clock a wideband stream is taken to run at; no packet states it, so every row scaled by it says it is assumed.
pub(crate) const REFERENCE_HZ: u32 = 16_000;
const MS_PER_SECOND: u32 = 1000;
/// Samples in one frame at the reference clock, 320.
pub(crate) const SAMPLES_PER_FRAME: u32 = REFERENCE_HZ * FRAME_MS / MS_PER_SECOND;
/// What the packets after the burst must hold before a rate is formed from them.
pub(crate) const FLOOR: Floor = Floor {
    sdus: 500,
    span_ns: 60_000_000_000,
};

/// One audio notification that brought a frame: when it came, the frames delivered by then and the coded bytes it brought.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Heard {
    pub(crate) arrival: Arrival,
    pub(crate) bytes: u64,
}

/// A step of a counter that is not one: the value before it and the value after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Skip {
    pub(crate) before: u16,
    pub(crate) after: u16,
}

/// What followed the stop mark of a session, up to the next start mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AfterStop {
    /// No stop mark ends the session.
    Unmarked,
    /// The pods did not answer the stop write.
    Unanswered,
    Answered {
        reply_after_ns: u64,
        /// Audio notifications logged after the reply.
        audio_after_reply: usize,
    },
}

/// The stream of one start mark, cut at the mark that ends it.
#[derive(Debug)]
pub(crate) struct Session {
    pub(crate) start_ns: u64,
    pub(crate) stream: LoadedAtt,
    pub(crate) after_stop: AfterStop,
}

impl Session {
    pub(crate) fn cut(records: &[Record], window: Window, tail: Option<Window>) -> Self {
        let after_stop = tail.map_or(AfterStop::Unmarked, |tail| {
            let late = load(segment(records, tail));
            let answered = late
                .replies
                .iter()
                .find(|reply| reply.answers == Answers::Stop);
            answered.map_or(AfterStop::Unanswered, |reply| AfterStop::Answered {
                reply_after_ns: reply.at_ns.saturating_sub(tail.start_ns),
                audio_after_reply: late
                    .audio
                    .iter()
                    .filter(|packet| packet.record > reply.record)
                    .count(),
            })
        });
        Self {
            start_ns: window.start_ns,
            stream: load(segment(records, window)),
            after_stop,
        }
    }

    /// From the start mark to the reply of the pods.
    pub(crate) fn reply_after_ns(&self) -> Option<u64> {
        let replies = self.stream.replies.iter();
        let reply = replies
            .into_iter()
            .find(|reply| reply.answers == Answers::Start)?;
        Some(reply.at_ns.saturating_sub(self.start_ns))
    }

    /// From the start mark to the first audio notification.
    pub(crate) fn audio_after_ns(&self) -> Option<u64> {
        let first = self.stream.audio.first()?;
        Some(first.at_ns.saturating_sub(self.start_ns))
    }

    /// The steps of the packet counter that are not 1 modulo 256, each notification against the one before it.
    pub(crate) fn packet_skips(&self) -> Vec<Skip> {
        let counters: Vec<u8> = self
            .stream
            .audio
            .iter()
            .map(|packet| packet.counter)
            .collect();
        let steps = counters.iter().zip(counters.iter().skip(1));
        steps
            .filter(|(before, after)| after.wrapping_sub(**before) != 1)
            .map(|(before, after)| Skip {
                before: u16::from(*before),
                after: u16::from(*after),
            })
            .collect()
    }

    /// The steps of the frame counter that are not 1, each frame against the one before it, across notifications.
    pub(crate) fn frame_skips(&self) -> Vec<Skip> {
        let counters: Vec<u16> = self.stream.frames().map(|frame| frame.counter).collect();
        let steps = counters.iter().zip(counters.iter().skip(1));
        steps
            .filter(|(before, after)| after.wrapping_sub(**before) != 1)
            .map(|(&before, &after)| Skip { before, after })
            .collect()
    }

    /// Every audio notification with a whole frame; one with none brings no audio and cannot place its start.
    pub(crate) fn heard(&self) -> Vec<Heard> {
        let mut frames_done = 0_u64;
        let with_a_frame = self
            .stream
            .audio
            .iter()
            .filter(|packet| !packet.frames.is_empty());
        let heard = with_a_frame.map(|packet| {
            let mut bytes = 0_u64;
            for frame in &packet.frames {
                // No Rust target has a usize wider than 64 bits, so the fallback is never taken.
                bytes = bytes.saturating_add(u64::try_from(frame.len).unwrap_or(u64::MAX));
                frames_done = frames_done.saturating_add(1);
            }
            let at_ns = packet.at_ns;
            Heard {
                arrival: Arrival { at_ns, frames_done },
                bytes,
            }
        });
        heard.collect()
    }
}

fn arrivals(heard: &[Heard]) -> Vec<Arrival> {
    heard.iter().map(|packet| packet.arrival).collect()
}

/// Where the stream starts against its start mark, and the burst it opens with.
pub(crate) fn lead_of(heard: &[Heard], start_ns: u64) -> Option<Lead> {
    lead(&arrivals(heard), start_ns, f64::from(FRAME_MS) * NS_PER_MS)
}

/// The packets that follow the burst `lead` found.
pub(crate) fn after_burst<'a>(heard: &'a [Heard], lead: &Lead) -> &'a [Heard] {
    // No Rust target has a usize narrower than 32 bits, so the fallback is never taken.
    let burst = usize::try_from(lead.burst_packets).unwrap_or(usize::MAX);
    heard.get(burst..).unwrap_or(&[])
}

/// The frame clock of the packets after the burst.
pub(crate) fn frame_clock(steady: &[Heard]) -> Result<ClockEstimate, Thin> {
    clock_of(&arrivals(steady), SAMPLES_PER_FRAME, FLOOR)
}

/// The coded bits the packets after the burst carry each second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Bitrate {
    /// Frame bytes after the first packet, times eight, over the span.
    pub(crate) mean: f64,
    /// The whole seconds after the first packet that the three figures below are taken over.
    pub(crate) seconds: usize,
    pub(crate) second_min: u64,
    pub(crate) second_median: u64,
    pub(crate) second_max: u64,
}

fn bits_of(bytes: u64) -> u64 {
    bytes.saturating_mul(u64::from(u8::BITS))
}

/// The bitrate of the packets after the burst, as a mean and over each whole second; a second takes its last instant and not its first.
pub(crate) fn bitrate(steady: &[Heard]) -> Result<Bitrate, Thin> {
    let none = Thin {
        sdus: 0,
        span_ns: 0,
    };
    let (Some((first, rest)), Some(last)) = (steady.split_first(), steady.last()) else {
        return Err(none);
    };
    let (first_ns, sdus) = (first.arrival.at_ns, steady.len());
    let span_ns = FLOOR.span(sdus, first_ns, last.arrival.at_ns)?;
    let thin = Thin { sdus, span_ns };
    let whole_seconds = Duration::from_nanos(span_ns).as_secs();
    let last_second = whole_seconds.checked_sub(1).ok_or(thin)?;
    let mut filled = BTreeMap::<u64, u64>::new();
    let mut bytes = 0_u64;
    for packet in rest {
        bytes = bytes.saturating_add(packet.bytes);
        let since_ns = packet.arrival.at_ns.saturating_sub(first_ns);
        // The first second also takes a packet logged at the very instant of the first one.
        let second = Duration::from_nanos(since_ns.saturating_sub(1)).as_secs();
        if second <= last_second {
            let bits = filled.entry(second).or_default();
            *bits = bits.saturating_add(bits_of(packet.bytes));
        }
    }
    let mut filled: Vec<u64> = filled.into_values().collect();
    filled.sort_unstable();
    let empty = whole_seconds.saturating_sub(u64::try_from(filled.len()).or(Err(thin))?);
    // A second no packet fell in reads zero, sorts before every filled one, and is counted, never stored.
    let ranked = |rank: u64| match rank.checked_sub(empty) {
        None => Some(0),
        Some(rank) => filled.get(usize::try_from(rank).ok()?).copied(),
    };
    let bits = to_f64(bits_of(bytes));
    Ok(Bitrate {
        mean: per_second(bits, span_ns).ok_or(thin)?,
        seconds: usize::try_from(whole_seconds).or(Err(thin))?,
        second_min: ranked(0).ok_or(thin)?,
        second_median: ranked(last_second / 2).ok_or(thin)?,
        second_max: ranked(last_second).ok_or(thin)?,
    })
}
