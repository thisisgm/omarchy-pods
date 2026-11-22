//! A Goertzel bank on a fixed grid: the mean power of a window every 250 Hz, and the three questions asked of it.

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
/// No band edge is looked for under this grid point, where one step of the grid is so much of an octave that the tilt of a voice falls as far as an edge does.
const EDGE_FROM_HZ: u32 = 1000;
/// The least fall from one grid point to the next that ends a band; a tilt of 12 dB an octave falls under 4 dB over any step from 1 kHz up.
const EDGE_FALL_DB: f64 = 12.0;
/// The fewest blocks an edge is read from: the mean power of noise over fewer falls that far between two points by chance.
const EDGE_MIN_BLOCKS: usize = 8;

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

/// Why a window reaches nowhere over the quiet stretch it is set against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum NoReach {
    #[error(
        "this window or the quiet stretch it is set against holds no whole {BLOCK_MS} ms block"
    )]
    NoBlock,
    #[error(
        "the quiet stretch it is set against holds nothing at {hz} Hz to lie over, as digital silence does"
    )]
    NothingUnder { hz: u32 },
    #[error(
        "the window lies {EXCEED_DB} dB over the quiet stretch it is set against at no grid point"
    )]
    Nowhere,
}

/// Why a stretch has no frequency where its band ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum NoEdge {
    #[error(
        "it holds {blocks} whole {BLOCK_MS} ms blocks, and an edge is read from {EDGE_MIN_BLOCKS} or more"
    )]
    Thin { blocks: usize },
    #[error("it holds nothing from {EDGE_FROM_HZ} Hz up")]
    Nothing,
    #[error(
        "from {EDGE_FROM_HZ} Hz up no grid point lies {EDGE_FALL_DB} dB over all that is found over it: its band reaches half the rate, or fades with no edge"
    )]
    NoFall,
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

fn block_len(rate: u32) -> usize {
    samples_in(BLOCK_MS, rate).max(1)
}

/// The mean power of `stretches` at each grid frequency, lowest first, over the whole Hann windowed blocks of each, so no block spans two; stretches shorter than one block have none.
pub(crate) fn grid(stretches: &[&[f32]], rate: u32) -> Vec<(u32, f64)> {
    let block_len = block_len(rate);
    let hann: Vec<f64> = (0..block_len)
        .map(|index| 0.5 - 0.5 * (TAU * count(index) / count(block_len)).cos())
        .collect();
    let steps = (1_u32..).map(|step| step.saturating_mul(GRID_STEP_HZ));
    let mut grid: Vec<(u32, f64)> = steps
        .take_while(|&hz| hz <= rate / 2)
        .map(|hz| (hz, 0.0))
        .collect();
    let mut windowed = vec![0.0_f64; block_len];
    let mut blocks = 0_usize;
    let whole = stretches
        .iter()
        .flat_map(|stretch| stretch.chunks_exact(block_len));
    for block in whole {
        for ((weighted, &sample), weight) in windowed.iter_mut().zip(block).zip(&hann) {
            *weighted = f64::from(sample) * weight;
        }
        for (hz, power) in &mut grid {
            *power += goertzel(&windowed, *hz, rate);
        }
        blocks = blocks.saturating_add(1);
    }
    if blocks == 0 {
        return Vec::new();
    }
    for (_, power) in &mut grid {
        *power /= count(blocks);
    }
    grid
}

/// What a band edge says of itself.
pub(crate) fn edge_note() -> String {
    format!(
        "where the band of the stretch ends: its mean spectrum over every {BLOCK_MS} ms block, on a {GRID_STEP_HZ} Hz grid from {EDGE_FROM_HZ} Hz up, each point taken as the highest level at or over it; the lowest point that lies {EDGE_FALL_DB} dB or more over the next"
    )
}

/// The lowest grid frequency whose power lies an edge's fall over the highest power found over it.
fn edge_of(grid: &[(u32, f64)]) -> Result<u32, NoEdge> {
    let from = grid.partition_point(|&(hz, _)| hz < EDGE_FROM_HZ);
    let mut downwards = grid.get(from..).unwrap_or(&[]).iter().rev();
    let mut over = downwards.next().map_or(0.0, |&(_, power)| power);
    let mut edge = None;
    for &(hz, power) in downwards {
        if power > 0.0 && power >= over * ratio(EDGE_FALL_DB) {
            edge = Some(hz);
        }
        over = over.max(power);
    }
    if over <= 0.0 {
        return Err(NoEdge::Nothing);
    }
    edge.ok_or(NoEdge::NoFall)
}

/// Where the band of `stretch` ends; refused where it holds too few blocks, nothing at all, or no fall that is an edge.
pub(crate) fn band_edge(stretch: &[f32], rate: u32) -> Result<u32, NoEdge> {
    let blocks = stretch.len().checked_div(block_len(rate)).unwrap_or(0);
    if blocks < EDGE_MIN_BLOCKS {
        return Err(NoEdge::Thin { blocks });
    }
    edge_of(&grid(&[stretch], rate))
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

/// What a reach of `loud` over `quiet` says of itself.
pub(crate) fn reach_note(loud: &str, quiet: &str) -> String {
    format!(
        "the highest point of a {GRID_STEP_HZ} Hz grid where {loud} lies {EXCEED_DB} dB over {quiet}"
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

#[cfg(test)]
mod tests;
