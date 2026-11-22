//! A Goertzel bank on a fixed grid: the mean power of a window every 250 Hz, and the two questions asked of it.

use std::f64::consts::TAU;

use super::super::analyse::stats::count;
use super::levels::samples_in;

/// The grid: one frequency every 250 Hz, from 250 Hz to half the rate.
const GRID_STEP_HZ: u32 = 250;
/// The blocks the power is averaged over: at 16 ms a Hann window keeps each grid point clear of its neighbours.
const BLOCK_MS: u32 = 16;
/// The grid point every other is compared with.
const REFERENCE_HZ: u32 = 1000;
/// A window has fallen away where it lies this far under its own 1 kHz point.
const FALL_DB: f64 = 20.0;
/// One window reaches as far as it lies this much over another.
const EXCEED_DB: f64 = 6.0;

/// Why a window has no frequency where it falls away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum NoCutoff {
    #[error("it holds no whole {BLOCK_MS} ms block")]
    NoBlock,
    #[error("it holds nothing at {REFERENCE_HZ} Hz to compare with")]
    NoReference,
    #[error("it is within {FALL_DB} dB of its {REFERENCE_HZ} Hz point up to half the rate")]
    ReachesTheTop,
}

/// Why a window reaches nowhere over the silent one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum NoReach {
    #[error("this window or the silent one holds no whole {BLOCK_MS} ms block")]
    NoBlock,
    #[error("the silent window holds nothing at {hz} Hz to lie over, as digital silence does")]
    NothingUnder { hz: u32 },
    #[error("the window lies {EXCEED_DB} dB over the silent one at no grid point")]
    Nowhere,
}

fn ratio(db: f64) -> f64 {
    10.0_f64.powf(db / 10.0)
}

/// The power of one block at `hz`, by Goertzel's recurrence.
fn goertzel(block: &[f64], hz: u32, rate: u32) -> f64 {
    let coefficient = 2.0 * (TAU * f64::from(hz) / f64::from(rate)).cos();
    let (mut last, mut before) = (0.0_f64, 0.0_f64);
    for &sample in block {
        let now = sample + coefficient * last - before;
        before = last;
        last = now;
    }
    last * last + before * before - coefficient * last * before
}

/// The mean power of `samples` at each grid frequency, lowest first, over whole Hann windowed blocks; a window shorter than one block has none.
pub(crate) fn grid(samples: &[f32], rate: u32) -> Vec<(u32, f64)> {
    let block_len = samples_in(BLOCK_MS, rate).max(1);
    let hann: Vec<f64> = (0..block_len)
        .map(|index| 0.5 - 0.5 * (TAU * count(index) / count(block_len)).cos())
        .collect();
    let windowed: Vec<f64> = samples
        .chunks_exact(block_len)
        .flat_map(|block| block.iter().zip(&hann))
        .map(|(&sample, weight)| f64::from(sample) * weight)
        .collect();
    let blocks = windowed.chunks_exact(block_len);
    if blocks.len() == 0 {
        return Vec::new();
    }
    let mut grid = Vec::new();
    let mut hz = GRID_STEP_HZ;
    while hz <= rate / 2 {
        let power: f64 = blocks.clone().map(|block| goertzel(block, hz, rate)).sum();
        grid.push((hz, power / count(blocks.len())));
        hz = hz.saturating_add(GRID_STEP_HZ);
    }
    grid
}

/// What a cutoff says of itself.
pub(crate) fn cutoff_note() -> String {
    format!(
        "the lowest point of a {GRID_STEP_HZ} Hz grid from which every point lies {FALL_DB} dB or more under the {REFERENCE_HZ} Hz point"
    )
}

/// The lowest grid frequency from which every point lies 20 dB or more under the 1 kHz point.
pub(crate) fn cutoff(grid: &[(u32, f64)]) -> Result<u32, NoCutoff> {
    let at_reference = grid.iter().find(|&&(hz, _)| hz == REFERENCE_HZ);
    let &(_, reference) = at_reference.ok_or(NoCutoff::NoBlock)?;
    if reference <= 0.0 {
        return Err(NoCutoff::NoReference);
    }
    let fallen = reference / ratio(FALL_DB);
    let above = grid.iter().rev().take_while(|&&(_, power)| power <= fallen);
    above
        .last()
        .map(|&(hz, _)| hz)
        .ok_or(NoCutoff::ReachesTheTop)
}

/// What a reach says of itself.
pub(crate) fn reach_note() -> String {
    format!(
        "the highest point of a {GRID_STEP_HZ} Hz grid where the window lies {EXCEED_DB} dB over the silent window"
    )
}

/// The highest grid frequency where `loud` lies 6 dB over `quiet`; refused where a grid is empty, the two differ, or `quiet` holds a point of no power.
pub(crate) fn reach(loud: &[(u32, f64)], quiet: &[(u32, f64)]) -> Result<u32, NoReach> {
    if loud.is_empty() || loud.len() != quiet.len() {
        return Err(NoReach::NoBlock);
    }
    if let Some(&(hz, _)) = quiet.iter().find(|&&(_, power)| power <= 0.0) {
        return Err(NoReach::NothingUnder { hz });
    }
    let pairs = loud.iter().zip(quiet).rev();
    let mut over = pairs.filter(|&(&(_, loud), &(_, quiet))| loud > quiet * ratio(EXCEED_DB));
    over.next().map(|(&(hz, _), _)| hz).ok_or(NoReach::Nowhere)
}
