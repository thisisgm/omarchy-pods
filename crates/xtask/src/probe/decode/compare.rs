//! Two decodes of one stream compared sample by sample, at the offset where they agree best.

/// A float sample times this is its value in steps of a 16 bit sample.
pub(crate) const LSB_PER_FULL_SCALE: f64 = 32768.0;
/// A frame differs when one of its samples is more than this many steps apart.
pub(crate) const TOLERANCE_LSB: f64 = 1.0;
/// Two frames agree up to this share of their peak where that is more than `TOLERANCE_LSB`: a difference 60 dB under the peak, set with a margin over what two decoders of generated streams were measured apart and not proven for every stream.
pub(crate) const AGREEMENT_OF_PEAK: f64 = 1e-3;
/// The offsets tried, in units, nearest first: a later one wins only by a smaller largest difference.
const OFFSETS: [i32; 7] = [0, -1, 1, -2, 2, -3, 3];
/// The most units two decodes may differ in length, which is also the furthest offset tried.
pub(crate) const SEARCH_UNITS: usize = 3;
/// The fewest units a decode may hold: one more than the furthest offset reaches at both ends, so a frame is left to judge it on.
const FEWEST_UNITS: usize = 2 * SEARCH_UNITS + 1;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Comparison {
    /// The units the first decode is behind the second, where the two agree best; none when no frame could judge the offsets, and the decodes are then compared level.
    pub(crate) offset_units: Option<i32>,
    /// The whole frames compared at that offset.
    pub(crate) frames: usize,
    /// The largest difference of two samples, in steps of a 16 bit sample; infinite when a sample is not a number.
    pub(crate) max_lsb: f64,
    pub(crate) frames_over_one_lsb: usize,
    /// The largest difference of a frame as a share of the peak of that frame in either decode; infinite when a sample is not a number.
    pub(crate) max_of_peak: f64,
    /// Frames further apart than the larger of `TOLERANCE_LSB` and `AGREEMENT_OF_PEAK` of their peak.
    pub(crate) frames_over_gate: usize,
}

/// One frame of each decode laid on the other: their largest difference and the largest sample of either, in steps of a 16 bit sample.
#[derive(Debug, Clone, Copy)]
struct Apart {
    lsb: f64,
    peak_lsb: f64,
}

impl Apart {
    fn of(a: &[f32], b: &[f32]) -> Self {
        let levels = a.iter().chain(b).map(|&sample| lsb_apart(sample, 0.0));
        // A sample that is not a number stays out of the peak, so its frame is infinitely far apart by either measure.
        let finite = levels.filter(|level| level.is_finite());
        Self {
            lsb: frame_apart(a, b),
            peak_lsb: finite.fold(0.0, f64::max),
        }
    }

    /// The difference as a share of the peak; not a number for two frames of silence, which a largest share passes over.
    fn of_peak(self) -> f64 {
        self.lsb / self.peak_lsb
    }

    fn over_gate(self) -> bool {
        self.lsb > TOLERANCE_LSB.max(AGREEMENT_OF_PEAK * self.peak_lsb)
    }
}

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

fn frame(samples: &[f32], index: usize, samples_per_unit: usize) -> Option<&[f32]> {
    let from = index.checked_mul(samples_per_unit)?;
    samples.get(from..from.checked_add(samples_per_unit)?)
}

/// Frame `index` of `b` laid on the frame of `a` that is `offset` units later, where `a` holds one.
fn apart_at(
    a: &[f32],
    b: &[f32],
    offset: i32,
    index: usize,
    samples_per_unit: usize,
) -> Option<Apart> {
    let in_a = index.checked_add_signed(isize::try_from(offset).ok()?)?;
    let a = frame(a, in_a, samples_per_unit)?;
    Some(Apart::of(a, frame(b, index, samples_per_unit)?))
}

/// The offset whose largest difference, one of `largest` in the order of `OFFSETS`, is the smallest: the nearest of those that tie.
fn nearest_of_the_closest(largest: [f64; OFFSETS.len()]) -> i32 {
    let mut best = (0, f64::INFINITY);
    for (offset, most) in OFFSETS.into_iter().zip(largest) {
        if most < best.1 {
            best = (offset, most);
        }
    }
    best.0
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
    let mut largest = None;
    for index in judged {
        let at = OFFSETS.map(|offset| {
            let apart = apart_at(a, b, offset, index, samples_per_unit);
            apart.map_or(f64::INFINITY, |frame| frame.lsb)
        });
        // A frame that meets a sample that is not a number at one offset is left out at every offset, so none wins by comparing fewer frames.
        if at.iter().all(|lsb| lsb.is_finite()) {
            let largest = largest.get_or_insert([0.0_f64; OFFSETS.len()]);
            for (most, lsb) in largest.iter_mut().zip(at) {
                *most = most.max(lsb);
            }
        }
    }
    let offset_units = largest.map(nearest_of_the_closest);
    let laid_at = offset_units.unwrap_or(0);
    // A frame of `b` that `a` holds no frame for at that offset is left out.
    let laid = (0..b_units).filter_map(|index| apart_at(a, b, laid_at, index, samples_per_unit));
    let apart: Vec<Apart> = laid.collect();
    let over_one_lsb = apart.iter().filter(|frame| frame.lsb > TOLERANCE_LSB);
    let shares = apart.iter().map(|frame| frame.of_peak());
    Ok(Comparison {
        offset_units,
        frames: apart.len(),
        max_lsb: apart.iter().map(|frame| frame.lsb).fold(0.0, f64::max),
        frames_over_one_lsb: over_one_lsb.count(),
        max_of_peak: shares.fold(0.0, f64::max),
        frames_over_gate: apart.iter().filter(|frame| frame.over_gate()).count(),
    })
}

#[cfg(test)]
pub(crate) mod tests;
