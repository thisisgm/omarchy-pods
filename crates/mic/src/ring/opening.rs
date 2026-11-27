use super::{
    FRAMES_PER_MS, OPENING_EXIT_MS, OPENING_MAX_PPM, OPENING_RAMP_QUANTA, PARTS_PER_MILLION,
    float_of,
};

/// The catch-up rate falls by a quarter of its excess per quantum as it spends its remaining lag budget.
pub(super) fn opening_ratio(behind: i64, frames: i64) -> f64 {
    let exit = i64::from(OPENING_EXIT_MS.saturating_mul(FRAMES_PER_MS));
    let distance = float_of(behind.saturating_sub(exit)) / float_of(frames.max(1));
    if distance <= 0.0 {
        return 1.0;
    }
    let max_excess = OPENING_MAX_PPM / PARTS_PER_MILLION;
    let step = max_excess / f64::from(OPENING_RAMP_QUANTA);
    let mut budget = 0.0;
    for quantum in 1..=OPENING_RAMP_QUANTA {
        let excess = step * f64::from(quantum);
        // A quantum at ratio r closes 1 - 1/r quanta of lag.
        let closure = excess / (1.0 + excess);
        if distance <= budget + closure {
            let share = (distance - budget) / closure;
            return 1.0 + step * (f64::from(quantum.saturating_sub(1)) + share);
        }
        budget += closure;
    }
    1.0 + max_excess
}
