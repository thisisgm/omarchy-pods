//! Two decodes of one stream compared sample by sample, at the offset where they agree best.

use std::ops::Range;

/// A float sample times this is its value in steps of a 16 bit sample.
pub(crate) const LSB_PER_FULL_SCALE: f64 = 32768.0;
/// A frame differs when one of its samples is more than this many steps apart.
pub(crate) const TOLERANCE_LSB: f64 = 1.0;
/// The offsets tried, in units, nearest first: a later one wins only by a smaller largest difference.
const OFFSETS: [i32; 7] = [0, -1, 1, -2, 2, -3, 3];
/// The most units two decodes may differ in length, which is also the furthest offset tried.
pub(crate) const SEARCH_UNITS: usize = 3;
/// The fewest units a decode may hold: one more than the furthest offset reaches at both ends, so a frame is left to judge it on.
const FEWEST_UNITS: usize = 2 * SEARCH_UNITS + 1;

/// How far apart two decodes of one stream are.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Comparison {
    /// The units the first decode is behind the second, where the two agree best.
    pub(crate) offset_units: i32,
    /// The whole frames compared at that offset.
    pub(crate) frames: usize,
    /// The largest difference of two samples, in steps of a 16 bit sample; infinite when a sample is not a number.
    pub(crate) max_lsb: f64,
    pub(crate) frames_over_one_lsb: usize,
}

/// Why two decodes were not compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CompareError {
    #[error("{samples} samples are not a whole number of units of {samples_per_unit}")]
    NotWholeUnits {
        samples: usize,
        samples_per_unit: usize,
    },
    #[error("{a} units against {b}: more than {SEARCH_UNITS} apart, which no offset explains")]
    TooFarApart { a: usize, b: usize },
    #[error(
        "{a} units against {b}: fewer than {FEWEST_UNITS} leave no frame to judge an offset on"
    )]
    TooShort { a: usize, b: usize },
}

/// The difference of two samples in steps of a 16 bit sample; a sample that is not a number is infinitely far from any other.
pub(crate) fn lsb_apart(a: f32, b: f32) -> f64 {
    let apart = (f64::from(a) - f64::from(b)).abs() * LSB_PER_FULL_SCALE;
    if apart.is_finite() {
        apart
    } else {
        f64::INFINITY
    }
}

/// The largest difference in a frame of each decode.
pub(crate) fn frame_apart(a: &[f32], b: &[f32]) -> f64 {
    let apart = a.iter().zip(b).map(|(&a, &b)| lsb_apart(a, b));
    apart.fold(0.0, f64::max)
}

fn units_in(samples: &[f32], samples_per_unit: usize) -> Result<usize, CompareError> {
    let not_whole = CompareError::NotWholeUnits {
        samples: samples.len(),
        samples_per_unit,
    };
    if samples.len().checked_rem(samples_per_unit) != Some(0) {
        return Err(not_whole);
    }
    samples.len().checked_div(samples_per_unit).ok_or(not_whole)
}

/// Frame `index` of a decode.
fn frame(samples: &[f32], index: usize, samples_per_unit: usize) -> Option<&[f32]> {
    let from = index.checked_mul(samples_per_unit)?;
    samples.get(from..from.checked_add(samples_per_unit)?)
}

/// The largest difference in each of `frames` of `b` and the frame of `a` that is `offset` units later; a frame `a` does not hold is left out.
fn frames_apart<'a>(
    a: &'a [f32],
    b: &'a [f32],
    offset: i32,
    frames: Range<usize>,
    samples_per_unit: usize,
) -> impl Iterator<Item = f64> + 'a {
    frames.filter_map(move |index| {
        let in_a = index.checked_add_signed(isize::try_from(offset).ok()?)?;
        let a = frame(a, in_a, samples_per_unit)?;
        Some(frame_apart(a, frame(b, index, samples_per_unit)?))
    })
}

/// Compares the frames of `a` and `b` at the offset of up to three units where they agree best.
pub(crate) fn compare(
    a: &[f32],
    b: &[f32],
    samples_per_unit: usize,
) -> Result<Comparison, CompareError> {
    let a_units = units_in(a, samples_per_unit)?;
    let b_units = units_in(b, samples_per_unit)?;
    if a_units.abs_diff(b_units) > SEARCH_UNITS {
        return Err(CompareError::TooFarApart {
            a: a_units,
            b: b_units,
        });
    }
    let shorter = a_units.min(b_units);
    if shorter < FEWEST_UNITS {
        return Err(CompareError::TooShort {
            a: a_units,
            b: b_units,
        });
    }
    // The offsets are judged on the frames every one of them can compare: an offset must not win by leaving a frame that differs outside what it compares.
    let judged = SEARCH_UNITS..shorter.saturating_sub(SEARCH_UNITS);
    let mut best = (0, f64::INFINITY);
    for offset in OFFSETS {
        let apart = frames_apart(a, b, offset, judged.clone(), samples_per_unit);
        let largest = apart.fold(0.0, f64::max);
        if largest < best.1 {
            best = (offset, largest);
        }
    }
    let (offset_units, _) = best;
    let apart: Vec<f64> = frames_apart(a, b, offset_units, 0..b_units, samples_per_unit).collect();
    Ok(Comparison {
        offset_units,
        frames: apart.len(),
        max_lsb: apart.iter().copied().fold(0.0, f64::max),
        frames_over_one_lsb: apart.iter().filter(|&&lsb| lsb > TOLERANCE_LSB).count(),
    })
}

#[cfg(test)]
pub(crate) mod tests;
