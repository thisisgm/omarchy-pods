//! The Opus decoder behind its fence: a frame in, 20 ms of samples or a refusal out, and a panic caught.

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};

use super::decode::Decoded;

/// The rate Opus decodes at with no converter of its own in the way.
pub const DICTATION_DECODE_HZ: u32 = pods_opus::OutputRate::Hz48000.hz();
/// The samples a 20 ms frame decodes to at `DICTATION_DECODE_HZ`.
pub const FRAME_SAMPLES: usize = pods_opus::SAMPLES_48K;
/// The longest frame the receiver the stream was made for takes.
pub const MAX_OPUS_FRAME_BYTES: usize = pods_opus::MAX_PACKET_BYTES;

pub struct Decoder {
    opus: pods_opus::Decoder,
    frame: [f32; FRAME_SAMPLES],
}

impl Decoder {
    /// Opens a mono decoder at `DICTATION_DECODE_HZ`.
    #[must_use]
    pub fn new() -> Option<Self> {
        let opus = catch_unwind(pods_opus::Decoder::new_48k).ok()?;
        Some(Self {
            opus,
            frame: [0.0; FRAME_SAMPLES],
        })
    }

    /// A frame of no bytes or of more than `MAX_OPUS_FRAME_BYTES` is refused here and never reaches the decoder.
    pub fn frame(&mut self, data: &[u8]) -> Decoded<'_> {
        if data.is_empty() || data.len() > MAX_OPUS_FRAME_BYTES {
            return Decoded::Error;
        }
        self.fenced(|opus, pcm| {
            let decoded = opus.decode(data).ok()?;
            let dst = pcm.get_mut(..decoded.len())?;
            dst.copy_from_slice(decoded);
            Some(decoded.len())
        })
    }

    /// The decoder's own concealment for one frame that never came.
    pub fn conceal(&mut self) -> Decoded<'_> {
        self.fenced(|opus, pcm| {
            let concealed = opus.conceal();
            let dst = pcm.get_mut(..concealed.len())?;
            dst.copy_from_slice(concealed);
            Some(concealed.len())
        })
    }

    /// One call into the decoder, which gives the samples it wrote; a call that panicked leaves the decoder as the panic left it.
    pub(crate) fn fenced(
        &mut self,
        call: impl FnOnce(&mut pods_opus::Decoder, &mut [f32]) -> Option<usize>,
    ) -> Decoded<'_> {
        let Self { opus, frame } = self;
        match catch_unwind(AssertUnwindSafe(|| call(opus, frame))) {
            Ok(Some(FRAME_SAMPLES)) => Decoded::Samples(frame),
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

#[cfg(test)]
mod tests;
