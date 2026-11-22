//! The rates of one stretch of audio, refused when the stretch is too thin to support them.

use super::lead::Arrival;
use super::load::{AudioPacket, Loaded};
use super::stats::{count, least_squares, median, mode, per_second, ppm, seconds, share, to_f64};

/// A rate needs at least this long between the first and the last audio SDU: half a one minute soak, which ends just under 60 s.
pub(crate) const THIN_MIN_SPAN_NS: u64 = 30_000_000_000;
pub(crate) const THIN_MIN_SDUS: usize = 500;
const UPLINK_FLOOR: Floor = Floor {
    sdus: THIN_MIN_SDUS,
    span_ns: THIN_MIN_SPAN_NS,
};
/// The slope and the first to last ratio must agree within this many parts per million.
const DISAGREE_PPM: f64 = 50.0;
const BITS_PER_BYTE: f64 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Thin {
    pub(crate) sdus: usize,
    /// From the first audio SDU to the last.
    pub(crate) span_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Floor {
    pub(crate) sdus: usize,
    pub(crate) span_ns: u64,
}

impl Floor {
    pub(crate) fn span(self, sdus: usize, first_ns: u64, last_ns: u64) -> Result<u64, Thin> {
        let span_ns = last_ns.saturating_sub(first_ns);
        if sdus < self.sdus || span_ns < self.span_ns {
            return Err(Thin { sdus, span_ns });
        }
        Ok(span_ns)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Cadence {
    /// Arrival intervals over the span.
    pub(crate) sdus_per_second: f64,
    /// The most common count of units in an SDU.
    pub(crate) units_per_sdu: usize,
    /// The fraction of SDUs with that count.
    pub(crate) units_per_sdu_share: f64,
    pub(crate) interval_min_ns: u64,
    pub(crate) interval_median_ns: u64,
    pub(crate) interval_max_ns: u64,
}

/// The rate of the peer's unit clock, measured two ways.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ClockEstimate {
    /// The least squares slope of units completed against arrival time.
    pub(crate) units_per_second: f64,
    /// Half the 95 percent interval of that slope.
    pub(crate) half_width: f64,
    /// Units completed after the first SDU over the span.
    pub(crate) ratio: f64,
    pub(crate) samples_per_unit: u32,
}

impl ClockEstimate {
    fn scaled(&self, units_per_second: f64) -> f64 {
        units_per_second * f64::from(self.samples_per_unit)
    }

    pub(crate) fn hz(&self) -> f64 {
        self.scaled(self.units_per_second)
    }

    pub(crate) fn hz_low(&self) -> f64 {
        self.scaled(self.units_per_second - self.half_width)
    }

    pub(crate) fn hz_high(&self) -> f64 {
        self.scaled(self.units_per_second + self.half_width)
    }

    pub(crate) fn ppm_vs(&self, reference_hz: f64) -> f64 {
        ppm(self.hz(), reference_hz)
    }

    pub(crate) fn disagree(&self) -> bool {
        ppm(self.units_per_second, self.ratio).abs() > DISAGREE_PPM
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Bitrate {
    /// Coded unit bytes after the first SDU, times eight, over the span.
    pub(crate) audio_bits_per_second: f64,
    /// Whole SDUs after the first, times eight, over the span.
    pub(crate) sdu_bits_per_second: f64,
    /// The shortest and the longest unit, `None` when no SDU holds a unit.
    pub(crate) unit_bytes: Option<(usize, usize)>,
}

/// A stretch long enough for a rate: the SDUs after its first and the time they span.
struct Stretch<'a> {
    rest: &'a [AudioPacket],
    span_ns: u64,
    /// What a figure that cannot be formed after all is refused with.
    thin: Thin,
}

fn stretch(loaded: &Loaded) -> Result<Stretch<'_>, Thin> {
    let sdus = loaded.audio.len();
    let (Some((first, rest)), Some(last)) = (loaded.audio.split_first(), loaded.audio.last())
    else {
        return Err(Thin { sdus, span_ns: 0 });
    };
    let span_ns = UPLINK_FLOOR.span(sdus, first.at_ns, last.at_ns)?;
    Ok(Stretch {
        rest,
        span_ns,
        thin: Thin { sdus, span_ns },
    })
}

pub(crate) fn cadence(loaded: &Loaded) -> Result<Cadence, Thin> {
    let Stretch {
        rest,
        span_ns,
        thin,
        ..
    } = stretch(loaded)?;
    let mut intervals: Vec<u64> = loaded
        .audio
        .iter()
        .zip(rest)
        .map(|(before, after)| after.at_ns.saturating_sub(before.at_ns))
        .collect();
    intervals.sort_unstable();
    let unit_counts = loaded.audio.iter().map(|sdu| sdu.units.len());
    let (units_per_sdu, with_that_count) = mode(unit_counts).ok_or(thin)?;
    Ok(Cadence {
        sdus_per_second: per_second(count(rest.len()), span_ns).ok_or(thin)?,
        units_per_sdu,
        units_per_sdu_share: share(with_that_count, loaded.audio.len()).ok_or(thin)?,
        interval_min_ns: intervals.first().copied().ok_or(thin)?,
        interval_median_ns: median(&intervals).ok_or(thin)?,
        interval_max_ns: intervals.last().copied().ok_or(thin)?,
    })
}

/// Every audio SDU counts, one with no unit too.
pub(crate) fn clock(loaded: &Loaded, samples_per_unit: u32) -> Result<ClockEstimate, Thin> {
    let mut frames_done = 0_u64;
    let arrivals: Vec<Arrival> = loaded
        .audio
        .iter()
        .map(|sdu| {
            // No Rust target has a usize wider than 64 bits, so the fallback is never taken.
            let units = u64::try_from(sdu.units.len()).unwrap_or(u64::MAX);
            frames_done = frames_done.saturating_add(units);
            let at_ns = sdu.at_ns;
            Arrival { at_ns, frames_done }
        })
        .collect();
    clock_of(&arrivals, samples_per_unit, UPLINK_FLOOR)
}

/// The clock behind `arrivals`: the slope of frames delivered against the arrival of the packet that completed them, beside the plain ratio.
pub(crate) fn clock_of(
    arrivals: &[Arrival],
    samples_per_unit: u32,
    floor: Floor,
) -> Result<ClockEstimate, Thin> {
    let (Some(first), Some(last)) = (arrivals.first(), arrivals.last()) else {
        return Err(Thin {
            sdus: 0,
            span_ns: 0,
        });
    };
    let sdus = arrivals.len();
    let span_ns = floor.span(sdus, first.at_ns, last.at_ns)?;
    let thin = Thin { sdus, span_ns };
    let points: Vec<(f64, f64)> = arrivals
        .iter()
        .map(|arrival| {
            let since_first_ns = arrival.at_ns.saturating_sub(first.at_ns);
            (seconds(since_first_ns), to_f64(arrival.frames_done))
        })
        .collect();
    let fit = least_squares(&points).ok_or(thin)?;
    let after_first = last.frames_done.saturating_sub(first.frames_done);
    Ok(ClockEstimate {
        units_per_second: fit.slope,
        half_width: fit.half_width,
        ratio: per_second(to_f64(after_first), span_ns).ok_or(thin)?,
        samples_per_unit,
    })
}

pub(crate) fn bitrate(loaded: &Loaded) -> Result<Bitrate, Thin> {
    let Stretch {
        rest,
        span_ns,
        thin,
        ..
    } = stretch(loaded)?;
    let unit_bytes = rest
        .iter()
        .flat_map(|sdu| &sdu.units)
        .fold(0_usize, |bytes, unit| bytes.saturating_add(unit.len));
    let sdu_bytes = rest
        .iter()
        .fold(0_usize, |bytes, sdu| bytes.saturating_add(sdu.len));
    let bits_per_second = |bytes: usize| per_second(count(bytes) * BITS_PER_BYTE, span_ns);
    let lengths = || loaded.units().map(|unit| unit.len);
    Ok(Bitrate {
        audio_bits_per_second: bits_per_second(unit_bytes).ok_or(thin)?,
        sdu_bits_per_second: bits_per_second(sdu_bytes).ok_or(thin)?,
        unit_bytes: lengths().min().zip(lengths().max()),
    })
}

#[cfg(test)]
mod tests;
