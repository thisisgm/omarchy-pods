//! The level of what the reader hands out: one fixed gain, then a limiter that keeps nothing between samples, so sample n in is sample n out.

/// The fixed gain is provisional. It has been checked with a steady tone and still awaits measurement on speech.
pub const MIC_GAIN_DB: f32 = 14.0;
/// The limiter starts bending samples here. Speech peaks are expected to stay at or below it.
pub const LIMIT_KNEE_DBFS: f32 = -6.0;
/// Room for what a 16 kHz recorder's converter adds to the peak of a flattened wave, at most 2.1 dB.
pub const LIMIT_CEILING_DBFS: f32 = -3.0;
/// Precomputed linear gain for [`MIC_GAIN_DB`] (`10^(14 / 20)`).
const MIC_GAIN: f32 = 5.011_872_3;
/// Precomputed linear knee for [`LIMIT_KNEE_DBFS`] (`10^(-6 / 20)`).
const LIMIT_KNEE: f32 = 0.501_187_2;
/// Precomputed linear ceiling for [`LIMIT_CEILING_DBFS`] (`10^(-3 / 20)`).
const LIMIT_CEILING: f32 = 0.707_945_76;
/// Linear span between the knee and the ceiling.
const LIMIT_ROOM: f32 = LIMIT_CEILING - LIMIT_KNEE;
/// Largest float strictly under [`LIMIT_CEILING`].
const LIMIT_HIGHEST: f32 = LIMIT_CEILING.next_down();

/// What the level met in one block.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    /// Samples whose magnitude after the gain was over the knee.
    pub limited: u32,
    /// Samples that were not finite and became zero.
    pub nonfinite: u32,
}

/// The gain, then the limiter, in place: a magnitude over the knee leaves it at slope one and bends under the ceiling.
pub fn apply(block: &mut [f32]) -> Tally {
    let mut tally = Tally::default();
    for sample in block {
        if !sample.is_finite() {
            *sample = 0.0;
            tally.nonfinite = tally.nonfinite.saturating_add(1);
            continue;
        }
        let raised = *sample * MIC_GAIN;
        let magnitude = raised.abs();
        *sample = if magnitude > LIMIT_KNEE {
            tally.limited = tally.limited.saturating_add(1);
            let over = (magnitude - LIMIT_KNEE) / LIMIT_ROOM;
            // The same curve as the knee plus the room times over / (1 + over), in the form a product past the range of a float keeps finite.
            (LIMIT_CEILING - LIMIT_ROOM / (1.0 + over))
                .min(LIMIT_HIGHEST)
                .copysign(raised)
        } else {
            raised
        };
    }
    tally
}

#[cfg(test)]
mod tests;
