//! The arithmetic every figure goes through: conversions, rates, a mode, a median and a least squares slope.

use std::collections::BTreeMap;

pub(crate) const NS_PER_SECOND: f64 = 1e9;
pub(crate) const NS_PER_MS: f64 = 1e6;
const PARTS_PER_MILLION: f64 = 1e6;
/// Two to the power 32, the weight of the high half of a `u64`.
const HIGH_HALF_WEIGHT: f64 = 4_294_967_296.0;
/// The two sided 95 percent point of the normal distribution.
const NORMAL_95: f64 = 1.96;
/// A fit needs this many points: from here the 95 percent point of the t distribution is within 0.3 percent of the normal one.
const MIN_FIT_POINTS: usize = 500;
/// The points a line uses up, so the residuals have this many fewer degrees of freedom.
const FIT_PARAMETERS: f64 = 2.0;

/// A `u64` as an `f64`: exact below two to the power 53, rounded once, in the one addition, above it.
pub(crate) fn to_f64(value: u64) -> f64 {
    let [b0, b1, b2, b3, b4, b5, b6, b7] = value.to_le_bytes();
    let low = u32::from_le_bytes([b0, b1, b2, b3]);
    let high = u32::from_le_bytes([b4, b5, b6, b7]);
    f64::from(high) * HIGH_HALF_WEIGHT + f64::from(low)
}

pub(crate) fn count(len: usize) -> f64 {
    // No Rust target has a usize wider than 64 bits, so the fallback is never taken.
    to_f64(u64::try_from(len).unwrap_or(u64::MAX))
}

pub(crate) fn seconds(ns: u64) -> f64 {
    to_f64(ns) / NS_PER_SECOND
}

pub(crate) fn millis(ns: u64) -> f64 {
    to_f64(ns) / NS_PER_MS
}

/// `count` over `span_ns` as a rate a second; a span of zero supports no rate.
pub(crate) fn per_second(count: f64, span_ns: u64) -> Option<f64> {
    (span_ns > 0).then(|| count / seconds(span_ns))
}

/// How far `measured` lies from `reference`, in parts per million of the reference, signed.
pub(crate) fn ppm(measured: f64, reference: f64) -> f64 {
    (measured - reference) / reference * PARTS_PER_MILLION
}

/// `part` of `whole` as a fraction; nothing has no share.
pub(crate) fn share(part: usize, whole: usize) -> Option<f64> {
    (whole > 0).then(|| count(part) / count(whole))
}

/// The most common value and how often it occurs; of several equally common, the smallest.
pub(crate) fn mode<T: Ord>(values: impl Iterator<Item = T>) -> Option<(T, usize)> {
    let mut counts: BTreeMap<T, usize> = BTreeMap::new();
    for value in values {
        let seen = counts.entry(value).or_insert(0);
        *seen = seen.saturating_add(1);
    }
    let mut best: Option<(T, usize)> = None;
    for (value, seen) in counts {
        if best.as_ref().is_none_or(|(_, most)| seen > *most) {
            best = Some((value, seen));
        }
    }
    best
}

/// The step from each value to the next, counted the way a 32 bit counter wraps.
pub(crate) fn wrapping_deltas(values: &[u32]) -> Vec<u32> {
    values
        .iter()
        .zip(values.iter().skip(1))
        .map(|(before, after)| after.wrapping_sub(*before))
        .collect()
}

/// The middle of a sorted list, the lower of the two middles when the count is even.
pub(crate) fn median<T: Copy>(sorted: &[T]) -> Option<T> {
    sorted.get(sorted.len().saturating_sub(1) / 2).copied()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Fit {
    pub(crate) slope: f64,
    /// Half the width of the 95 percent interval of the slope, from the residuals.
    pub(crate) half_width: f64,
}

/// The least squares slope of y against x; no line for under `MIN_FIT_POINTS` points, whose interval would be too narrow, or for points with one x.
pub(crate) fn least_squares(points: &[(f64, f64)]) -> Option<Fit> {
    if points.len() < MIN_FIT_POINTS {
        return None;
    }
    let total = count(points.len());
    let mean_x = points.iter().map(|&(x, _)| x).sum::<f64>() / total;
    let mean_y = points.iter().map(|&(_, y)| y).sum::<f64>() / total;
    let mut spread = 0.0;
    let mut cross = 0.0;
    for &(x, y) in points {
        spread += (x - mean_x) * (x - mean_x);
        cross += (x - mean_x) * (y - mean_y);
    }
    // Zero is no spread; not a number or infinite is an x that was neither.
    if !spread.is_normal() {
        return None;
    }
    let slope = cross / spread;
    let mut residual_squares = 0.0;
    for &(x, y) in points {
        let residual = (y - mean_y) - slope * (x - mean_x);
        residual_squares += residual * residual;
    }
    let variance = residual_squares / (total - FIT_PARAMETERS);
    Some(Fit {
        slope,
        half_width: NORMAL_95 * (variance / spread).sqrt(),
    })
}

#[cfg(test)]
mod tests;
