//! The generated audio a fake `DoAP` microphone hears.

use std::time::Instant;

use pods_samples::opus;

use super::{Doap, Heard, Machine};

impl Doap {
    /// The next `frames` frames the microphone hears are silent, before any queued mark.
    pub fn silent(&self, frames: u32) {
        self.shared.doap(|doap| {
            doap.hear(Instant::now());
            doap.silent = frames;
        });
    }

    /// Between queued silence and marks, hears an independently coded quiet tone.
    pub fn quiet(&self, frames: u32) {
        self.shared.doap(|doap| {
            doap.hear(Instant::now());
            doap.quiet = frames;
        });
    }

    /// The next `frames` frames the microphone hears hold the second tone, streaming or not.
    pub fn mark(&self, frames: u32) {
        self.shared.doap(|doap| {
            doap.hear(Instant::now());
            doap.marked = frames;
        });
    }
}

impl Machine {
    /// The bytes of the fixture the microphone coded `heard` to.
    pub(super) fn coded(&self, heard: Heard) -> &'static [u8] {
        if heard.silent {
            return opus::silence();
        }
        if heard.quiet {
            return opus::quiet();
        }
        if heard.marked {
            return opus::mark_onset();
        }
        let fixture = &self.tone;
        let place = usize::try_from(heard.place).unwrap_or(usize::MAX);
        let at = place.checked_rem(fixture.len()).unwrap_or(0);
        fixture.get(at).copied().unwrap_or_default()
    }
}
