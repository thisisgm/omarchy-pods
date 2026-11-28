//! RFC 6716 Section 4.3.2 CELT band energy envelope decoding (coarse, fine, finalization).

use crate::range::RangeDecoder;
use crate::tables::{
    E_MEANS, E_PROB_MODEL_INTER, E_PROB_MODEL_INTRA, MAX_FINE_BITS, SMALL_ENERGY_ICDF, WB_BANDS,
};

/// Inter-frame time prediction coefficient for 20 ms (`LM = 3`, `16384 / 32768`, RFC 6716 Section 4.3.2.1).
const ALPHA_INTER_LM3: f64 = 16384.0 / 32768.0;
/// Inter-frame frequency prediction coefficient for 20 ms (`LM = 3`, `6554 / 32768`, RFC 6716 Section 4.3.2.1).
const BETA_INTER_LM3: f64 = 6554.0 / 32768.0;
/// Intra-frame frequency prediction coefficient (`4915 / 32768`, RFC 6716 Section 4.3.2.1).
const BETA_INTRA: f64 = 4915.0 / 32768.0;
/// Minimum clamped previous energy fed into temporal prediction (RFC 6716 Section 4.3.2.1).
const PRED_FLOOR: f64 = -9.0;
/// Minimum stored band log-energy floor in 6 dB units (RFC 6716 Section 4.3.2.1).
pub(crate) const ENERGY_FLOOR: f64 = -28.0;
/// Upper cap on log-domain band energy before exponentiation (RFC 8251 Section 8).
const ENERGY_CAP: f64 = 32.0;

/// Decodes 17 coarse band energies from range coder (sample input: Laplace deltas `[2, -1, 0, 1, ..., 0]` in 6 dB steps, or 2-bit/1-bit low-budget codes).
pub(crate) fn decode_coarse(
    ec: &mut RangeDecoder<'_>,
    intra: bool,
    old_e: &mut [f64; WB_BANDS],
    budget_bits: i32,
) {
    let (alpha, beta, model) = if intra {
        (0.0, BETA_INTRA, &E_PROB_MODEL_INTRA)
    } else {
        (ALPHA_INTER_LM3, BETA_INTER_LM3, &E_PROB_MODEL_INTER)
    };
    let mut prev = 0.0_f64;
    for band in 0..WB_BANDS {
        let tell = ec.tell();
        let rem = budget_bits.saturating_sub(tell);
        let qi = if rem >= 15 {
            let (p0, decay) = model.get(band).copied().unwrap_or((128, 64));
            ec.dec_laplace(u32::from(p0) << 7, u32::from(decay) << 6)
        } else if rem >= 2 {
            match ec.dec_icdf(&SMALL_ENERGY_ICDF, 2) {
                0 => 0,
                1 => -1,
                _ => 1,
            }
        } else if rem == 1 {
            if ec.dec_bit_logp(1) == 0 { 0 } else { -1 }
        } else {
            -1
        };
        let q = f64::from(qi);
        let prev_e = old_e.get(band).copied().unwrap_or(0.0).max(PRED_FLOOR);
        let new_e = (alpha * prev_e + prev + q).max(ENERGY_FLOOR);
        if let Some(dst) = old_e.get_mut(band) {
            *dst = new_e;
        }
        prev += q - beta * q;
    }
}

/// Decodes explicit fine energy refinement bits per band (RFC 6716 Section 4.3.2.2).
pub(crate) fn decode_fine(
    ec: &mut RangeDecoder<'_>,
    old_e: &mut [f64; WB_BANDS],
    fine_quant: &[i32; WB_BANDS],
) {
    for band in 0..WB_BANDS {
        let bits = fine_quant.get(band).copied().unwrap_or(0);
        if bits <= 0 {
            continue;
        }
        let u_bits = u32::try_from(bits.min(MAX_FINE_BITS)).unwrap_or(0);
        let q2 = ec.dec_bits(u_bits);
        let denom = f64::from(1_u32 << u_bits);
        let offset = (f64::from(q2) + 0.5) / denom - 0.5;
        if let Some(dst) = old_e.get_mut(band) {
            *dst += offset;
        }
    }
}

/// Uses remaining frame bits for final 1-bit energy refinements by priority (RFC 6716 Section 4.3.2.2).
pub(crate) fn decode_finalise(
    ec: &mut RangeDecoder<'_>,
    old_e: &mut [f64; WB_BANDS],
    fine_quant: &[i32; WB_BANDS],
    fine_priority: &[u8; WB_BANDS],
    budget_bits: i32,
) {
    for prio in 0..2_u8 {
        for band in 0..WB_BANDS {
            let fq = fine_quant.get(band).copied().unwrap_or(0);
            let fp = fine_priority.get(band).copied().unwrap_or(1);
            if fq >= MAX_FINE_BITS || fp != prio {
                continue;
            }
            if budget_bits.saturating_sub(ec.tell()) < 1 {
                return;
            }
            let q2 = ec.dec_bits(1);
            let shift = u32::try_from(fq.clamp(0, MAX_FINE_BITS))
                .unwrap_or(0)
                .wrapping_add(1);
            let step = 0.5 / f64::from(1_u32 << shift);
            let offset = if q2 != 0 { step } else { -step };
            if let Some(dst) = old_e.get_mut(band) {
                *dst += offset;
            }
        }
    }
}

/// Converts log2 band energies to linear MDCT band amplitudes capped per RFC 8251 Section 8.
pub(crate) fn log_to_linear(old_e: &[f64; WB_BANDS], out: &mut [f64; WB_BANDS]) {
    for band in 0..WB_BANDS {
        let log_e = old_e.get(band).copied().unwrap_or(ENERGY_FLOOR);
        let mean = E_MEANS.get(band).copied().unwrap_or(0.0);
        let total = (log_e + mean).min(ENERGY_CAP);
        if let Some(dst) = out.get_mut(band) {
            *dst = total.exp2();
        }
    }
}
