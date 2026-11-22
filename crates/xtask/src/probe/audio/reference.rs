//! A second recording of the same sound, placed on the clock of the run by the wall clock time of its first sample.

use std::ops::Range;

use super::super::analyse::load::Window;
use super::super::wav::Wav;
use super::levels::samples_in;

const NS_PER_SECOND: i128 = 1_000_000_000;
const NS_PER_MS: i128 = 1_000_000;

/// The reference and where its first sample lies on the clock of the marks; that place is only as good as the recorder's clock.
#[derive(Debug)]
pub(crate) struct Reference<'a> {
    pub(crate) wav: &'a Wav,
    /// The stamp a mark made at the reference's first sample would carry; before the zero of the run it is negative.
    first_sample_ns: i128,
}

impl<'a> Reference<'a> {
    pub(crate) fn new(wav: &'a Wav, start_ns: u64, realtime_at_zero_ns: u64) -> Self {
        let first_sample_ns = i128::from(start_ns).saturating_sub(i128::from(realtime_at_zero_ns));
        Self {
            wav,
            first_sample_ns,
        }
    }

    /// How far into the reference the stamp `at_ns` lies, in whole milliseconds rounded down; negative before its first sample.
    pub(crate) fn ms_at(&self, at_ns: u64) -> Option<i64> {
        let into_ns = i128::from(at_ns).saturating_sub(self.first_sample_ns);
        i64::try_from(into_ns.div_euclid(NS_PER_MS)).ok()
    }

    /// Whether the reference holds every sample from `from_ms` to `until_ms` after its first.
    pub(crate) fn holds(&self, from_ms: i64, until_ms: i64) -> bool {
        let until = u32::try_from(until_ms).ok();
        let end = until.map(|until_ms| samples_in(until_ms, self.wav.rate));
        from_ms >= 0 && end.is_some_and(|end| end <= self.wav.samples.len())
    }

    /// The stamp the reference sample `sample` would carry; `None` for a rate of zero, which the reader refuses.
    pub(crate) fn stamp_ns(&self, sample: usize) -> Option<i128> {
        let into_ns = i128::try_from(sample)
            .ok()?
            .saturating_mul(NS_PER_SECOND)
            .checked_div(i128::from(self.wav.rate))?;
        Some(self.first_sample_ns.saturating_add(into_ns))
    }

    fn sample_at(&self, at_ns: u64) -> Option<usize> {
        let into_ns = i128::from(at_ns).saturating_sub(self.first_sample_ns);
        let sample = into_ns.saturating_mul(i128::from(self.wav.rate)) / NS_PER_SECOND;
        usize::try_from(sample).ok()
    }

    pub(crate) fn stretch(&self, window: Window) -> Option<(usize, &'a [f32])> {
        let range: Range<usize> =
            self.sample_at(window.start_ns)?..self.sample_at(window.end_ns?)?;
        Some((range.start, self.wav.samples.get(range)?))
    }
}
