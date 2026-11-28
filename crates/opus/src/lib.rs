//! Clean-room MIT Opus CELT wideband 20 ms mono (`TOC = 0xb8`) decoder with PLC, from RFC 6716.

#![forbid(unsafe_code)]

mod alloc;
mod cwrs;
mod energy;
mod mdct;
mod plc;
mod postfilter;
mod pvq;
mod range;
mod tables;
mod tf;

use std::fmt;

use energy::ENERGY_FLOOR;
use mdct::N_LONG;
use plc::PlcState;
use postfilter::{PostfilterParams, PostfilterState};
use range::RangeDecoder;
use tables::{
    BITRES, EBANDS, M_BLOCKS, OVERLAP_48K, SPREAD_ICDF, TAPSET_ICDF, TF_SELECT_LM3, WB_BANDS,
};

/// The only Opus TOC byte accepted: config 23 (CELT-only, wideband, 20 ms), mono, code 0 (RFC 6716 Section 3.1).
pub const TOC_CELT_WIDEBAND_20MS_MONO: u8 = 0xb8;
/// Maximum accepted Opus packet length in bytes (matches the AirPods `DoAP` dictation frame cap).
pub const MAX_PACKET_BYTES: usize = 94;
/// Samples decoded per 20 ms frame at 48 kHz.
pub const SAMPLES_48K: usize = 960;
/// Samples decoded per 20 ms frame at 16 kHz.
pub const SAMPLES_16K: usize = 320;

/// Supported output sample rates for [`Decoder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputRate {
    /// 16 kHz mono (320 samples per 20 ms frame).
    Hz16000,
    /// 48 kHz mono (960 samples per 20 ms frame).
    Hz48000,
}

impl OutputRate {
    /// Sample rate in Hertz.
    #[must_use]
    pub const fn hz(self) -> u32 {
        match self {
            Self::Hz16000 => 16_000,
            Self::Hz48000 => 48_000,
        }
    }

    /// Number of mono PCM samples produced per 20 ms frame.
    #[must_use]
    pub const fn frame_samples(self) -> usize {
        match self {
            Self::Hz16000 => SAMPLES_16K,
            Self::Hz48000 => SAMPLES_48K,
        }
    }
}

/// Errors returned by [`Decoder::decode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Input packet was empty.
    EmptyPacket,
    /// Input packet exceeded [`MAX_PACKET_BYTES`].
    OversizedPacket(usize),
    /// First byte was not [`TOC_CELT_WIDEBAND_20MS_MONO`] (`0xb8`).
    UnsupportedToc(u8),
    /// Synthesized audio contained a non-finite sample.
    NonFiniteOutput,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPacket => f.write_str("empty Opus packet"),
            Self::OversizedPacket(len) => {
                write!(
                    f,
                    "Opus packet length {len} exceeds limit {MAX_PACKET_BYTES}"
                )
            }
            Self::UnsupportedToc(toc) => {
                write!(f, "unsupported Opus TOC byte {toc:#04x}, expected 0xb8")
            }
            Self::NonFiniteOutput => f.write_str("non-finite synthesized output"),
        }
    }
}

impl std::error::Error for Error {}

/// Preallocated, zero-allocation-per-frame RFC 6716 CELT wideband 20 ms mono decoder.
#[derive(Debug, Clone)]
pub struct Decoder {
    rate: OutputRate,
    old_e: [f64; WB_BANDS],
    prev1_e: [f64; WB_BANDS],
    prev2_e: [f64; WB_BANDS],
    overlap: [f64; OVERLAP_48K],
    postfilter: PostfilterState,
    plc: PlcState,
    rng_seed: u32,
    spec: [f64; N_LONG],
    time_buf: [f64; N_LONG],
    out_48k: [f32; SAMPLES_48K],
    out_16k: [f32; SAMPLES_16K],
}

