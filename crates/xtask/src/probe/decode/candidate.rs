//! The pure Rust decoder under test, with every call into it fenced so a panic is a count and not the end of the run.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use rusty_aac::AacDecoder;

/// A call into the decoder longer than this is counted as slow: 100 ms, thirteen times the 7.5 ms a unit lasts.
pub(crate) const SLOW_CALL: Duration = Duration::from_millis(100);

/// One call of a decoder: a unit in, its samples or the decoder's own error out.
type Call = Box<dyn FnMut(&[u8]) -> Result<Vec<f32>, String>>;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum UnitResult {
    Samples(Vec<f32>),
    Error(String),
    Panic,
}

pub(crate) struct Candidate {
    call: Call,
}

impl Candidate {
    /// A decoder for raw access units of the stream these four configuration bytes describe, or the decoder's reason for refusing them.
    pub(crate) fn new(config: [u8; 4]) -> Result<Self, String> {
        let built = catch_unwind(|| AacDecoder::with_config_bytes(&config));
        let mut decoder = built
            .map_err(|_panic| "the decoder panicked on the configuration".to_owned())?
            .map_err(|error| error.to_string())?;
        let call = Box::new(move |unit: &[u8]| {
            let audio = decoder.decode(unit, None);
            audio
                .map(|audio| audio.samples)
                .map_err(|error| error.to_string())
        });
        Ok(Self { call })
    }

    /// A decoder that panicked is kept as it is, so what it does next is counted too.
    pub(crate) fn unit(&mut self, unit: &[u8]) -> UnitResult {
        match catch_unwind(AssertUnwindSafe(|| (self.call)(unit))) {
            Ok(Ok(samples)) => UnitResult::Samples(samples),
            Ok(Err(error)) => UnitResult::Error(error),
            Err(_) => UnitResult::Panic,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Tally {
    /// Calls that gave samples: one frame of them wherever the caller names a frame.
    pub(crate) decoded: usize,
    pub(crate) errors: usize,
    pub(crate) panics: usize,
    pub(crate) samples: Vec<f32>,
    pub(crate) first_error: Option<String>,
    /// The time inside the decoder over all calls, and the longest single call.
    pub(crate) total: Duration,
    pub(crate) slowest: Duration,
    /// Calls that took longer than `SLOW_CALL`.
    pub(crate) slow: usize,
}

impl Tally {
    /// Feeds every unit to the candidate, in order, each exactly once.
    pub(crate) fn of(candidate: &mut Candidate, units: &[&[u8]], frame: usize) -> Self {
        let mut tally = Self::default();
        for unit in units {
            tally.call(Some(frame), || candidate.unit(unit));
        }
        tally
    }

    /// Times one call of a decoder and counts what it gave; samples that are not one frame of `frame` are an error, and where no frame is named any count passes.
    pub(crate) fn call(&mut self, frame: Option<usize>, decode: impl FnOnce() -> UnitResult) {
        let started = Instant::now();
        let result = decode();
        self.took(started.elapsed());
        self.count(result, frame);
    }

    fn took(&mut self, call: Duration) {
        self.total = self.total.saturating_add(call);
        self.slowest = self.slowest.max(call);
        if call > SLOW_CALL {
            self.slow = self.slow.saturating_add(1);
        }
    }

    fn count(&mut self, result: UnitResult, frame: Option<usize>) {
        let error = match (result, frame) {
            // Not one frame: an error, so the decode is not whole and nothing lays it frame on frame against another.
            (UnitResult::Samples(samples), Some(frame)) if samples.len() != frame => {
                format!(
                    "{} samples from one unit, where a frame is {frame}",
                    samples.len()
                )
            }
            (UnitResult::Samples(samples), _) => {
                self.decoded = self.decoded.saturating_add(1);
                self.samples.extend(samples);
                return;
            }
            (UnitResult::Error(error), _) => error,
            (UnitResult::Panic, _) => {
                self.panics = self.panics.saturating_add(1);
                return;
            }
        };
        self.errors = self.errors.saturating_add(1);
        self.first_error.get_or_insert(error);
    }
}

#[cfg(test)]
mod tests;
