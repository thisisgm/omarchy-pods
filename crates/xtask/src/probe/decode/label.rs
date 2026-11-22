//! The rate a decode is labelled with: the one rate the clock rows of the analysis put every start that holds audio near.

use super::super::analyse::ANALYSIS_FILE;
use super::super::analyse::report::{CLOCK_KEY, COARSE_CLOCK_KEY, NONE, Row, values};
use super::WAV_FILE;

/// The rate the stream is thought to run at, and the rate its configuration declares.
const TRUE_RATE_HZ: u32 = 64_000;
const DECLARED_RATE_HZ: u32 = 48_000;
/// A slope within this many percent of a rate is labelled with that rate.
const SLOPE_WITHIN_PERCENT: f64 = 1.0;
/// A coarse clock within this many percent of a rate is labelled with that rate: a 60 ms late arrival, the latest that is no gap, moves it 2 percent over the shortest span.
const COARSE_WITHIN_PERCENT: f64 = 5.0;
const PERCENT: f64 = 100.0;
const STARTS: &str = "starts";

#[derive(Debug, thiserror::Error)]
pub(crate) enum LabelError {
    #[error(
        "{ANALYSIS_FILE} gives the clock of no start, so the decoded audio has no rate to be labelled with"
    )]
    NoClock,
    #[error(
        "start {start} of {ANALYSIS_FILE} holds audio and gives neither {CLOCK_KEY} nor {COARSE_CLOCK_KEY} as a number: its clock was not measured, and the notes of its clock rows say why"
    )]
    ClockNotMeasured { start: usize },
    #[error(
        "{ANALYSIS_FILE} has {slopes} {CLOCK_KEY} rows and {coarse} {COARSE_CLOCK_KEY} rows, so the two rows of a start cannot be paired"
    )]
    RowsUnpaired { slopes: usize, coarse: usize },
    #[error(
        "{key} {hz} is within {within} percent of neither {TRUE_RATE_HZ} Hz nor {DECLARED_RATE_HZ} Hz, so the decoded audio has no rate to be labelled with"
    )]
    NeitherRate {
        key: &'static str,
        hz: String,
        within: f64,
    },
    #[error("the clock rows of {ANALYSIS_FILE} are not all near one rate")]
    ClocksDisagree,
}

/// The clock row a start gives its rate by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clock {
    /// The slope, which only a long start has.
    Slope,
    /// The coarse figure, read only where there is no slope.
    Coarse,
}

impl Clock {
    fn key(self) -> &'static str {
        match self {
            Self::Slope => CLOCK_KEY,
            Self::Coarse => COARSE_CLOCK_KEY,
        }
    }

    /// A figure of this row stands for a rate it is within this many percent of.
    fn within_percent(self) -> f64 {
        match self {
            Self::Slope => SLOPE_WITHIN_PERCENT,
            Self::Coarse => COARSE_WITHIN_PERCENT,
        }
    }

    fn rate_of(self, hz: f64) -> Option<u32> {
        // The distance is scaled, not divided: 60800 / 64000 - 1 comes to over 5 percent in floating point.
        let near = |rate: &u32| {
            let rate = f64::from(*rate);
            (hz - rate).abs() * PERCENT <= self.within_percent() * rate
        };
        [TRUE_RATE_HZ, DECLARED_RATE_HZ].into_iter().find(near)
    }
}

/// The rate of a run and how many of its starts each clock row gave it for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Label {
    pub(crate) rate_hz: u32,
    /// Starts labelled by their slope.
    by_slope: usize,
    /// Starts with no slope, labelled by their coarse clock.
    by_coarse: usize,
    /// Starts that hold no unit, so no sample of the decode is theirs and they need no rate.
    without_audio: usize,
}

impl Label {
    // Sample input: `[("refused", Some("63987")), ("63998.412", Some("64012"))]`, the slope and the coarse clock of each start in order; an analysis made before the coarse row has `None` for it.
    fn of(starts: &[(&str, Option<&str>)]) -> Result<Self, LabelError> {
        let mut rate_hz = None;
        let (mut by_slope, mut by_coarse, mut without_audio) = (0_usize, 0_usize, 0_usize);
        for (start, &(slope, coarse)) in (1_usize..).zip(starts) {
            let rows = [(Clock::Slope, Some(slope)), (Clock::Coarse, coarse)];
            let figure = rows.into_iter().find_map(|(clock, value)| {
                let hz: f64 = value?.parse().ok()?;
                Some((clock, value?, hz))
            });
            let Some((clock, value, hz)) = figure else {
                if coarse != Some(NONE) {
                    return Err(LabelError::ClockNotMeasured { start });
                }
                without_audio = without_audio.saturating_add(1);
                continue;
            };
            let rate = clock.rate_of(hz).ok_or_else(|| LabelError::NeitherRate {
                key: clock.key(),
                hz: value.to_owned(),
                within: clock.within_percent(),
            })?;
            if rate_hz.is_some_and(|other| other != rate) {
                return Err(LabelError::ClocksDisagree);
            }
            rate_hz = Some(rate);
            let counted = match clock {
                Clock::Slope => &mut by_slope,
                Clock::Coarse => &mut by_coarse,
            };
            *counted = counted.saturating_add(1);
        }
        Ok(Self {
            rate_hz: rate_hz.ok_or(LabelError::NoClock)?,
            by_slope,
            by_coarse,
            without_audio,
        })
    }

    /// The label the clock rows of `analysis` give, the two rows of one start paired by their order.
    // Sample input: `clock_hz\trefused\tHz\tstart 1; ...` then `clock_hz_coarse\t63987\tHz\tstart 1; ...`, such a pair for each start; an analysis made before the coarse row has none of the second.
    pub(crate) fn read(analysis: &str) -> Result<Self, LabelError> {
        let (slopes, coarse) = (
            values(analysis, CLOCK_KEY),
            values(analysis, COARSE_CLOCK_KEY),
        );
        if !coarse.is_empty() && coarse.len() != slopes.len() {
            return Err(LabelError::RowsUnpaired {
                slopes: slopes.len(),
                coarse: coarse.len(),
            });
        }
        let starts: Vec<(&str, Option<&str>)> = slopes
            .into_iter()
            .enumerate()
            .map(|(index, slope)| (slope, coarse.get(index).copied()))
            .collect();
        Self::of(&starts)
    }

    /// The label and where it came from: how many starts each clock row spoke for.
    pub(crate) fn rows(&self) -> [Row; 4] {
        let from = |clock: Clock, starts: usize, which: &str| {
            let (key, within) = (clock.key(), clock.within_percent());
            let note = format!("{which} {key} is within {within} percent of the label");
            Row::new(&format!("rate_label_from_{key}"), &starts, STARTS, &note)
        };
        [
            Row::new(
                "rate_label",
                &self.rate_hz,
                "Hz",
                &format!(
                    "the one rate every start that holds audio is near; written into {WAV_FILE}"
                ),
            ),
            from(Clock::Slope, self.by_slope, "starts whose"),
            from(
                Clock::Coarse,
                self.by_coarse,
                "starts too short for a slope, whose",
            ),
            Row::new(
                "rate_label_starts_without_audio",
                &self.without_audio,
                STARTS,
                "starts that hold no unit: no sample of the decode is theirs",
            ),
        ]
    }
}

#[cfg(test)]
mod tests;
