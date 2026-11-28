//! RFC 6716 Sections 4.3.4.4 and 4.3.5 recursive PVQ band splitting, folding, and anti-collapse.

use crate::cwrs::{
    CwrsTable, MAX_BAND_N, bits_to_pulses, celt_lcg, cwrs_table, decode_pulses, max_direct_bits,
    renormalise_vector,
};
use crate::range::{RangeDecoder, ilog};
use crate::tables::{
    BITRES, CeltTables, EBANDS, EXP2_TABLE8, LM, LOG_N, M_BLOCKS, WB_BANDS, base_width_slot,
    pseudo_to_pulses, tables,
};
use crate::tf::{apply_tf_hadamard, exp_rotation, interleave_hadamard, invert_tf_hadamard};

/// Bit-exact Q15 cosine approximation for angle `x` in `0..=16384` (0..pi/2).
#[inline]
const fn bitexact_cos(x: i32) -> i32 {
    let x2 = (4096_i32.wrapping_add(x.wrapping_mul(x))) >> 13;
    let t1 = ((-626_i32).wrapping_mul(x2).wrapping_add(16384)) >> 15;
    let t2 = (x2
        .wrapping_mul(8277_i32.wrapping_add(t1))
        .wrapping_add(16384))
        >> 15;
    let t3 = (x2
        .wrapping_mul((-7651_i32).wrapping_add(t2))
        .wrapping_add(16384))
        >> 15;
    1_i32.wrapping_add((32767_i32.wrapping_sub(x2)).wrapping_add(t3))
}

/// Fixed-point Q11 approximation of `log2(imid / iside)` for split bit allocation.
#[inline]
fn bitexact_log2tan(iside: i32, imid: i32) -> i32 {
    let lc = i32::try_from(ilog(u32::try_from(imid.max(1)).unwrap_or(1))).unwrap_or(1);
    let ls = i32::try_from(ilog(u32::try_from(iside.max(1)).unwrap_or(1))).unwrap_or(1);
    let ic = imid << (15_i32.saturating_sub(lc));
    let is = iside << (15_i32.saturating_sub(ls));
    let fc = (ic
        .wrapping_mul(
            7932_i32.wrapping_add(((-2597_i32).wrapping_mul(ic).wrapping_add(16384)) >> 15),
        )
        .wrapping_add(16384))
        >> 15;
    let fs = (is
        .wrapping_mul(
            7932_i32.wrapping_add(((-2597_i32).wrapping_mul(is).wrapping_add(16384)) >> 15),
        )
        .wrapping_add(16384))
        >> 15;
    ((lc.wrapping_sub(ls)) << 11).wrapping_add(fc.wrapping_sub(fs))
}

/// Computes split-angle quantization resolution `qn` (`1..=256`, even when `> 1`) per RFC 6716 Section 4.3.4.4.
#[inline]
fn compute_qn(half_dim: usize, budget: i32, pulse_cap: i32) -> u32 {
    let dof = i32::try_from(half_dim.wrapping_mul(2).saturating_sub(1))
        .unwrap_or(1)
        .max(1);
    let offset = (pulse_cap >> 1).saturating_sub(8);
    let mut qb = budget.checked_div(dof).unwrap_or(0).saturating_add(offset);
    let cap_lim = budget.saturating_sub(pulse_cap).saturating_sub(4 << BITRES);
    qb = qb.min(cap_lim).min(8 << BITRES);
    if qb < (1 << (BITRES.wrapping_sub(1))) {
        return 1;
    }
    let shift = 14_u32.saturating_sub(u32::try_from(qb >> BITRES).unwrap_or(0));
    let frac = usize::try_from(qb & 7).unwrap_or(0);
    let base = EXP2_TABLE8.get(frac).copied().unwrap_or(16384);
    let qn_raw = base >> shift;
    let qn_even = (qn_raw.saturating_add(1)) & !1_u32;
    qn_even.clamp(2, 256)
}

