//! Unit tests for `pods-aac`: config parsing, syntax edge cases, TNS, IMDCT accuracy, and state preservation.

use std::f64::consts::PI;

use super::*;
use crate::bits::BitReader;
use crate::encode::{BitWriter, emit_cb1_quad};
use crate::filterbank::Filterbank;
use crate::tables::huffman::CB11;

const SILENT_UNIT: [u8; 2] = [0x00, 0x00];
const TONE_UNIT: [u8; 4] = [0xb4, 0x04, 0x42, 0x50];

#[test]
fn config_accepts_only_expected_eld_480_48k_mono() {
    assert!(Decoder::with_config(&EXPECTED_CONFIG).is_ok());
    // 512-sample frame (frameLengthFlag = 0)
    assert_eq!(
        Decoder::with_config(&[0xf8, 0xe6, 0x20, 0x00]).unwrap_err(),
        Error::UnsupportedConfig
    );
    // 64 kHz sampling index 2
    assert_eq!(
        Decoder::with_config(&[0xf8, 0xe4, 0x30, 0x00]).unwrap_err(),
        Error::UnsupportedConfig
    );
    // Stereo (channelConfiguration = 2)
    assert_eq!(
        Decoder::with_config(&[0xf8, 0xe6, 0x50, 0x00]).unwrap_err(),
        Error::UnsupportedConfig
    );
    // LD-SBR flag set
    assert_eq!(
        Decoder::with_config(&[0xf8, 0xe6, 0x31, 0x00]).unwrap_err(),
        Error::UnsupportedConfig
    );
    // Wrong length or all zeros
    assert_eq!(
        Decoder::with_config(&[0x00; 4]).unwrap_err(),
        Error::UnsupportedConfig
    );
    assert_eq!(
        Decoder::with_config(&[0xf8, 0xe6, 0x30]).unwrap_err(),
        Error::UnsupportedConfig
    );
    assert_eq!(
        Decoder::with_config(&[0xf8, 0xe6, 0x30, 0x00, 0x00]).unwrap_err(),
        Error::UnsupportedConfig
    );
    // Flipping any single bit of EXPECTED_CONFIG must be refused.
    for bit in 0..32_u8 {
        let mut mutated = EXPECTED_CONFIG;
        mutated[usize::from(bit / 8)] ^= 1_u8 << (7 - (bit % 8));
        assert_eq!(
            Decoder::with_config(&mutated).unwrap_err(),
            Error::UnsupportedConfig,
            "bit {bit}"
        );
    }
}

#[test]
fn silent_and_tone_units_decode_cleanly() {
    let mut decoder = Decoder::new();
    for _ in 0..10 {
        let out = decoder.decode(&SILENT_UNIT).unwrap();
        assert_eq!(out.len(), SAMPLES_PER_UNIT);
        assert!(out.iter().all(|&s| s == 0.0));
    }
    let out = decoder.decode(&TONE_UNIT).unwrap();
    assert_eq!(out.len(), SAMPLES_PER_UNIT);
    let peak = out.iter().fold(0.0_f32, |m, &s| m.max(s.abs()));
    assert!(peak > 0.06 && peak < 0.08, "peak={peak}");
}

#[test]
fn encode_matches_known_fixture_bytes() {
    let mut writer = BitWriter::new();
    writer.write(0, 8);
    writer.write(0, 6);
    writer.write_bool(false);
    assert_eq!(writer.into_bytes(), SILENT_UNIT);

    let mut writer = BitWriter::new();
    writer.write(0xb4, 8);
    writer.write(1, 6);
    writer.write(1, 4);
    writer.write(1, 5);
    writer.write_bool(false);
    writer.write_bool(false);
    emit_cb1_quad([0, 0, 0, 1], &mut writer);
    assert_eq!(writer.into_bytes(), TONE_UNIT);
}

#[test]
fn fast_imdct_matches_direct_cosine_definition_to_f64_precision() {
    let mut spec = [0.0_f64; SAMPLES_PER_UNIT];
    let mut state = 0x1234_5678_9abc_def0_u64;
    for slot in &mut spec {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let low = u16::try_from(state & 0xffff).unwrap();
        *slot = (f64::from(low) / 32768.0) - 1.0;
    }

    let mut filterbank = Filterbank::new();
    let mut fast_frames = [[0.0_f32; SAMPLES_PER_UNIT]; 4];
    filterbank.synthesize(&spec, &mut fast_frames[0]);
    let zero = [0.0_f64; SAMPLES_PER_UNIT];
    for frame in fast_frames.iter_mut().skip(1) {
        filterbank.synthesize(&zero, frame);
    }

    // Direct O(M^2) reference synthesis over all 1920 samples.
    let n0 = 840.5_f64;
    let mut max_error = 0.0_f64;
    for n in 0..1920_u16 {
        let mut sum = 0.0_f64;
        for (k, &xk) in spec.iter().enumerate() {
            let k_value = f64::from(u16::try_from(k).unwrap());
            let angle = (2.0 * PI / 960.0) * (f64::from(n) + n0) * (k_value + 0.5);
            sum += xk * angle.cos();
        }
        let index = usize::from(n);
        let w = tables::window::ELD_WINDOW_480
            .get(index)
            .copied()
            .unwrap_or(0.0);
        #[expect(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            reason = "reference f64-to-f32 sample comparison"
        )]
        let reference_sample = (sum * (2.0 / 960.0) * w) as f32;
        let fast_sample = fast_frames[index / SAMPLES_PER_UNIT][index % SAMPLES_PER_UNIT];
        max_error = max_error.max(f64::from((fast_sample - reference_sample).abs()));
    }
    assert!(max_error < 1e-6, "max_error={max_error:e}");
}

