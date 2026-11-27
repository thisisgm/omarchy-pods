//! Clean-room MIT ER AAC-ELD decoder for the AirPods 0x58 uplink microphone configuration.
//!
//! Decodes exclusively `EXPECTED_CONFIG = [0xf8, 0xe6, 0x30, 0x00]`:
//! - ISO/IEC 14496-3 Clause 1.6.2.1 `AudioSpecificConfig`: AOT 39 (ER AAC-ELD),
//!   sampling frequency index 3 (48 kHz declared), channel configuration 1 (mono), `epConfig = 0`.
//! - ISO/IEC 14496-3 Clause 4.5.1.1 `ELDSpecificConfig`: `frameLengthFlag = 1` (480 samples),
//!   all error resilience flags 0, `ldSbrPresentFlag = 0`, `eldExtType = ELDEXT_TERM (0)`.

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

/// The single `AudioSpecificConfig` this decoder accepts (ER AAC-ELD, mono, 480 samples, 48 kHz).
pub const EXPECTED_CONFIG: [u8; 4] = [0xf8, 0xe6, 0x30, 0x00];

/// Decoded PCM samples produced per access unit.
pub const SAMPLES_PER_UNIT: usize = 480;

/// Maximum accepted access unit byte length (AirPods L2CAP units cap at 255 bytes; 4096 leaves margin for synthetic tests).
pub const MAX_UNIT_BYTES: usize = 4096;

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
            pns_seed: 0x1f2e_3d4c,
        }
    }

    /// Parses an ISO/IEC 14496-3 `AudioSpecificConfig` and constructs a decoder if and only if
    /// it specifies ER AAC-ELD (AOT 39), 48 kHz (index 3), mono (1), 480-sample frames,
    /// zero resilience flags, no LD-SBR, no ELD extensions, and `epConfig = 0`.
    pub fn with_config(config: &[u8]) -> Result<Self, Error> {
        validate_config(config)?;
        Ok(Self::new())
    }

    /// Resets the overlap-add filterbank history and PNS state.
    pub fn reset(&mut self) {
        self.filterbank.reset();
        self.output.fill(0.0);
        self.pns_seed = 0x1f2e_3d4c;
    }

    /// Decodes one raw ER AAC-ELD access unit into 480 mono `f32` PCM samples.
    ///
    /// Performs zero heap allocations. On error, filterbank overlap state is left untouched.
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
            return Err(Error::NonFiniteOutput);
        }
        Ok(&self.output)
    }

    /// Returns the 480-sample buffer written by the most recent successful [`Self::decode`] call.
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

/// Validates ISO/IEC 14496-3 Clauses 1.6.2.1 (`AudioSpecificConfig`) and 4.5.1.1 (`ELDSpecificConfig`).
fn validate_config(config: &[u8]) -> Result<(), Error> {
    if config.len() != EXPECTED_CONFIG.len() {
        return Err(Error::UnsupportedConfig);
    }
    let mut reader = BitReader::new(config);
    // GetAudioObjectType() (Table 1.16): 5-bit escape 31 + 6-bit ext => 32 + 7 = 39 (ER AAC-ELD).
    let aot_base = reader.read_bits(5).ok().ok_or(Error::UnsupportedConfig)?;
    let aot = if aot_base == 31 {
        let ext = reader.read_bits(6).ok().ok_or(Error::UnsupportedConfig)?;
        32_u32.wrapping_add(ext)
    } else {
        aot_base
    };
    if aot != 39 {
        return Err(Error::UnsupportedConfig);
    }
    // samplingFrequencyIndex (Table 1.18): 3 = 48,000 Hz.
    let sf_index = reader.read_bits(4).ok().ok_or(Error::UnsupportedConfig)?;
    if sf_index != 3 {
        return Err(Error::UnsupportedConfig);
    }
    // channelConfiguration (Table 1.19): 1 = mono.
    let channel_config = reader.read_bits(4).ok().ok_or(Error::UnsupportedConfig)?;
    if channel_config != 1 {
        return Err(Error::UnsupportedConfig);
    }
    // ELDSpecificConfig (Table 4.180):
    // frameLengthFlag = 1 (480 samples), resilience flags = 000, ldSbrPresentFlag = 0.
    let frame_length_flag = reader.read_bool().ok().ok_or(Error::UnsupportedConfig)?;
    let section_resilience = reader.read_bool().ok().ok_or(Error::UnsupportedConfig)?;
    let sf_resilience = reader.read_bool().ok().ok_or(Error::UnsupportedConfig)?;
    let spectral_resilience = reader.read_bool().ok().ok_or(Error::UnsupportedConfig)?;
    let ld_sbr_present = reader.read_bool().ok().ok_or(Error::UnsupportedConfig)?;
    if !frame_length_flag
        || section_resilience
        || sf_resilience
        || spectral_resilience
        || ld_sbr_present
    {
        return Err(Error::UnsupportedConfig);
    }
    // eldExtType loop terminated immediately by ELDEXT_TERM (0x0).
    let eld_ext_type = reader.read_bits(4).ok().ok_or(Error::UnsupportedConfig)?;
    if eld_ext_type != 0 {
        return Err(Error::UnsupportedConfig);
    }
    // epConfig (Table 1.15 for AOT 39): 0 (no error protection layer).
    let ep_config = reader.read_bits(2).ok().ok_or(Error::UnsupportedConfig)?;
    if ep_config != 0 {
        return Err(Error::UnsupportedConfig);
    }
    // Trailing alignment bits must be zero.
    let trailing = reader.read_bits(2).ok().ok_or(Error::UnsupportedConfig)?;
    if trailing != 0 || reader.bits_left() != 0 {
        return Err(Error::UnsupportedConfig);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