impl Decoder {
    /// Creates a new decoder at `rate` and eagerly initializes all lookup tables.
    #[must_use]
    pub fn new(rate: OutputRate) -> Self {
        tables::warmup();
        mdct::warmup();
        cwrs::warmup();
        Self {
            rate,
            old_e: [0.0; WB_BANDS],
            prev1_e: [ENERGY_FLOOR; WB_BANDS],
            prev2_e: [ENERGY_FLOOR; WB_BANDS],
            overlap: [0.0; OVERLAP_48K],
            postfilter: PostfilterState::new(),
            plc: PlcState::new(),
            rng_seed: 0,
            spec: [0.0; N_LONG],
            time_buf: [0.0; N_LONG],
            out_48k: [0.0; SAMPLES_48K],
            out_16k: [0.0; SAMPLES_16K],
        }
    }

    /// Creates a 48 kHz mono decoder (convenience wrapper around [`Self::new`]).
    #[must_use]
    pub fn new_48k() -> Self {
        Self::new(OutputRate::Hz48000)
    }

    /// Resets all persistent energy, overlap, post-filter, and PLC state.
    pub fn reset(&mut self) {
        self.old_e.fill(0.0);
        self.prev1_e.fill(ENERGY_FLOOR);
        self.prev2_e.fill(ENERGY_FLOOR);
        self.overlap.fill(0.0);
        self.postfilter.reset();
        self.plc.reset();
        self.rng_seed = 0;
        self.out_48k.fill(0.0);
        self.out_16k.fill(0.0);
    }

    /// Returns the number of samples produced by each [`Self::decode`] or [`Self::conceal`] call.
    #[must_use]
    pub const fn frame_samples(&self) -> usize {
        self.rate.frame_samples()
    }

    /// Decodes one `0xb8` packet (e.g. `[0xb8, 0x7f, 0x54, ...]` of 1..=94 bytes) without allocating; any error leaves decoder state untouched.
    pub fn decode(&mut self, packet: &[u8]) -> Result<&[f32], Error> {
        let Some((&toc, payload)) = packet.split_first() else {
            return Err(Error::EmptyPacket);
        };
        if packet.len() > MAX_PACKET_BYTES {
            return Err(Error::OversizedPacket(packet.len()));
        }
        if toc != TOC_CELT_WIDEBAND_20MS_MONO {
            return Err(Error::UnsupportedToc(toc));
        }

        // A 1-byte packet (`[0xb8]` with zero payload bytes) runs DTX/PLC per RFC 6716 Section 3.2.1.
        if payload.is_empty() {
            self.plc.try_conceal(
                &mut self.postfilter,
                &mut self.overlap,
                &mut self.old_e,
                &mut self.out_48k,
            )?;
            self.prev2_e = self.prev1_e;
            self.prev1_e = self.old_e;
            return Ok(self.select_output_slice());
        }

        self.decode_celt_frame(payload)?;
        Ok(self.select_output_slice())
    }

    /// Synthesizes one concealed 20 ms frame for a lost packet without allocating (RFC 6716 Section 4.4).
    pub fn conceal(&mut self) -> &[f32] {
        if self
            .plc
            .try_conceal(
                &mut self.postfilter,
                &mut self.overlap,
                &mut self.old_e,
                &mut self.out_48k,
            )
            .is_ok()
        {
            self.prev2_e = self.prev1_e;
            self.prev1_e = self.old_e;
        }
        self.select_output_slice()
    }

    /// Decodes a silence frame: drains overlap and commits state only after finite `f32` PCM synthesis.
    fn decode_silence(&mut self, next_rng: u32) -> Result<(), Error> {
        self.time_buf.fill(0.0);
        if let Some(head) = self.time_buf.get_mut(..OVERLAP_48K) {
            head.copy_from_slice(&self.overlap);
        }
        if !self
            .time_buf
            .iter()
            .all(|v| v.is_finite() && v.abs() < 1e30)
        {
            return Err(Error::NonFiniteOutput);
        }
        self.postfilter.try_apply(
            &self.time_buf,
            PostfilterParams::default(),
            &mut self.out_48k,
        )?;
        self.plc.on_valid_frame();
        self.prev2_e = self.prev1_e;
        self.prev1_e.fill(ENERGY_FLOOR);
        self.old_e.fill(ENERGY_FLOOR);
        self.overlap.fill(0.0);
        self.rng_seed = next_rng;
        Ok(())
    }

