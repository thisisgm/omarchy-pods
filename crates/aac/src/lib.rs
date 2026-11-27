//! Clean-room MIT ER AAC-ELD decoder for the one AirPods uplink microphone configuration, [`EXPECTED_CONFIG`].

#![forbid(unsafe_code)]

mod bits;
pub mod encode;
mod filterbank;
mod huffman;
mod ics;
mod tables;
mod tns;

use std::fmt;

use bits::BitReader;
use filterbank::Filterbank;
pub use tables::BAND_EDGES;

/// The only `AudioSpecificConfig` accepted: AOT 39, 48 kHz, mono, 480-sample frames, no resilience, SBR, extension or `epConfig`.
pub const EXPECTED_CONFIG: [u8; 4] = [0xf8, 0xe6, 0x30, 0x00];

/// Decoded PCM samples produced per access unit.
pub const SAMPLES_PER_UNIT: usize = 480;

/// Maximum accepted access unit byte length (AirPods L2CAP units cap at 255 bytes; 4096 leaves margin for synthetic tests).
pub const MAX_UNIT_BYTES: usize = 4096;

/// Starting state of the PNS noise generator, shared by [`Decoder::new`] and [`Decoder::reset`].
const PNS_SEED: u32 = 0x1f2e_3d4c;

/// Errors returned by [`Decoder::with_config`] and [`Decoder::decode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The supplied `AudioSpecificConfig` is not ER AAC-ELD mono 480-sample 48 kHz without SBR/resilience/extensions.
    UnsupportedConfig,
    /// Access unit is empty.
    EmptyUnit,
    /// Access unit exceeds [`MAX_UNIT_BYTES`].
    OversizedUnit(usize),
    /// Bitstream ended before the frame syntax completed.
    Truncated,
    /// `max_sfb` exceeds the 35 scalefactor bands of a 480-sample 48 kHz frame.
    MaxSfbOutOfRange(usize),
    /// Section length is zero or overflows `max_sfb`.
    InvalidSection,
    /// Reserved Huffman codebook 12 was signalled.
    ReservedCodebook,
    /// Decoded scalefactor fell outside `0..=255`.
    ScaleFactorOutOfRange(i32),
    /// TNS filter order exceeded the ELD maximum of 12.
    TnsOrderOutOfRange(usize),
    /// Codebook 11 escape prefix exceeded the ISO/IEC 14496-3 limit.
    EscapeOverflow,
    /// Malformed Huffman or bitstream syntax.
    InvalidBitstream,
    /// Synthesized frame contained a non-finite sample.
    NonFiniteOutput,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedConfig => f.write_str("unsupported AudioSpecificConfig"),
            Self::EmptyUnit => f.write_str("empty access unit"),
            Self::OversizedUnit(len) => {
                write!(f, "access unit length {len} exceeds limit {MAX_UNIT_BYTES}")
            }
            Self::Truncated => f.write_str("truncated access unit"),
            Self::MaxSfbOutOfRange(n) => {
                write!(f, "max_sfb {n} exceeds 35 scalefactor bands")
            }
            Self::InvalidSection => f.write_str("invalid section data"),
            Self::ReservedCodebook => f.write_str("reserved spectral codebook 12"),
            Self::ScaleFactorOutOfRange(sf) => {
                write!(f, "scalefactor {sf} outside 0..=255")
            }
            Self::TnsOrderOutOfRange(order) => {
                write!(f, "TNS filter order {order} exceeds maximum 12")
            }
            Self::EscapeOverflow => f.write_str("codebook 11 escape sequence overflow"),
            Self::InvalidBitstream => f.write_str("invalid bitstream syntax"),
            Self::NonFiniteOutput => f.write_str("non-finite synthesized output"),
        }
    }
}

impl std::error::Error for Error {}

/// Preallocated, zero-allocation-per-unit ER AAC-ELD (480 @ 48 kHz mono) decoder.
#[derive(Debug, Clone)]
pub struct Decoder {
    filterbank: Filterbank,
    spec: [f64; SAMPLES_PER_UNIT],
    output: [f32; SAMPLES_PER_UNIT],
    pns_seed: u32,
}

impl Decoder {
    /// Creates a decoder for `EXPECTED_CONFIG`.
    #[must_use]
    pub fn new() -> Self {
        // Eagerly warm up process-global static tables here so `decode` performs zero allocations.
        let _huff = huffman::tables();
        let _math = tables::math();
        Self {
            filterbank: Filterbank::new(),
            spec: [0.0; SAMPLES_PER_UNIT],
            output: [0.0; SAMPLES_PER_UNIT],
            pns_seed: PNS_SEED,
        }
    }

    /// Builds a decoder when `config` is exactly [`EXPECTED_CONFIG`], the only configuration decoded.
    pub fn with_config(config: &[u8]) -> Result<Self, Error> {
        if config != EXPECTED_CONFIG {
            return Err(Error::UnsupportedConfig);
        }
        Ok(Self::new())
    }

    /// Resets the overlap-add filterbank history and PNS state.
    pub fn reset(&mut self) {
        self.filterbank.reset();
        self.output.fill(0.0);
        self.pns_seed = PNS_SEED;
    }

    /// Decodes one access unit into 480 mono samples without allocating; a parse error leaves the overlap untouched.
    pub fn decode(&mut self, unit: &[u8]) -> Result<&[f32], Error> {
        if unit.is_empty() {
            return Err(Error::EmptyUnit);
        }
        if unit.len() > MAX_UNIT_BYTES {
            return Err(Error::OversizedUnit(unit.len()));
        }

        let mut reader = BitReader::new(unit);
        let parsed = ics::parse_eld_sce(&mut reader, &mut self.spec, &mut self.pns_seed)?;
        parsed.tns.apply(parsed.max_sfb, &mut self.spec);
        if !self.spec.iter().all(|s| s.is_finite()) {
            return Err(Error::NonFiniteOutput);
        }
        self.filterbank.synthesize(&self.spec, &mut self.output);
        if !self.output.iter().all(|s| s.is_finite()) {
            // The overlap already holds this frame, so start clean rather than carry it into the next three.
            self.reset();
            return Err(Error::NonFiniteOutput);
        }
        Ok(&self.output)
    }

    /// Returns the 480 samples of the last successful [`Self::decode`], or silence after a reset.
    #[must_use]
    pub fn last_frame(&self) -> &[f32] {
        &self.output
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
