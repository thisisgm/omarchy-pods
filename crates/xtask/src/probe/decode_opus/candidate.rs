//! The pure Rust Opus decoder under test, with every call into it fenced so a panic is a count and not the end of the run.

use std::panic::{AssertUnwindSafe, catch_unwind};

use opuscule::{Channels, Decoder, SampleRate};

pub(crate) use crate::probe::decode::candidate::UnitResult as FrameResult;

/// The longest packet Opus allows, 120 ms, in samples at 48 kHz, so no packet finds the buffer too small.
const MAX_PACKET_SAMPLES: usize = 5760;

/// The rate the candidate decodes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputRate {
    Hz16000,
    Hz48000,
}

impl OutputRate {
    pub(crate) const fn hz(self) -> u32 {
        match self {
            Self::Hz16000 => 16_000,
            Self::Hz48000 => 48_000,
        }
    }

    /// The samples of one 20 ms frame at this rate.
    pub(crate) const fn frame(self) -> u16 {
        match self {
            Self::Hz16000 => 320,
            Self::Hz48000 => 960,
        }
    }
}

/// One call of a decoder: a packet, or none for a lost one, and the buffer its samples go to; the count of samples or the decoder's own error out.
type Call = Box<dyn FnMut(Option<&[u8]>, &mut [f32]) -> Result<usize, String>>;

/// The decoder candidate at one output rate, mono.
pub(crate) struct Candidate {
    call: Call,
    rate: OutputRate,
    pcm: Vec<f32>,
}

impl Candidate {
    pub(crate) fn new(rate: OutputRate) -> Self {
        let sample_rate = match rate {
            OutputRate::Hz16000 => SampleRate::Hz16000,
            OutputRate::Hz48000 => SampleRate::Hz48000,
        };
        let mut decoder = Decoder::new(sample_rate, Channels::Mono);
        let call = move |packet: Option<&[u8]>, pcm: &mut [f32]| {
            let samples = decoder.decode(packet, pcm, false);
            samples.map_err(|error| error.to_string())
        };
        Self::calling(Box::new(call), rate)
    }

    fn calling(call: Call, rate: OutputRate) -> Self {
        Self {
            call,
            rate,
            pcm: vec![0.0; MAX_PACKET_SAMPLES],
        }
    }

    /// A decoder that panicked is kept as it is, so what it does next is counted too.
    pub(crate) fn frame(&mut self, frame: &[u8]) -> FrameResult {
        self.fenced(Some(frame), MAX_PACKET_SAMPLES)
    }

    /// One frame of concealment, as for a frame that never came.
    pub(crate) fn conceal(&mut self) -> FrameResult {
        self.fenced(None, usize::from(self.rate.frame()))
    }

    fn fenced(&mut self, packet: Option<&[u8]>, room: usize) -> FrameResult {
        let Some(pcm) = self.pcm.get_mut(..room) else {
            return FrameResult::Error(format!("no room for {room} samples"));
        };
        let call = &mut self.call;
        match catch_unwind(AssertUnwindSafe(|| call(packet, pcm))) {
            Ok(Ok(samples)) => match pcm.get(..samples) {
                Some(decoded) => FrameResult::Samples(decoded.to_vec()),
                None => FrameResult::Error(format!("{samples} samples in a buffer of {room}")),
            },
            Ok(Err(error)) => FrameResult::Error(error),
            Err(_) => FrameResult::Panic,
        }
    }
}

#[cfg(test)]
mod tests;