    /// Decodes one CELT WB 20 ms mono payload (sample input: 14..=93 bytes after TOC `0xb8`, e.g. `[0x7f, 0x54, 0xce, 0xa7, ...]`) and commits state only after finite synthesis.
    fn decode_celt_frame(&mut self, payload: &[u8]) -> Result<(), Error> {
        let mut ec = RangeDecoder::new(payload);
        let total_bits = ec.storage_bits();
        let total_bits_frac = total_bits << BITRES;

        // 1. Silence flag (RFC 6716 Table 56); present whenever `total_bits >= 16` (`payload.len() >= 2`).
        let silence = if total_bits >= 16 {
            ec.dec_bit_logp(15) != 0
        } else {
            false
        };
        if silence {
            return self.decode_silence(ec.rng());
        }

        // 2. Post-filter parameters (RFC 6716 Section 4.3.7.1).
        let pf = decode_postfilter(&mut ec, total_bits);

        // 3. Transient and intra flags (RFC 6716 Sections 4.3.1-4.3.2).
        let is_transient = if ec.tell().saturating_add(3) <= total_bits {
            ec.dec_bit_logp(3) != 0
        } else {
            false
        };
        let intra = if ec.tell().saturating_add(3) <= total_bits {
            ec.dec_bit_logp(3) != 0
        } else {
            false
        };

        // 4. Coarse energy decoding into staged energy history (RFC 6716 Section 4.3.2.1).
        let mut next_old_e = self.old_e;
        energy::decode_coarse(&mut ec, intra, &mut next_old_e, total_bits);

        // 5. Per-band TF change and `tf_select` (RFC 6716 Sections 4.3.1 & 4.3.4.5).
        let tf_res = decode_tf(&mut ec, is_transient, total_bits);

        // 6. Spreading parameter (RFC 6716 Table 56 & Section 4.3.4.3).
        let spread = if ec.tell().saturating_add(5) <= total_bits {
            ec.dec_icdf(&SPREAD_ICDF, 5)
        } else {
            2
        };

        // 7. Dynamic allocation boosts and trim (RFC 6716 Section 4.3.3).
        let (offsets, total_boost, skip_start) = alloc::decode_dynalloc(&mut ec, total_bits_frac);
        let trim = alloc::decode_trim(&mut ec, total_bits_frac, total_boost);

        // 8. Bit allocation (RFC 6716 Section 4.3.3).
        let allocation = alloc::compute_allocation(
            &mut ec,
            total_bits_frac,
            &offsets,
            skip_start,
            trim,
            is_transient,
        );

        // 9. Fine energy bits (RFC 6716 Section 4.3.2.2).
        energy::decode_fine(&mut ec, &mut next_old_e, &allocation.fine_quant);

        // 10. PVQ shape decoding (RFC 6716 Section 4.3.4); only wideband bins `0..320` are active.
        if let Some(active) = self.spec.get_mut(..320) {
            active.fill(0.0);
        }
        let mut collapse_masks = [0_u8; WB_BANDS];
        let mut next_seed = self.rng_seed;
        pvq::decode_all_bands(
            &mut ec,
            allocation.coded_bands,
            &allocation.pulses,
            allocation.balance,
            &tf_res,
            spread,
            is_transient,
            total_bits_frac,
            allocation.anti_collapse_rsv,
            &mut self.spec,
            &mut collapse_masks,
            &mut next_seed,
        );

        // 11. Anti-collapse flag (RFC 6716 Section 4.3.5).
        let anti_collapse_on = if allocation.anti_collapse_rsv > 0 {
            ec.dec_bits(1) != 0
        } else {
            false
        };

        // 12. Energy finalization using leftover bits (RFC 6716 Section 4.3.2.2).
        energy::decode_finalise(
            &mut ec,
            &mut next_old_e,
            &allocation.fine_quant,
            &allocation.fine_priority,
            total_bits,
        );

        // 13. Anti-collapse synthesis uses the pre-update LCG seed and prior non-transient energies (RFC 6716 Section 4.3.5).
        if anti_collapse_on {
            pvq::anti_collapse(
                &mut self.spec,
                &collapse_masks,
                &allocation.pulses,
                &next_old_e,
                &self.prev1_e,
                &self.prev2_e,
                &mut next_seed,
            );
        }

        // 14. Denormalization, IMDCT, post-filter, finiteness check, and atomic state commit (RFC 6716 Sections 4.3.6-4.3.7).
        self.synthesize_and_commit(&next_old_e, is_transient, pf, ec.rng())
    }

