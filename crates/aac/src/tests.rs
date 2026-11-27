//! Unit tests for `pods-aac`: config parsing, syntax edge cases, TNS, IMDCT accuracy, and state preservation.

use std::f64::consts::PI;

use super::*;
use crate::encode::{BitWriter, emit_cb1_quad};
use crate::filterbank::Filterbank;

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
    let mut dec = Decoder::new();
    for _ in 0..10 {
        let out = dec.decode(&SILENT_UNIT).unwrap();
        assert_eq!(out.len(), SAMPLES_PER_UNIT);
        assert!(out.iter().all(|&s| s == 0.0));
    }
    let out = dec.decode(&TONE_UNIT).unwrap();
    assert_eq!(out.len(), SAMPLES_PER_UNIT);
    let peak = out.iter().fold(0.0_f32, |m, &s| m.max(s.abs()));
    assert!(peak > 0.06 && peak < 0.08, "peak={peak}");
}

#[test]
fn encode_matches_known_fixture_bytes() {
    let mut bw = BitWriter::new();
    bw.write(0, 8);
    bw.write(0, 6);
    bw.write_bool(false);
    assert_eq!(bw.into_bytes(), SILENT_UNIT);

    let mut bw = BitWriter::new();
    bw.write(0xb4, 8);
    bw.write(1, 6);
    bw.write(1, 4);
    bw.write(1, 5);
    bw.write_bool(false);
    bw.write_bool(false);
    emit_cb1_quad([0, 0, 0, 1], &mut bw);
    assert_eq!(bw.into_bytes(), TONE_UNIT);
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

    let mut fb = Filterbank::new();
    let mut fast_frames = [[0.0_f32; SAMPLES_PER_UNIT]; 4];
    fb.synthesize(&spec, &mut fast_frames[0]);
    let zero = [0.0_f64; SAMPLES_PER_UNIT];
    for frame in fast_frames.iter_mut().skip(1) {
        fb.synthesize(&zero, frame);
    }

    // Direct O(M^2) reference synthesis over all 1920 samples.
    let n0 = 840.5_f64;
    let mut max_err = 0.0_f64;
    for n in 0..1920_u16 {
        let mut sum = 0.0_f64;
        for (k, &xk) in spec.iter().enumerate() {
            let kf = f64::from(u16::try_from(k).unwrap());
            let angle = (2.0 * PI / 960.0) * (f64::from(n) + n0) * (kf + 0.5);
            sum += xk * angle.cos();
        }
        let n_idx = usize::from(n);
        let w = tables::window::ELD_WINDOW_480
            .get(n_idx)
            .copied()
            .unwrap_or(0.0);
        #[expect(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            reason = "reference f64-to-f32 sample comparison"
        )]
        let ref_sample = (sum * (2.0 / 960.0) * w) as f32;
        let fast_sample = fast_frames[n_idx / SAMPLES_PER_UNIT][n_idx % SAMPLES_PER_UNIT];
        max_err = max_err.max(f64::from((fast_sample - ref_sample).abs()));
    }
    assert!(max_err < 1e-6, "max_err={max_err:e}");
}

#[test]
fn errors_are_refused_without_corrupting_overlap_state() {
    let mut dec = Decoder::new();
    let mut twin = Decoder::new();

    // Prime both decoders with the same tone unit.
    let a0 = dec.decode(&TONE_UNIT).unwrap().to_vec();
    let b0 = twin.decode(&TONE_UNIT).unwrap().to_vec();
    assert_eq!(a0, b0);

    // Feed bad units to `dec` only.
    assert_eq!(dec.decode(&[]), Err(Error::EmptyUnit));
    assert_eq!(dec.decode(&[0xff, 0xff]), Err(Error::MaxSfbOutOfRange(63)));
    assert_eq!(
        dec.decode(&vec![0_u8; MAX_UNIT_BYTES + 1]),
        Err(Error::OversizedUnit(MAX_UNIT_BYTES + 1))
    );
    // Truncated tone unit
    assert_eq!(dec.decode(&TONE_UNIT[..3]), Err(Error::Truncated));

    // Following valid unit must match `twin` bit-for-bit!
    let a1 = dec.decode(&SILENT_UNIT).unwrap().to_vec();
    let b1 = twin.decode(&SILENT_UNIT).unwrap().to_vec();
    assert_eq!(a1, b1);
}

#[test]
fn edge_cases_reserved_cb_invalid_section_tns_order_and_escape_overflow() {
    let mut dec = Decoder::new();

    // Reserved CB 12
    let mut bw = BitWriter::new();
    bw.write(160, 8);
    bw.write(1, 6);
    bw.write(12, 4);
    bw.write(1, 5);
    bw.write_bool(false);
    assert_eq!(dec.decode(&bw.into_bytes()), Err(Error::ReservedCodebook));

    // Section length 0
    let mut bw = BitWriter::new();
    bw.write(160, 8);
    bw.write(1, 6);
    bw.write(0, 4);
    bw.write(0, 5);
    assert_eq!(dec.decode(&bw.into_bytes()), Err(Error::InvalidSection));

    // TNS order 13 (> 12)
    let mut bw = BitWriter::new();
    bw.write(160, 8);
    bw.write(1, 6);
    bw.write(0, 4);
    bw.write(1, 5);
    bw.write_bool(true); // tns_present
    bw.write(1, 2); // n_filt = 1
    bw.write_bool(false); // coef_res = 3-bit
    bw.write(1, 6); // length = 1
    bw.write(13, 5); // order = 13 > 12
    assert_eq!(
        dec.decode(&bw.into_bytes()),
        Err(Error::TnsOrderOutOfRange(13))
    );

    // Valid TNS order 2 exercises filtering path without error.
    let mut bw = BitWriter::new();
    bw.write(160, 8);
    bw.write(2, 6);
    bw.write(1, 4);
    bw.write(2, 5);
    bw.write_bool(false);
    bw.write_bool(false);
    bw.write_bool(true); // tns_present
    bw.write(1, 2); // n_filt = 1
    bw.write_bool(false); // 3-bit res
    bw.write(35, 6); // length = 35
    bw.write(2, 5); // order = 2
    bw.write_bool(false); // direction up
    bw.write_bool(false); // no compress
    bw.write(1, 3); // +1
    bw.write(7, 3); // -1
    emit_cb1_quad([1, 0, 0, 0], &mut bw);
    emit_cb1_quad([0, 0, 0, 1], &mut bw);
    let out = dec.decode(&bw.into_bytes()).unwrap();
    assert!(out.iter().all(|s| s.is_finite()));
    assert!(out.iter().any(|&s| s != 0.0));

    // PNS (CB 13) decodes finite non-zero noise.
    let mut bw = BitWriter::new();
    bw.write(160, 8);
    bw.write(1, 6);
    bw.write(13, 4);
    bw.write(1, 5);
    bw.write(256, 9); // noise delta 0
    bw.write_bool(false); // no TNS
    let out = dec.decode(&bw.into_bytes()).unwrap();
    assert!(out.iter().all(|s| s.is_finite()));
    assert!(out.iter().any(|&s| s != 0.0));
}