/// Decodes split angle `itheta` in `0..=qn` from range coder (sample input: uniform symbol `3` in `0..=8` when `blocks > 1`, or `fm = 11` over `ft = 25` when `qn = 8`, `blocks = 1`).
fn decode_itheta(ec: &mut RangeDecoder<'_>, qn: u32, blocks: usize) -> u32 {
    if qn <= 1 {
        return 0;
    }
    if blocks > 1 {
        return ec.dec_uint(qn.wrapping_add(1));
    }
    let half = (qn >> 1).wrapping_add(1);
    let ft = half.wrapping_mul(half);
    let fm = ec.decode(ft);
    let (itheta, fl, fh) = if fm < (half.wrapping_mul(half.wrapping_add(1)) >> 1) {
        // Rising ramp: `fl(k) = k*(k+1)/2`, width `k + 1`.
        let mut k = 0_u32;
        while (k.wrapping_add(1)).wrapping_mul(k.wrapping_add(2)) >> 1 <= fm {
            k = k.wrapping_add(1);
        }
        let fl = k.wrapping_mul(k.wrapping_add(1)) >> 1;
        (k, fl, fl.wrapping_add(k.wrapping_add(1)))
    } else {
        // Falling ramp: symmetric from `ft`.
        let rem = ft.wrapping_sub(1).wrapping_sub(fm);
        let mut r = 0_u32;
        while (r.wrapping_add(1)).wrapping_mul(r.wrapping_add(2)) >> 1 <= rem {
            r = r.wrapping_add(1);
        }
        let k = qn.wrapping_sub(r);
        let fh = ft.saturating_sub(r.wrapping_mul(r.wrapping_add(1)) >> 1);
        let fl = fh.saturating_sub(r.wrapping_add(1));
        (k, fl, fh)
    };
    ec.update(fl, fh, ft);
    itheta
}

/// Computes split child budgets `(b1, b2)` and trig gains `(cos_t, sin_t)` from decoded `itheta`.
fn split_budgets(
    itheta: u32,
    qn: u32,
    rem_b: i32,
    half: usize,
    blocks: usize,
    lm: i32,
) -> (i32, i32, f64, f64) {
    let th_q14 = i32::try_from(
        u64::from(itheta)
            .wrapping_mul(16384)
            .checked_div(u64::from(qn.max(1)))
            .unwrap_or(0),
    )
    .unwrap_or(0)
    .clamp(0, 16384);
    let imid = bitexact_cos(th_q14);
    let iside = bitexact_cos(16384_i32.wrapping_sub(th_q14));
    let cos_t = f64::from(imid) / 32768.0;
    let sin_t = f64::from(iside) / 32768.0;
    let (b1, b2) = if itheta == 0 {
        (rem_b, 0)
    } else if itheta == qn {
        (0, rem_b)
    } else {
        let l2_q11 = bitexact_log2tan(iside, imid);
        let mult = i32::try_from(half.saturating_sub(1)).unwrap_or(1);
        let prod = mult.wrapping_mul(l2_q11);
        let mut delta_i = if prod >= 0 {
            prod.wrapping_add(128) >> 8
        } else {
            ((prod.wrapping_neg().wrapping_add(128)) >> 8).wrapping_neg()
        };
        if blocks > 1 {
            if delta_i < 0 {
                let neg_shift = u32::try_from(5_i32.saturating_sub(lm)).unwrap_or(2);
                delta_i =
                    delta_i.wrapping_sub(delta_i.checked_div(1_i32 << neg_shift).unwrap_or(0));
            } else if delta_i > 0 {
                let pos_shift = u32::try_from(3_i32.saturating_sub(lm)).unwrap_or(0);
                let sub = i32::try_from(half).unwrap_or(0) >> pos_shift;
                delta_i = delta_i.saturating_sub(sub).max(0);
            }
        }
        delta_i = delta_i.clamp(rem_b.wrapping_neg(), rem_b);
        let left = ((rem_b.saturating_add(delta_i)) >> 1).clamp(0, rem_b);
        (left, rem_b.saturating_sub(left))
    };
    (b1, b2, cos_t, sin_t)
}