    /// Synthesizes `self.spec`, validates finite `f32` PCM via post-filter/de-emphasis, and commits state only on success.
    fn synthesize_and_commit(
        &mut self,
        next_old_e: &[f64; WB_BANDS],
        is_transient: bool,
        pf: PostfilterParams,
        next_rng: u32,
    ) -> Result<(), Error> {
        denormalize_spec(next_old_e, &mut self.spec);
        let mut next_overlap = self.overlap;
        mdct::synthesize(
            &self.spec,
            is_transient,
            &mut next_overlap,
            &mut self.time_buf,
        );
        if !self
            .time_buf
            .iter()
            .all(|v| v.is_finite() && v.abs() < 1e30)
        {
            return Err(Error::NonFiniteOutput);
        }
        self.postfilter
            .try_apply(&self.time_buf, pf, &mut self.out_48k)?;
        self.plc.on_valid_frame();
        if is_transient {
            // RFC 6716 Section 4.3.5: transient frames take the elementwise minimum into `oldLogE` without shifting `oldLogE2`.
            for (dst, &cur) in self.prev1_e.iter_mut().zip(next_old_e.iter()) {
                *dst = dst.min(cur);
            }
        } else {
            self.prev2_e = self.prev1_e;
            self.prev1_e = *next_old_e;
        }
        self.old_e = *next_old_e;
        self.overlap = next_overlap;
        self.rng_seed = next_rng;
        Ok(())
    }

    /// Selects 48 kHz output or decimates the bandlimited 0..8 kHz signal by 3 for 16 kHz.
    fn select_output_slice(&mut self) -> &[f32] {
        match self.rate {
            OutputRate::Hz48000 => &self.out_48k,
            OutputRate::Hz16000 => {
                // Wideband CELT is bandlimited strictly below 8 kHz (bin 320), so 3:1 decimation is alias-free.
                for (i, dst) in self.out_16k.iter_mut().enumerate() {
                    *dst = self.out_48k.get(i.wrapping_mul(3)).copied().unwrap_or(0.0);
                }
                &self.out_16k
            }
        }
    }
}

/// Decodes pitch post-filter header from range coder (sample input: `flag=1, octave=2, fine_pitch=17, qg=3, tapset=0` -> `period=80, gain=0.375`).
fn decode_postfilter(ec: &mut RangeDecoder<'_>, total_bits: i32) -> PostfilterParams {
    if ec.tell().saturating_add(16) > total_bits || ec.dec_bit_logp(1) == 0 {
        return PostfilterParams::default();
    }
    let octave = ec.dec_uint(6);
    let fine_pitch = ec.dec_bits(octave.wrapping_add(4));
    let period = (16_u32 << octave)
        .wrapping_add(fine_pitch)
        .saturating_sub(1);
    let qg = ec.dec_bits(3);
    let tapset = if ec.tell().saturating_add(2) <= total_bits {
        ec.dec_icdf(&TAPSET_ICDF, 2)
    } else {
        0
    };
    PostfilterParams {
        period: usize::try_from(period).unwrap_or(15),
        gain: 0.09375 * f64::from(qg.wrapping_add(1)),
        tapset,
    }
}

