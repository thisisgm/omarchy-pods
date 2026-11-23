//! What hides an edge: a raised cosine that falls or rises, and audio that ended played on backwards under the fall.

use std::f32::consts::PI;

use super::FRAMES_PER_MS;
use super::ring::Ring;

/// Long enough to hide an edge, short enough to keep a syllable.
const FADE_MS: u16 = 5;
pub(crate) const FADE_FRAMES: u16 = FADE_MS * FRAMES_PER_MS;

/// One at `at` 0, falling to zero at `over`; the caller keeps `at` under `over`.
fn fall(at: u16, over: u16) -> f32 {
    f32::midpoint(1.0, (PI * f32::from(at) / f32::from(over)).cos())
}

/// The gain of the sample `at` samples into a fade in; one from the end of the fade on.
pub(crate) fn fade_in(at: u16) -> f32 {
    if at >= FADE_FRAMES {
        return 1.0;
    }
    1.0 - fall(at, FADE_FRAMES)
}

/// Audio that ended at frame `last`, played on from there backwards under the fall, so its end is no step.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Tail {
    last: i64,
    at: u16,
    over: u16,
    level: f32,
    run: u64,
}

impl Tail {
    pub(crate) const NONE: Self = Self::of(0, 0, 0.0, 0);

    /// `level` is the gain the audio stood at when it ended, and `run` the floor of the run it is of.
    pub(crate) const fn of(last: i64, over: u16, level: f32, run: u64) -> Self {
        Self {
            last,
            at: 0,
            over,
            level,
            run,
        }
    }

    pub(crate) const fn falling(&self) -> bool {
        self.at < self.over
    }

    /// None once the tail has fallen to zero.
    pub(crate) fn next(&mut self, ring: &Ring) -> Option<f32> {
        if !self.falling() {
            return None;
        }
        let frame = self.last.saturating_sub(i64::from(self.at));
        let sample = ring.sample(self.run, frame);
        let faded = sample * fall(self.at, self.over) * self.level;
        self.at = self.at.saturating_add(1);
        Some(faded)
    }
}
