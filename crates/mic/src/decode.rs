//! The AAC low delay decoder behind its fence: a unit in, one frame of samples or a refusal out, and a panic caught.

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};

pub use pods_aac::BAND_EDGES;
use pods_aac::Decoder as AacDecoder;

use super::{EXPECTED_CONFIG, SAMPLES_PER_UNIT};

// The fence hands on `last_frame` whole, so both crates must count one frame alike.
#[expect(clippy::as_conversions, reason = "a const context has no `From`")]
const _: () = assert!(pods_aac::SAMPLES_PER_UNIT == SAMPLES_PER_UNIT as usize);

pub struct Decoder {
    aac: AacDecoder,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decoded<'a> {
    Samples(&'a [f32]),
    /// The decoder refused what it was given.
    Error,
    Panic,
}

impl Decoder {
    /// None when the decoder refuses `EXPECTED_CONFIG`.
    #[must_use]
    pub fn new() -> Option<Self> {
        let aac = catch_unwind(|| AacDecoder::with_config(&EXPECTED_CONFIG));
        Some(Self {
            aac: aac.ok()?.ok()?,
        })
    }

    pub fn unit(&mut self, unit: &[u8]) -> Decoded<'_> {
        self.fenced(|aac| aac.decode(unit).map(|_| ()))
    }

    /// One call into the decoder, whose frame is then `last_frame`; a call that panicked leaves the decoder as the panic left it.
    pub(crate) fn fenced(
        &mut self,
        call: impl FnOnce(&mut AacDecoder) -> Result<(), pods_aac::Error>,
    ) -> Decoded<'_> {
        let aac = &mut self.aac;
        match catch_unwind(AssertUnwindSafe(|| call(aac))) {
            Ok(Ok(())) => Decoded::Samples(self.aac.last_frame()),
            Ok(Err(_)) => Decoded::Error,
            Err(_) => Decoded::Panic,
        }
    }
}

impl fmt::Debug for Decoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Decoder").finish_non_exhaustive()
    }
}