#[test]
fn errors_are_refused_without_corrupting_overlap_state() {
    let mut decoder = Decoder::new();
    let mut twin = Decoder::new();

    // Prime both decoders with the same tone unit.
    let a0 = decoder.decode(&TONE_UNIT).unwrap().to_vec();
    let b0 = twin.decode(&TONE_UNIT).unwrap().to_vec();
    assert_eq!(a0, b0);

    // Feed bad units to `decoder` only.
    assert_eq!(decoder.decode(&[]), Err(Error::EmptyUnit));
    assert_eq!(
        decoder.decode(&[0xff, 0xff]),
        Err(Error::MaxSfbOutOfRange(63))
    );
    assert_eq!(
        decoder.decode(&vec![0_u8; MAX_UNIT_BYTES + 1]),
        Err(Error::OversizedUnit(MAX_UNIT_BYTES + 1))
    );
    // Truncated tone unit
    assert_eq!(decoder.decode(&TONE_UNIT[..3]), Err(Error::Truncated));

    // Following valid unit must match `twin` bit-for-bit!
    let a1 = decoder.decode(&SILENT_UNIT).unwrap().to_vec();
    let b1 = twin.decode(&SILENT_UNIT).unwrap().to_vec();
    assert_eq!(a1, b1);
}

#[test]
fn edge_cases_reserved_cb_invalid_section_and_tns_order() {
    let mut decoder = Decoder::new();

    // Reserved CB 12
    let mut writer = BitWriter::new();
    writer.write(160, 8);
    writer.write(1, 6);
    writer.write(12, 4);
    writer.write(1, 5);
    writer.write_bool(false);
    assert_eq!(
        decoder.decode(&writer.into_bytes()),
        Err(Error::ReservedCodebook)
    );

    // Section length 0
    let mut writer = BitWriter::new();
    writer.write(160, 8);
    writer.write(1, 6);
    writer.write(0, 4);
    writer.write(0, 5);
    assert_eq!(
        decoder.decode(&writer.into_bytes()),
        Err(Error::InvalidSection)
    );

    // TNS order 13 (> 12)
    let mut writer = BitWriter::new();
    writer.write(160, 8);
    writer.write(1, 6);
    writer.write(0, 4);
    writer.write(1, 5);
    writer.write_bool(true); // tns_present
    writer.write(1, 2); // n_filt = 1
    writer.write_bool(false); // coef_res = 3-bit
    writer.write(1, 6); // length = 1
    writer.write(13, 5); // order = 13 > 12
    assert_eq!(
        decoder.decode(&writer.into_bytes()),
        Err(Error::TnsOrderOutOfRange(13))
    );
}

/// Parses `unit` and applies TNS, returning the spectrum the filterbank would receive.
fn parsed_spectrum(unit: &[u8]) -> Result<[f64; SAMPLES_PER_UNIT], Error> {
    let mut spec = [0.0_f64; SAMPLES_PER_UNIT];
    let mut seed = PNS_SEED;
    let parsed = ics::parse_eld_sce(&mut BitReader::new(unit), &mut spec, &mut seed)?;
    parsed.tns.apply(parsed.max_sfb, &mut spec);
    Ok(spec)
}

/// Two CB1 bands holding +1 at lines 0 and 7 at unit gain, then one order-2 TNS filter over every band.
fn tns_unit(direction_down: bool) -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.write(160, 8); // global_gain 160 is a gain of exactly 1
    writer.write(2, 6); // max_sfb
    writer.write(1, 4); // codebook 1
    writer.write(2, 5); // section of 2 bands
    writer.write_bool(false); // scalefactor delta 0
    writer.write_bool(false); // scalefactor delta 0
    writer.write_bool(true); // tns_present
    writer.write(1, 2); // n_filt
    writer.write_bool(false); // 3-bit coefficients
    writer.write(35, 6); // length
    writer.write(2, 5); // order
    writer.write_bool(direction_down);
    writer.write_bool(false); // no compression
    writer.write(1, 3); // +1
    writer.write(7, 3); // -1
    emit_cb1_quad([1, 0, 0, 0], &mut writer);
    emit_cb1_quad([0, 0, 0, 1], &mut writer);
    writer.into_bytes()
}

