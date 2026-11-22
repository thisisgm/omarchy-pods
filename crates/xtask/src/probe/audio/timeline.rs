//! Places the samples of the stream on the clock of the marks, by the arrival of the unit each sample came in, and cuts the stream at the cues.

use std::ops::Range;

use super::super::analyse::UNITS_FILE;
use super::super::analyse::input::{Cue, Mark, MarkKind};
use super::super::analyse::load::Window;
use super::super::script::Asked;

/// The name of the units table column that holds the arrival of a unit's SDU.
const ARRIVAL_NAME: &str = "arrival_ns";
/// A window opens this long after its cue, which is the time the owner takes to start.
const AFTER_CUE_NS: u64 = 1_000_000_000;

/// Why the units table was refused; it names the line and never repeats it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum TimelineError {
    #[error("{UNITS_FILE} names no {ARRIVAL_NAME} column in its first line")]
    Header,
    #[error(
        "{UNITS_FILE} line {line} has no arrival in nanoseconds that is at or after the one before"
    )]
    Arrival { line: usize },
}

/// When each unit of the stream arrived; sample k of the stream came in unit k / `samples_per_unit`.
#[derive(Debug)]
pub(crate) struct Timeline {
    arrivals_ns: Vec<u64>,
    samples_per_unit: usize,
}

impl Timeline {
    // Sample input: "index\tsdu\tarrival_ns\tstamp\tlen\tmax_sfb\n0\t3\t30000000\t0\t2\t0\n", the units table of an analysis.
    pub(crate) fn parse(table: &str, samples_per_unit: usize) -> Result<Self, TimelineError> {
        let mut lines = table.lines();
        let mut names = lines.next().unwrap_or_default().split('\t');
        let column = names.position(|name| name == ARRIVAL_NAME);
        let column = column.ok_or(TimelineError::Header)?;
        let mut arrivals_ns: Vec<u64> = Vec::new();
        for (index, line) in lines.enumerate() {
            let field = line.split('\t').nth(column);
            let arrival = field.and_then(|field| field.parse::<u64>().ok());
            let in_order =
                arrival.filter(|at_ns| arrivals_ns.last().is_none_or(|last| at_ns >= last));
            // The header is line 1.
            let line = index.saturating_add(2);
            arrivals_ns.push(in_order.ok_or(TimelineError::Arrival { line })?);
        }
        Ok(Self {
            arrivals_ns,
            samples_per_unit,
        })
    }

    /// The samples every unit of the table holds together.
    pub(crate) fn samples(&self) -> usize {
        self.arrivals_ns.len().saturating_mul(self.samples_per_unit)
    }

    /// The samples of the units that arrived inside `window`: one stretch, because units are in arrival order.
    pub(crate) fn stretch(&self, window: Window) -> Range<usize> {
        let unit_at = |at_ns: u64| self.arrivals_ns.partition_point(|&arrival| arrival < at_ns);
        let from = unit_at(window.start_ns);
        let until = window
            .end_ns
            .map_or(self.arrivals_ns.len(), unit_at)
            .max(from);
        from.saturating_mul(self.samples_per_unit)..until.saturating_mul(self.samples_per_unit)
    }

    /// When the unit that holds `sample` arrived.
    pub(crate) fn arrival_ns(&self, sample: usize) -> Option<u64> {
        let unit = sample.checked_div(self.samples_per_unit)?;
        self.arrivals_ns.get(unit).copied()
    }
}

/// The stretch one cue covers: from a second after it to the next cue or stop mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CueWindow {
    /// `None` for a cue no measure reads.
    pub(crate) asked: Option<Asked>,
    pub(crate) window: Window,
}

/// One window for each cue, in order.
pub(crate) fn cue_windows(cues: &[Cue<'_>], marks: &[Mark]) -> Vec<CueWindow> {
    let stops = marks.iter().filter(|mark| mark.kind == MarkKind::Stop);
    let ends: Vec<u64> = stops
        .map(|mark| mark.at_ns)
        .chain(cues.iter().map(|cue| cue.at_ns))
        .collect();
    let window_of = |cue: &Cue<'_>| {
        let later = ends.iter().copied().filter(|&end_ns| end_ns > cue.at_ns);
        CueWindow {
            asked: Asked::by(cue.text),
            window: Window {
                start_ns: cue.at_ns.saturating_add(AFTER_CUE_NS),
                end_ns: later.min(),
            },
        }
    };
    cues.iter().map(window_of).collect()
}