/// Decodes two ordered split halves, donating surplus beyond 3 bits from the first to the second.
#[expect(
    clippy::too_many_arguments,
    reason = "ordered split pair carries tables, both half slices, budgets, gains, and LCG seed"
)]
fn decode_split_pair(
    celt: &CeltTables,
    cwrs: &CwrsTable,
    ec: &mut RangeDecoder<'_>,
    first_x: &mut [f64],
    first_lowband: Option<&[f64]>,
    first_budget: i32,
    first_gain: f64,
    second_x: &mut [f64],
    second_lowband: Option<&[f64]>,
    second_budget: i32,
    second_gain: f64,
    base_slot: usize,
    band: usize,
    remaining_bits: &mut i32,
    sub_blocks: usize,
    next_lm: i32,
    spread: usize,
    seed: &mut u32,
) -> (u8, u8) {
    let rem_before = *remaining_bits;
    let cm_first = quant_partition(
        celt,
        cwrs,
        ec,
        first_x,
        first_lowband,
        base_slot,
        band,
        first_budget,
        remaining_bits,
        sub_blocks,
        next_lm,
        first_gain,
        spread,
        seed,
    );
    let used_first = rem_before.saturating_sub(*remaining_bits);
    let second_adj = if second_budget > 0 {
        let surplus = first_budget
            .saturating_sub(used_first)
            .saturating_sub(3 << BITRES)
            .max(0);
        second_budget.saturating_add(surplus)
    } else {
        0
    };
    let cm_second = quant_partition(
        celt,
        cwrs,
        ec,
        second_x,
        second_lowband,
        base_slot,
        band,
        second_adj,
        remaining_bits,
        sub_blocks,
        next_lm,
        second_gain,
        spread,
        seed,
    );
    (cm_first, cm_second)
}

/// Decodes one recursive split pair and combines their collapse masks.
#[expect(
    clippy::too_many_arguments,
    reason = "recursive PVQ split carries tables, geometry, budgets, lowband folding state, and LCG seed"
)]
fn quant_partition_split(
    celt: &CeltTables,
    cwrs: &CwrsTable,
    ec: &mut RangeDecoder<'_>,
    vec: &mut [f64],
    lowband: Option<&[f64]>,
    base_slot: usize,
    band: usize,
    budget: i32,
    pulse_cap: i32,
    remaining_bits: &mut i32,
    blocks: usize,
    lm: i32,
    gain: f64,
    spread: usize,
    seed: &mut u32,
) -> u8 {
    let dim = vec.len();
    let half = dim >> 1;
    let qn = compute_qn(half, budget, pulse_cap);
    let tell_before = ec.tell_frac();
    let itheta = decode_itheta(ec, qn, blocks);
    let used_it = ec.tell_frac().saturating_sub(tell_before);
    *remaining_bits = remaining_bits.saturating_sub(used_it);
    let rem_b = budget.saturating_sub(used_it).max(0);
    let (b1, b2, cos_t, sin_t) = split_budgets(itheta, qn, rem_b, half, blocks, lm);
    let sub_blocks = if blocks > 1 { blocks >> 1 } else { 1 };
    let (x_lo, x_hi) = vec.split_at_mut(half);
    let (lb_lo, lb_hi) = match lowband {
        Some(lb) if lb.len() >= dim => (lb.get(..half), lb.get(half..dim)),
        _ => (None, None),
    };
    let next_lm = lm.wrapping_sub(1);
    let (cm1, cm2) = if b1 < b2 {
        let (c2, c1) = decode_split_pair(
            celt,
            cwrs,
            ec,
            x_hi,
            lb_hi,
            b2,
            gain * sin_t,
            x_lo,
            lb_lo,
            b1,
            gain * cos_t,
            base_slot,
            band,
            remaining_bits,
            sub_blocks,
            next_lm,
            spread,
            seed,
        );
        (c1, c2)
    } else {
        decode_split_pair(
            celt,
            cwrs,
            ec,
            x_lo,
            lb_lo,
            b1,
            gain * cos_t,
            x_hi,
            lb_hi,
            b2,
            gain * sin_t,
            base_slot,
            band,
            remaining_bits,
            sub_blocks,
            next_lm,
            spread,
            seed,
        )
    };
    if blocks > M_BLOCKS {
        cm1 | cm2
    } else {
        cm1 | (cm2 << (blocks >> 1).min(7))
    }
}

