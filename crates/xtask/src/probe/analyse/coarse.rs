//! The unit clock of a stretch from the pace of its arrivals after the opening: coarse, for telling rates a third apart in a stretch too short for a slope.

use super::lead::Arrival;
use super::rates::Thin;
use super::stats::{per_second, to_f64};

/// The opening left out of the span: a backlog delivered there, faster than it was recorded, would raise the figure.
pub(crate) const COARSE_OPENING_NS: u64 = 1_000_000_000;
/// The span needs this long from its first audio SDU to its last.
pub(crate) const COARSE_MIN_SPAN_NS: u64 = 3_000_000_000;
/// The span needs this many audio SDUs: those three seconds at 25 a second, the slower of the two rates the figure tells apart.
pub(crate) const COARSE_MIN_SDUS: usize = 75;
/// The two halves of the span must put the clock within this many percent of the figure of each other: a 60 ms late middle SDU, the latest that is no gap, moves them 8 apart over the shortest span.
pub(crate) const HALVES_WITHIN_PERCENT: f64 = 10.0;
const PERCENT: f64 = 100.0;

/// The rate of the peer's unit clock from the arrivals after the opening of a stretch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CoarseClock {
    /// Units delivered after the first SDU of the span, over the span.
    pub(crate) units_per_second: f64,
    /// From the first audio SDU after the opening to the last.
    pub(crate) span_ns: u64,
    /// Audio SDUs in the span.
    pub(crate) sdus: usize,
    /// How far apart the two halves of the span put the clock, in percent of the figure.
    pub(crate) halves_apart_percent: f64,
    pub(crate) samples_per_unit: u32,
}

impl CoarseClock {
    pub(crate) fn hz(&self) -> f64 {
        self.units_per_second * f64::from(self.samples_per_unit)
    }
}

/// Why a stretch has no coarse clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum NoCoarse {
    /// The stretch has no arrival: no SDU of it holds a whole unit.
    NoAudio,
    /// Too little after the opening, with what the span did hold.
    Thin(Thin),
    /// The halves of the span put the clock this many percent apart, so the delay of the arrivals changed inside it; `None` when a half has no time in it.
    Unsteady { halves_apart_percent: Option<f64> },
}

/// Units delivered after the first of `arrivals`, over the time to the last of them.
fn units_per_second(arrivals: &[Arrival]) -> Option<f64> {
    let (first, last) = (arrivals.first()?, arrivals.last()?);
    let units = last.frames_done.saturating_sub(first.frames_done);
    per_second(to_f64(units), last.at_ns.saturating_sub(first.at_ns))
}

/// The pace of the SDUs that arrive `COARSE_OPENING_NS` or more after the first, which is the unit clock while their delay does not change: the figure is off by the change of delay from the first to the last, over the span.
pub(crate) fn coarse_clock(
    arrivals: &[Arrival],
    samples_per_unit: u32,
) -> Result<CoarseClock, NoCoarse> {
    let first = arrivals.first().ok_or(NoCoarse::NoAudio)?;
    let opening_end_ns = first.at_ns.saturating_add(COARSE_OPENING_NS);
    let after_opening = arrivals.partition_point(|arrival| arrival.at_ns < opening_end_ns);
    let span = arrivals.get(after_opening..).unwrap_or(&[]);
    let span_ns = match (span.first(), span.last()) {
        (Some(first), Some(last)) => last.at_ns.saturating_sub(first.at_ns),
        _ => 0,
    };
    let thin = Thin {
        sdus: span.len(),
        span_ns,
    };
    if span.len() < COARSE_MIN_SDUS || span_ns < COARSE_MIN_SPAN_NS {
        return Err(NoCoarse::Thin(thin));
    }
    let whole = units_per_second(span).ok_or(NoCoarse::Thin(thin))?;
    // The two halves share the middle SDU: it ends the first and the second counts from it.
    let middle = span.len() / 2;
    let first_half = span.get(..=middle).and_then(units_per_second);
    let second_half = span.get(middle..).and_then(units_per_second);
    let apart = first_half
        .zip(second_half)
        .map(|(first, second)| (first - second).abs() / whole * PERCENT);
    match apart {
        Some(halves_apart_percent) if halves_apart_percent <= HALVES_WITHIN_PERCENT => {
            Ok(CoarseClock {
                units_per_second: whole,
                span_ns,
                sdus: span.len(),
                halves_apart_percent,
                samples_per_unit,
            })
        }
        _ => Err(NoCoarse::Unsteady {
            halves_apart_percent: apart,
        }),
    }
}

#[cfg(test)]
mod tests;
