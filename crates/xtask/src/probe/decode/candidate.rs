//! The pure Rust decoder under test, with every call into it fenced so a panic is a count and not the end of the run.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use pods_mic::EXPECTED_CONFIG;
use pods_mic::decode::{Decoded, Decoder};
use rusty_aac::AacDecoder;

/// A call into the decoder longer than this is counted as slow: 100 ms, thirteen times the 7.5 ms a unit lasts.
pub(crate) const SLOW_CALL: Duration = Duration::from_millis(100);

/// What the microphone chain's decoder says of a unit it gives no frame for; it keeps no reason.
const REFUSED_UNIT: &str = "the decoder refused the unit, or gave other than one frame";

/// One call of a decoder: a unit in, what it made of it out.
type Call = Box<dyn FnMut(&[u8]) -> UnitResult>;

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
    /// The microphone chain's decoder where the configuration is the one it takes; for any other, the same library told so. Or the reason the configuration was refused.
    pub(crate) fn new(config: [u8; 4]) -> Result<Self, String> {
        if config == EXPECTED_CONFIG {
            return Self::of_the_chain();
        }
        let built = catch_unwind(|| AacDecoder::with_config_bytes(&config));
        let mut decoder = built
            .map_err(|_panic| "the decoder panicked on the configuration".to_owned())?
            .map_err(|error| error.to_string())?;
        let call = Box::new(move |unit: &[u8]| match decoder.decode(unit, None) {
            Ok(audio) => UnitResult::Samples(audio.samples),
            Err(error) => UnitResult::Error(error.to_string()),
        });
        Ok(Self { call })
    }

    fn of_the_chain() -> Result<Self, String> {
        let mut decoder = Decoder::new().ok_or("the decoder refused its own configuration")?;
        let call = Box::new(move |unit: &[u8]| match decoder.unit(unit) {
            Decoded::Samples(samples) => UnitResult::Samples(samples.to_vec()),
            Decoded::Error => UnitResult::Error(REFUSED_UNIT.to_owned()),
            Decoded::Panic => UnitResult::Panic,
        });
        Ok(Self { call })
    }

    /// A decoder that panicked is kept as it is, so what it does next is counted too.
    pub(crate) fn unit(&mut self, unit: &[u8]) -> UnitResult {
        catch_unwind(AssertUnwindSafe(|| (self.call)(unit))).unwrap_or(UnitResult::Panic)
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