/// Decodes one recursive partition of a band, returning its collapse bitmask (RFC 6716 Section 4.3.4.4).
#[expect(
    clippy::too_many_arguments,
    reason = "RFC 6716 quant_partition carries tables, band geometry, split budget, lowband folding state, and LCG seed"
)]
fn quant_partition(
    celt: &CeltTables,
    cwrs: &CwrsTable,
    ec: &mut RangeDecoder<'_>,
    vec: &mut [f64],
    lowband: Option<&[f64]>,
    base_slot: usize,
    band: usize,
    budget: i32,
    remaining_bits: &mut i32,
    blocks: usize,
    lm: i32,
    gain: f64,
    spread: usize,
    seed: &mut u32,
) -> u8 {
    let dim = vec.len();
    if dim == 0 {
        return 0;
    }
    let pulse_cap = LOG_N
        .get(band)
        .copied()
        .unwrap_or(0)
        .wrapping_add(lm.wrapping_mul(1 << BITRES));
    let direct_cap = max_direct_bits(celt, base_slot, lm);
    // Split recursively when budget exceeds 32-bit PVQ codebook capacity (RFC 6716 Section 4.3.4.4).
    if lm != -1 && dim > 2 && (dim & 1) == 0 && budget >= direct_cap.saturating_add(12) {
        return quant_partition_split(
            celt,
            cwrs,
            ec,
            vec,
            lowband,
            base_slot,
            band,
            budget,
            pulse_cap,
            remaining_bits,
            blocks,
            lm,
            gain,
            spread,
            seed,
        );
    }

    let rem_avail = (*remaining_bits).max(0);
    let q_idx = bits_to_pulses(celt, base_slot, lm, budget, rem_avail);
    let lm_slot = usize::try_from(lm.wrapping_add(1)).unwrap_or(0).min(4);
    let used_cost = celt
        .pulse_rows
        .get(base_slot)
        .and_then(|r| r.get(lm_slot))
        .and_then(|row| row.bits.get(q_idx))
        .copied()
        .unwrap_or(0);
    *remaining_bits = remaining_bits.saturating_sub(used_cost);
    let pulse_cnt = pseudo_to_pulses(q_idx);
    if pulse_cnt > 0 {
        let cm = decode_pulses(cwrs, ec, vec, pulse_cnt, blocks);
        exp_rotation(vec, -1, blocks, pulse_cnt, spread);
        if (gain - 1.0).abs() > 0.0 {
            for sample in vec.iter_mut() {
                *sample *= gain;
            }
        }
        return cm;
    }

    // Zero-gain split half stays zero without advancing the LCG PRNG.
    if gain == 0.0 {
        vec.fill(0.0);
        return 0;
    }
    // Zero pulses: fold from lowband with +/-1/256 dither unless aggressive spread on a single block, else 12-bit noise.
    let mut cm = 0_u8;
    if !(spread == 3 && blocks == 1)
        && let Some(lb) = lowband
        && lb.len() >= dim
    {
        const FOLD_DITHER: f64 = 1.0 / 256.0;
        let slen = dim.checked_div(blocks.max(1)).unwrap_or(1).max(1);
        for (pos, (dst, &src)) in vec.iter_mut().zip(lb.iter().take(dim)).enumerate() {
            if src != 0.0 {
                cm |= 1_u8 << (pos.checked_div(slen).unwrap_or(0) % M_BLOCKS);
            }
            let dither = if (celt_lcg(seed) & 0x8000) != 0 {
                FOLD_DITHER
            } else {
                -FOLD_DITHER
            };
            *dst = src + dither;
        }
    } else {
        for dst in vec.iter_mut() {
            let rnd = celt_lcg(seed);
            let signed = i32::from_le_bytes(rnd.to_le_bytes()) >> 20;
            *dst = f64::from(signed);
        }
    }
    renormalise_vector(vec, gain);
    cm
}

/// Expands a `blocks`-bit collapse mask (`blocks <= 8`) across all `M_BLOCKS = 8` short blocks.
fn expand_collapse_mask(cm: u8, blocks: usize) -> u8 {
    if blocks >= M_BLOCKS || blocks == 0 {
        return cm;
    }
    let repeat = M_BLOCKS.checked_div(blocks).unwrap_or(1);
    let mut out = 0_u8;
    for b in 0..blocks {
        if (cm & (1_u8 << b)) != 0 {
            for r in 0..repeat {
                let bit = b.wrapping_mul(repeat).wrapping_add(r);
                if bit < M_BLOCKS {
                    out |= 1_u8 << bit;
                }
            }
        }
    }
    out
}

