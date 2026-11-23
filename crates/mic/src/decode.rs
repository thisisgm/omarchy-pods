//! The AAC low delay decoder behind its fence: a unit in, one frame of samples or a refusal out, and a panic caught.

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};

use rusty_aac::AacDecoder;

use super::{EXPECTED_CONFIG, SAMPLES_PER_UNIT};

pub struct Decoder {
    aac: AacDecoder,
    frame: Vec<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decoded<'a> {
    Samples(&'a [f32]),
    /// The decoder refused the unit, or gave other than one frame of samples.
    Error,
    Panic,
}

impl Decoder {
    /// None when the decoder refuses `EXPECTED_CONFIG`.
    #[must_use]
    pub fn new() -> Option<Self> {
        let aac = catch_unwind(|| AacDecoder::with_config_bytes(&EXPECTED_CONFIG));
        Some(Self {
            aac: aac.ok()?.ok()?,
            frame: Vec::new(),
        })
    }

    pub fn unit(&mut self, unit: &[u8]) -> Decoded<'_> {
        self.fenced(|aac| aac.decode(unit, None).ok().map(|audio| audio.samples))
    }

    /// One call into the decoder; a call that panicked leaves the decoder as the panic left it.
    pub(crate) fn fenced(
        &mut self,
        call: impl FnOnce(&mut AacDecoder) -> Option<Vec<f32>>,
    ) -> Decoded<'_> {
        let aac = &mut self.aac;
        match catch_unwind(AssertUnwindSafe(|| call(aac))) {
            Ok(Some(samples)) if samples.len() == usize::from(SAMPLES_PER_UNIT) => {
                self.frame = samples;
                Decoded::Samples(&self.frame)
            }
            Ok(_) => Decoded::Error,
            Err(_) => Decoded::Panic,
        }
    }
}

impl fmt::Debug for Decoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Decoder").finish_non_exhaustive()
    }
}
