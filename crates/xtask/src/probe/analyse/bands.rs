//! The band limit a unit declares in its first bits, and the spectral line and frequency that limit stands for.

use pods_mic::decode::BAND_EDGES;

use super::load::Loaded;

/// Entries in the band table: the limits 0 to 35.
const BANDS: usize = BAND_EDGES.len();
pub(crate) const LAST_BAND: usize = BANDS - 1;
/// The band limit is in the second byte of a unit, after the eight bits of the global gain.
const MAX_SFB_BYTE: usize = 1;
/// The band limit is the top six bits of that byte.
const MAX_SFB_SHIFT: u32 = 2;
/// The table spans 480 lines, which cover half the sample clock, so one line is the clock over 960 wide.
pub(crate) const LINES_PER_CLOCK: f64 = 960.0;
/// The sample clock the band table was made for; a stream on another clock may use another table.
pub(crate) const TABLE_CLOCK_HZ: u32 = 48_000;

// Sample input: 00 7c ..., whose bits 8 to 13 read 31; a unit under two bytes declares none.
pub(crate) fn max_sfb(unit: &[u8]) -> Option<u8> {
    unit.get(MAX_SFB_BYTE).map(|byte| byte >> MAX_SFB_SHIFT)
}

/// The first spectral line above the band limit `max_sfb`; `None` for a limit over `LAST_BAND`.
pub(crate) fn band_line(max_sfb: u8) -> Option<u16> {
    BAND_EDGES.get(usize::from(max_sfb)).copied()
}

/// The frequency of spectral line `line` on a sample clock of `clock_hz`.
pub(crate) fn band_hz(line: u16, clock_hz: f64) -> f64 {
    f64::from(line) * clock_hz / LINES_PER_CLOCK
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MaxSfbReport {
    pub(crate) histogram: [u32; BANDS],
    /// Units whose limit is over `LAST_BAND`, which the table does not map.
    pub(crate) invalid: u32,
    /// Units under two bytes, which declare no limit.
    pub(crate) unread: u32,
}

impl MaxSfbReport {
    pub(crate) fn max(&self) -> Option<u8> {
        (0_u8..)
            .zip(self.histogram)
            .filter(|&(_, units)| units > 0)
            .map(|(limit, _)| limit)
            .last()
    }
}

pub(crate) fn bandwidth(loaded: &Loaded) -> MaxSfbReport {
    let mut report = MaxSfbReport {
        histogram: [0; BANDS],
        invalid: 0,
        unread: 0,
    };
    for unit in loaded.units() {
        let Some(limit) = unit.max_sfb else {
            report.unread = report.unread.saturating_add(1);
            continue;
        };
        match report.histogram.get_mut(usize::from(limit)) {
            Some(units) => *units = units.saturating_add(1),
            None => report.invalid = report.invalid.saturating_add(1),
        }
    }
    report
}

#[cfg(test)]
mod tests;