/// Computes the number of PVQ sub-blocks for one band from its TF adjustment and transient flag.
#[inline]
fn band_blocks(n: usize, tf_change: i8, is_transient: bool) -> usize {
    if is_transient {
        let shift = usize::from(tf_change.unsigned_abs()).min(LM);
        if tf_change >= 0 {
            M_BLOCKS >> shift
        } else {
            (M_BLOCKS << shift).min(n)
        }
    } else {
        let shift = usize::from(tf_change.min(0).unsigned_abs()).min(LM);
        1_usize << shift
    }
}

/// Prepares TF-aligned lowband folding source in `scratch` when `start_bin >= n`.
fn prepare_lowband<'a>(
    lowband_buf: &[f64; 320],
    lowband_offset: usize,
    start_bin: usize,
    n: usize,
    tf_change: i8,
    is_transient: bool,
    scratch: &'a mut [f64; MAX_BAND_N],
) -> Option<&'a [f64]> {
    if start_bin < n {
        return None;
    }
    let fold_start = lowband_offset.saturating_sub(n);
    let fold_end = fold_start.wrapping_add(n);
    if fold_end > start_bin {
        return None;
    }
    let src = lowband_buf.get(fold_start..fold_end)?;
    let dst = scratch.get_mut(..n)?;
    dst.copy_from_slice(src);
    if tf_change != 0 || is_transient {
        invert_tf_hadamard(dst, tf_change, is_transient);
    }
    scratch.get(..n)
}

/// Decodes normalized shapes for all 17 wideband bands and tracks collapse masks (RFC 6716 Section 4.3.4).
#[expect(
    clippy::too_many_arguments,
    reason = "band decoder requires range coder, allocation outputs, TF decisions, spectrum buffer, and LCG seed"
)]
pub(crate) fn decode_all_bands(
    ec: &mut RangeDecoder<'_>,
    coded_bands: usize,
    pulses: &[i32; WB_BANDS],
    initial_balance: i32,
    tf_res: &[i8; WB_BANDS],
    spread: usize,
    is_transient: bool,
    total_bits_frac: i32,
    anti_collapse_rsv: i32,
    x: &mut [f64],
    collapse_masks: &mut [u8; WB_BANDS],
    seed: &mut u32,
) {
    let celt = tables();
    let cwrs = cwrs_table();
    let mut balance = initial_balance;
    let mut lowband_buf = [0.0_f64; 320];
    let mut lowband_scratch = [0.0_f64; MAX_BAND_N];
    let mut lowband_offset = 0_usize;
    let mut update_lowband = true;

    for band in 0..WB_BANDS {
        let start_bin = EBANDS
            .get(band)
            .copied()
            .unwrap_or(0)
            .wrapping_mul(M_BLOCKS);
        let end_bin = EBANDS
            .get(band.wrapping_add(1))
            .copied()
            .unwrap_or(0)
            .wrapping_mul(M_BLOCKS);
        let n = end_bin.wrapping_sub(start_bin);
        let base_slot = base_width_slot(n / M_BLOCKS);
        let tell = ec.tell_frac();
        let remaining_bands = coded_bands.saturating_sub(band);
        let curr_balance = if band >= coded_bands {
            0
        } else if remaining_bands == 1 {
            balance
        } else if remaining_bands == 2 {
            balance / 2
        } else {
            balance.checked_div(3).unwrap_or(0)
        };
        let alloc_b = pulses.get(band).copied().unwrap_or(0);
        let max_avail = total_bits_frac
            .saturating_sub(tell)
            .saturating_sub(anti_collapse_rsv)
            .saturating_sub(1)
            .max(0);
        let b = alloc_b.saturating_add(curr_balance).max(0).min(max_avail);

        let tf_change = tf_res.get(band).copied().unwrap_or(0);
        let blocks = band_blocks(n, tf_change, is_transient);
        if update_lowband {
            lowband_offset = start_bin;
        }
        let lowband_slice = prepare_lowband(
            &lowband_buf,
            lowband_offset,
            start_bin,
            n,
            tf_change,
            is_transient,
            &mut lowband_scratch,
        );
        let Some(band_x) = x.get_mut(start_bin..end_bin) else {
            break;
        };

        let is_coded = band < coded_bands;
        let band_budget = if is_coded { b } else { 0 };
        let mut remaining_bits = max_avail;
        let cm = quant_partition(
            celt,
            cwrs,
            ec,
            band_x,
            lowband_slice,
            base_slot,
            band,
            band_budget,
            &mut remaining_bits,
            blocks,
            i32::try_from(LM).unwrap_or(3),
            1.0,
            spread,
            seed,
        );
        if let Some(dst) = collapse_masks.get_mut(band) {
            *dst = if is_transient {
                expand_collapse_mask(cm, blocks)
            } else {
                cm
            };
        }
        if tf_change != 0 {
            apply_tf_hadamard(band_x, tf_change, is_transient);
        } else if is_transient {
            interleave_hadamard(band_x, M_BLOCKS, false);
        }

        let used = ec.tell_frac().saturating_sub(tell);
        if is_coded {
            balance = balance.saturating_add(alloc_b.saturating_sub(used));
        }

        if let Some(dst) = lowband_buf.get_mut(start_bin..end_bin) {
            let norm_scale = f64::from(u32::try_from(n).unwrap_or(1)).sqrt();
            for (d, &s) in dst.iter_mut().zip(band_x.iter()) {
                *d = s * norm_scale;
            }
        }
        let min_lowband_bits = i32::try_from(n).unwrap_or(0) << BITRES;
        update_lowband = band_budget > min_lowband_bits;
    }
}

