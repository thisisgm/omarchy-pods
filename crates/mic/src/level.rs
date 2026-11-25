//! The level of what the reader hands out: one fixed gain, then a limiter that keeps nothing between samples, so sample n in is sample n out.

/// The fixed gain is provisional. It has been checked with a steady tone and still awaits measurement on speech.
pub const MIC_GAIN_DB: f32 = 14.0;
/// The limiter starts bending samples here. Speech peaks are expected to stay at or below it.
pub const LIMIT_KNEE_DBFS: f32 = -6.0;
/// Room for what a 16 kHz recorder's converter adds to the peak of a flattened wave, at most 2.1 dB.
pub const LIMIT_CEILING_DBFS: f32 = -3.0;
/// A level in dB is twenty times the decimal logarithm of an amplitude.
const DB_PER_DECADE: f32 = 20.0;

/// What the level met in one block.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    /// Samples whose magnitude after the gain was over the knee.
    pub limited: u32,
    /// Samples that were not finite and became zero.
    pub nonfinite: u32,
}

fn amplitude(db: f32) -> f32 {
    10_f32.powf(db / DB_PER_DECADE)
}

/// The gain, then the limiter, in place: a magnitude over the knee leaves it at slope one and bends under the ceiling.
pub fn apply(block: &mut [f32]) -> Tally {
    // Worked out once a block, so no sample costs a call into a maths library.
    let gain = amplitude(MIC_GAIN_DB);
    let knee = amplitude(LIMIT_KNEE_DBFS);
    let ceiling = amplitude(LIMIT_CEILING_DBFS);
    let room = ceiling - knee;
    let highest = ceiling.next_down();
    let mut tally = Tally::default();
    for sample in block {
        if !sample.is_finite() {
            *sample = 0.0;
            tally.nonfinite = tally.nonfinite.saturating_add(1);
            continue;
        }
        let raised = *sample * gain;
        let magnitude = raised.abs();
        *sample = if magnitude > knee {
            tally.limited = tally.limited.saturating_add(1);
            let over = (magnitude - knee) / room;
            // The same curve as the knee plus the room times over / (1 + over), in the form a product past the range of a float keeps finite.
            (ceiling - room / (1.0 + over))
                .min(highest)
                .copysign(raised)
        } else {
            raised
        };
    }
    tally
}

#[cfg(test)]
mod tests;