/// Scales normalized PVQ band shapes by their linear band gains (RFC 6716 Section 4.3.6).
fn denormalize_spec(old_e: &[f64; WB_BANDS], spec: &mut [f64; N_LONG]) {
    let mut lin_e = [0.0_f64; WB_BANDS];
    energy::log_to_linear(old_e, &mut lin_e);
    for band in 0..WB_BANDS {
        let g = lin_e.get(band).copied().unwrap_or(0.0);
        let start = EBANDS
            .get(band)
            .copied()
            .unwrap_or(0)
            .wrapping_mul(M_BLOCKS);
        let end = EBANDS
            .get(band.wrapping_add(1))
            .copied()
            .unwrap_or(0)
            .wrapping_mul(M_BLOCKS);
        if let Some(slice) = spec.get_mut(start..end) {
            for v in slice.iter_mut() {
                *v *= g;
            }
        }
    }
}

/// Decodes 17 per-band TF toggles and optional `tf_select` from range coder (sample input: change bits `[0, 1, 0, ..., 0]` and `tf_select = 1`).
fn decode_tf(ec: &mut RangeDecoder<'_>, is_transient: bool, total_bits: i32) -> [i8; WB_BANDS] {
    let mut tf_res = [0_i8; WB_BANDS];
    let mut tf_changed = 0_usize;
    let mut curr = 0_usize;
    let tf_select_rsv = ec.tell().saturating_add(1) <= total_bits;
    let budget = if tf_select_rsv {
        total_bits.saturating_sub(1)
    } else {
        total_bits
    };
    let mut logp = if is_transient { 2_u32 } else { 4_u32 };
    for band in 0..WB_BANDS {
        if ec.tell().saturating_add(i32::try_from(logp).unwrap_or(4)) <= budget {
            curr ^= usize::try_from(ec.dec_bit_logp(logp)).unwrap_or(0);
            tf_changed |= curr;
        }
        if let Some(dst) = tf_res.get_mut(band) {
            *dst = i8::try_from(curr).unwrap_or(0);
        }
        logp = if is_transient { 4 } else { 5 };
    }
    let row0 = usize::from(is_transient).wrapping_mul(2);
    let row1 = row0.wrapping_add(1);
    let can_differ = TF_SELECT_LM3
        .get(row0)
        .and_then(|r| r.get(tf_changed))
        .copied()
        != TF_SELECT_LM3
            .get(row1)
            .and_then(|r| r.get(tf_changed))
            .copied();
    let tf_select = if tf_select_rsv && can_differ {
        usize::try_from(ec.dec_bit_logp(1)).unwrap_or(0)
    } else {
        0
    };
    let table_row = TF_SELECT_LM3
        .get(row0.wrapping_add(tf_select))
        .copied()
        .unwrap_or([0, 0]);
    for dst in &mut tf_res {
        let idx = usize::from(*dst != 0);
        *dst = table_row.get(idx).copied().unwrap_or(0);
    }
    tf_res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_api_smoke() {
        assert_eq!(OutputRate::Hz48000.hz(), 48_000);
        assert_eq!(OutputRate::Hz16000.hz(), 16_000);
        assert_eq!(OutputRate::Hz48000.frame_samples(), SAMPLES_48K);
        assert_eq!(OutputRate::Hz16000.frame_samples(), SAMPLES_16K);

        let mut dec = Decoder::new_48k();
        assert_eq!(dec.frame_samples(), SAMPLES_48K);
        assert_eq!(dec.decode(&[]), Err(Error::EmptyPacket));
        assert_eq!(
            dec.decode(&[TOC_CELT_WIDEBAND_20MS_MONO; MAX_PACKET_BYTES + 1]),
            Err(Error::OversizedPacket(MAX_PACKET_BYTES + 1))
        );
        assert_eq!(dec.decode(&[0xf8, 0x00]), Err(Error::UnsupportedToc(0xf8)));
        assert_eq!(
            dec.decode(&[TOC_CELT_WIDEBAND_20MS_MONO]).map(<[f32]>::len),
            Ok(SAMPLES_48K)
        );
        assert_eq!(dec.conceal().len(), SAMPLES_48K);
        dec.reset();
    }

    fn assert_persistent_state_eq(a: &Decoder, b: &Decoder) {
        assert_eq!(a.old_e, b.old_e);
        assert_eq!(a.prev1_e, b.prev1_e);
        assert_eq!(a.prev2_e, b.prev2_e);
        for (x, y) in a.overlap.iter().zip(b.overlap.iter()) {
            assert_eq!(x.to_bits(), y.to_bits());
        }
        assert_eq!(a.rng_seed, b.rng_seed);
        assert_eq!(a.plc.state_for_test(), b.plc.state_for_test());
        assert_eq!(a.postfilter.history(), b.postfilter.history());
        assert_eq!(a.postfilter.state_for_test(), b.postfilter.state_for_test());
        for (x, y) in a.out_48k.iter().zip(b.out_48k.iter()) {
            assert_eq!(x.to_bits(), y.to_bits());
        }
        for (x, y) in a.out_16k.iter().zip(b.out_16k.iter()) {
            assert_eq!(x.to_bits(), y.to_bits());
        }
    }

    #[test]
    fn late_non_finite_output_leaves_persistent_state_untouched() {
        let pkt1: &[u8] = &[
            0xb8, 0x7f, 0x54, 0xce, 0xa7, 0x4d, 0x11, 0xf1, 0x21, 0x86, 0xed, 0x67, 0x4e, 0x14,
            0x34,
        ];
        let pkt2: &[u8] = &[
            0xb8, 0x70, 0xb1, 0xfd, 0x52, 0x0c, 0x4e, 0xaa, 0x17, 0x0e, 0xc7, 0x4d, 0xe1, 0x6e,
            0x0a,
        ];
        let silence: &[u8] = &[TOC_CELT_WIDEBAND_20MS_MONO, 0xff, 0xfe];
        let dtx: &[u8] = &[TOC_CELT_WIDEBAND_20MS_MONO];

        // Public decode across normal, silence, and DTX at both output rates must preserve all state and allocate 0 bytes on late f32 overflow.
        for rate in [OutputRate::Hz48000, OutputRate::Hz16000] {
            let mut dec = Decoder::new(rate);
            assert!(dec.decode(pkt1).is_ok());
            let saved_mem = dec.postfilter.deemph_mem_for_test();
            for pkt in [pkt2, silence, dtx] {
                let mut poisoned = dec.clone();
                let mut twin = dec.clone();
                poisoned.postfilter.set_deemph_mem_for_test(1e44_f64);
                let snapshot = poisoned.clone();
                let stats = allocation_counter::measure(|| {
                    assert_eq!(poisoned.decode(pkt), Err(Error::NonFiniteOutput));
                });
                assert_eq!(stats.count_total, 0);
                assert_persistent_state_eq(&poisoned, &snapshot);
                poisoned.postfilter.set_deemph_mem_for_test(saved_mem);
                assert_eq!(poisoned.decode(pkt2), twin.decode(pkt2));
            }
        }

        // Pre-commit non-finite spectrum and overlap guards also preserve all persistent fields.
        let mut dec = Decoder::new_48k();
        assert!(dec.decode(pkt1).is_ok());
        let mut bad_spec = dec.clone();
        let mut twin = dec.clone();
        bad_spec.spec[0] = f64::NAN;
        assert_eq!(
            bad_spec.synthesize_and_commit(
                &[1.0; WB_BANDS],
                false,
                PostfilterParams::default(),
                0xdead_beef,
            ),
            Err(Error::NonFiniteOutput)
        );
        assert_persistent_state_eq(&bad_spec, &dec);
        assert_eq!(bad_spec.decode(pkt2), twin.decode(pkt2));

        let mut bad_sil = dec.clone();
        bad_sil.overlap[0] = f64::NAN;
        let saved_sil = bad_sil.clone();
        assert_eq!(
            bad_sil.decode_silence(0x1234_5678),
            Err(Error::NonFiniteOutput)
        );
        assert_persistent_state_eq(&bad_sil, &saved_sil);

        let mut bad_dtx = dec;
        bad_dtx.overlap[0] = f64::NAN;
        let saved_dtx = bad_dtx.clone();
        assert_eq!(bad_dtx.decode(dtx), Err(Error::NonFiniteOutput));
        assert_persistent_state_eq(&bad_dtx, &saved_dtx);

        let control = allocation_counter::measure(|| drop(std::hint::black_box(vec![0_u8; 64])));
        assert_eq!(control.count_total, 1);
    }
}