/// Inserts controlled LCG energy into collapsed short blocks of transient frames (RFC 6716 Section 4.3.5).
pub(crate) fn anti_collapse(
    x: &mut [f64],
    collapse_masks: &[u8; WB_BANDS],
    pulses: &[i32; WB_BANDS],
    old_e: &[f64; WB_BANDS],
    prev1_e: &[f64; WB_BANDS],
    prev2_e: &[f64; WB_BANDS],
    seed: &mut u32,
) {
    for band in 0..WB_BANDS {
        let mask = collapse_masks.get(band).copied().unwrap_or(0xFF);
        if mask == 0xFF {
            continue;
        }
        let start_bin = EBANDS
            .get(band)
            .copied()
            .unwrap_or(0)
            .wrapping_mul(M_BLOCKS);
        let end_bin = EBANDS
            .get(band.wrapping_add(1))
            .copied()
            .unwrap_or(0)
            .wrapping_mul(M_BLOCKS);
        let n = end_bin.wrapping_sub(start_bin);
        let n0 = n / M_BLOCKS;
        let cur = old_e.get(band).copied().unwrap_or(-28.0);
        let p1 = prev1_e.get(band).copied().unwrap_or(-28.0);
        let p2 = prev2_e.get(band).copied().unwrap_or(-28.0);
        let prev_min = p1.min(p2);
        let base_width_f = f64::from(u32::try_from(n0.max(1)).unwrap_or(1));
        let band_width_f = f64::from(u32::try_from(n.max(1)).unwrap_or(1));
        let band_pulses = pulses.get(band).copied().unwrap_or(0).max(0);
        let depth = u32::try_from(
            band_pulses
                .saturating_add(1)
                .checked_div(i32::try_from(n).unwrap_or(8))
                .unwrap_or(0),
        )
        .unwrap_or(0)
        .min(112);
        let r_cap = (0.5 * (-f64::from(depth) / 8.0).exp2()) / band_width_f.sqrt();
        let r = ((prev_min - cur).exp2() / base_width_f.sqrt()).min(r_cap);
        let Some(band_x) = x.get_mut(start_bin..end_bin) else {
            continue;
        };
        let mut injected = false;
        for blk in 0..M_BLOCKS {
            if (mask & (1_u8 << blk)) != 0 {
                continue;
            }
            injected = true;
            for k in 0..n0 {
                let idx = k.wrapping_mul(M_BLOCKS).wrapping_add(blk);
                let sgn = if (celt_lcg(seed) & 0x8000) != 0 {
                    r
                } else {
                    -r
                };
                if let Some(dst) = band_x.get_mut(idx) {
                    *dst = sgn;
                }
            }
        }
        if injected {
            renormalise_vector(band_x, 1.0);
        }
    }
}