#[test]
fn tns_filters_the_spectrum_with_the_spec_coefficients_in_both_directions() {
    // 3-bit coefficients 1 and -1 dequantize to sin(pi/7) and sin(-pi/9) (ISO/IEC 14496-3 Clause 4.6.9.3).
    let rc1 = (PI / 7.0).sin();
    let rc2 = (-PI / 9.0).sin();
    let a1 = rc1 + rc2 * rc1;
    let a2 = rc2;
    for direction_down in [false, true] {
        let spec = parsed_spectrum(&tns_unit(direction_down)).unwrap();
        let input = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        let order: Vec<usize> = if direction_down {
            (0..8).rev().collect()
        } else {
            (0..8).collect()
        };
        let mut want = [0.0_f64; 8];
        let (mut y1, mut y2) = (0.0, 0.0);
        for &line in &order {
            let y = input[line] - a1 * y1 - a2 * y2;
            want[line] = y;
            y2 = y1;
            y1 = y;
        }
        for (line, (&got, &expected)) in spec.iter().zip(want.iter()).enumerate() {
            assert!(
                (got - expected).abs() < 1e-12,
                "down={direction_down} line {line}: {got} != {expected}"
            );
        }
        assert!(spec.iter().skip(8).all(|&s| s == 0.0));
        assert!((spec[1] - input[1]).abs() > 0.1 || (spec[6] - input[6]).abs() > 0.1);
    }
}

#[test]
fn decode_synthesizes_the_tns_filtered_spectrum() {
    for direction_down in [false, true] {
        let unit = tns_unit(direction_down);
        let mut want = [0.0_f32; SAMPLES_PER_UNIT];
        Filterbank::new().synthesize(&parsed_spectrum(&unit).unwrap(), &mut want);
        assert_eq!(
            Decoder::new().decode(&unit).unwrap(),
            want,
            "down={direction_down}"
        );
    }
}

/// One noise band whose first energy is `pcm` (9 bits, bias 256) above `global_gain - 90`.
fn pns_unit(global_gain: u32, pcm: u32) -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.write(global_gain, 8);
    writer.write(1, 6); // max_sfb
    writer.write(13, 4); // noise codebook
    writer.write(1, 5); // section of 1 band
    writer.write(pcm, 9);
    writer.write_bool(false); // no TNS
    writer.into_bytes()
}

#[test]
fn pns_band_energy_is_two_to_the_half_noise_energy_at_float_scale_and_clamps() {
    // (global_gain, pcm, noise_nrg after the clamp): 160 - 90 + 0 = 70; 255 - 90 + 255 = 420 clamps to 155; 0 - 90 - 256 clamps to -100.
    for (global_gain, pcm, noise_nrg) in [(160, 256, 70), (255, 511, 155), (0, 0, -100)] {
        let spec = parsed_spectrum(&pns_unit(global_gain, pcm)).unwrap();
        let energy: f64 = spec.iter().take(4).map(|s| s * s).sum();
        // 2^(noise_nrg / 2) at the spec's integer scale, times the 2^-30 energy of the 2^-15 float scale.
        let want = (f64::from(noise_nrg - 60) * 0.5).exp2();
        assert!(
            (energy - want).abs() < want * 1e-12,
            "noise_nrg {noise_nrg}: energy {energy} != {want}"
        );
        assert!(spec.iter().skip(4).all(|&s| s == 0.0));
    }
}

/// One CB11 band of two `(16, 0)` pairs, each escaped with `ones` prefix ones and an all-ones word.
fn escape_unit(ones: u32) -> Vec<u8> {
    let escape_pair = 16 * 17;
    let (len, code) = CB11[escape_pair];
    let mut writer = BitWriter::new();
    writer.write(160, 8); // global_gain
    writer.write(1, 6); // max_sfb
    writer.write(11, 4); // CB11
    writer.write(1, 5); // section of 1 band
    writer.write_bool(false); // scalefactor delta 0
    writer.write_bool(false); // no TNS
    for _ in 0..2 {
        writer.write(u32::from(code), u32::from(len));
        writer.write_bool(false); // positive sign
        for _ in 0..ones {
            writer.write_bool(true);
        }
        writer.write_bool(false);
        let width = ones.wrapping_add(4);
        writer.write(1_u32.wrapping_shl(width).wrapping_sub(1), width);
    }
    writer.into_bytes()
}

#[test]
fn escape_accepts_eight_prefix_ones_and_refuses_nine() {
    let spec = parsed_spectrum(&escape_unit(8)).unwrap();
    let want = 8191.0_f64.powf(4.0 / 3.0);
    assert!(
        (spec[0] - want).abs() < want * 1e-12,
        "{} != {want}",
        spec[0]
    );
    assert!(
        (spec[2] - want).abs() < want * 1e-12,
        "{} != {want}",
        spec[2]
    );
    assert_eq!(parsed_spectrum(&escape_unit(9)), Err(Error::EscapeOverflow));
    assert_eq!(
        Decoder::new().decode(&escape_unit(9)),
        Err(Error::EscapeOverflow)
    );
}
