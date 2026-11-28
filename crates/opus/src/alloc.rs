//! RFC 6716 Section 4.3.3 bit-exact CELT bit allocation for wideband 20 ms mono.

use crate::range::RangeDecoder;
use crate::tables::{
    ALLOC_VECTORS, BITRES, EBANDS, FINE_OFFSET, LM, LOG_N, M_BLOCKS, MAX_FINE_BITS, TRIM_ICDF,
    WB_BANDS, tables,
};

/// Result of RFC 6716 Section 4.3.3 bit allocation for one mono frame.
#[derive(Debug, Clone)]
pub(crate) struct Allocation {
    pub(crate) coded_bands: usize,
    pub(crate) pulses: [i32; WB_BANDS],
    pub(crate) fine_quant: [i32; WB_BANDS],
    pub(crate) fine_priority: [u8; WB_BANDS],
    pub(crate) balance: i32,
    pub(crate) anti_collapse_rsv: i32,
}

/// Decodes dynamic band boosts from range-coded 1/64-to-1/2 flags (e.g. payload `7f 54 ce ...` after TOC) into `(boosts, total_boost, skip_start)` in 1/8th bits (RFC 6716 Section 4.3.3).
pub(crate) fn decode_dynalloc(
    ec: &mut RangeDecoder<'_>,
    total_bits_frac: i32,
) -> ([i32; WB_BANDS], i32, usize) {
    let caps = &tables().cap_lm3_mono;
    let mut offsets = [0_i32; WB_BANDS];
    let mut dynalloc_logp = 6_u32;
    let mut total_bits = total_bits_frac;
    let mut total_boost = 0_i32;
    let mut skip_start = 1_usize;
    let mut tell = ec.tell_frac();
    for band in 0..WB_BANDS {
        let n0 = EBANDS
            .get(band.wrapping_add(1))
            .copied()
            .unwrap_or(0)
            .wrapping_sub(EBANDS.get(band).copied().unwrap_or(0));
        let n = i32::try_from(n0.wrapping_mul(M_BLOCKS)).unwrap_or(8);
        let quanta = n.max(48).min(n.wrapping_mul(8));
        let cap = caps.get(band).copied().unwrap_or(0);
        let mut boost = 0_i32;
        let mut loop_logp = dynalloc_logp;
        while tell.saturating_add(i32::try_from(loop_logp).unwrap_or(6) << BITRES) < total_bits
            && boost < cap
        {
            let flag = ec.dec_bit_logp(loop_logp);
            tell = ec.tell_frac();
            if flag == 0 {
                break;
            }
            boost = boost.saturating_add(quanta);
            total_boost = total_boost.saturating_add(quanta);
            total_bits = total_bits.saturating_sub(quanta);
            loop_logp = 1;
        }
        if boost > 0 {
            skip_start = band.wrapping_add(1);
            if dynalloc_logp > 2 {
                dynalloc_logp = dynalloc_logp.wrapping_sub(1);
            }
        }
        if let Some(dst) = offsets.get_mut(band) {
            *dst = boost;
        }
    }
    (offsets, total_boost, skip_start)
}

/// Decodes allocation trim (`0..=10`, default `5`) if budget permits (RFC 6716 Section 4.3.3, Table 58).
pub(crate) fn decode_trim(
    ec: &mut RangeDecoder<'_>,
    total_bits_frac: i32,
    total_boost: i32,
) -> i32 {
    let tell = ec.tell_frac();
    if tell.saturating_add(6 << BITRES) <= total_bits_frac.saturating_sub(total_boost) {
        i32::try_from(ec.dec_icdf(&TRIM_ICDF, 7)).unwrap_or(5)
    } else {
        5
    }
}

/// Computes per-band shape bit allocations, fine-energy bits, and priorities (RFC 6716 Section 4.3.3).
pub(crate) fn compute_allocation(
    ec: &mut RangeDecoder<'_>,
    total_bits_frac: i32,
    offsets: &[i32; WB_BANDS],
    skip_start: usize,
    alloc_trim: i32,
    is_transient: bool,
) -> Allocation {
    let caps = &tables().cap_lm3_mono;
    let lm_i32 = i32::try_from(LM).unwrap_or(3);
    let mut total = total_bits_frac
        .saturating_sub(ec.tell_frac())
        .saturating_sub(1);
    let anti_collapse_rsv = if is_transient && total >= lm_i32.wrapping_add(2) << BITRES {
        1 << BITRES
    } else {
        0
    };
    total = total.saturating_sub(anti_collapse_rsv).max(0);
    let skip_rsv = if total >= (1 << BITRES) {
        1 << BITRES
    } else {
        0
    };
    total = total.saturating_sub(skip_rsv);

    let (thresh, trim_offset) = compute_thresh_and_trim(alloc_trim, lm_i32);
    let (mut bits, mut psum) =
        search_and_interpolate_bits(total, &trim_offset, offsets, caps, &thresh);
    let left_pre = total.saturating_sub(psum).max(0);
    let alloc_floor = 1_i32 << BITRES;
    for band in 0..WB_BANDS {
        let eb = i32::try_from(EBANDS.get(band).copied().unwrap_or(0)).unwrap_or(0);
        if bits.get(band).copied().unwrap_or(0) == 0
            && left_pre >= eb.wrapping_mul(2).wrapping_add(alloc_floor)
            && let Some(dst) = bits.get_mut(band)
        {
            *dst = alloc_floor;
            psum = psum.saturating_add(alloc_floor);
        }
    }
    let coded_bands = apply_band_skipping(
        ec, skip_start, skip_rsv, &thresh, &mut bits, &mut psum, &mut total,
    );
    distribute_leftover_bits(coded_bands, total, psum, &mut bits);
    let (pulses, fine_quant, fine_priority, balance) =
        split_fine_and_pulses(coded_bands, lm_i32, caps, &bits);

    Allocation {
        coded_bands,
        pulses,
        fine_quant,
        fine_priority,
        balance,
        anti_collapse_rsv,
    }
}

/// Computes per-band minimum thresholds and trim offsets in 1/8th bits.
fn compute_thresh_and_trim(alloc_trim: i32, lm_i32: i32) -> ([i32; WB_BANDS], [i32; WB_BANDS]) {
    let mut thresh = [0_i32; WB_BANDS];
    let mut trim_offset = [0_i32; WB_BANDS];
    let trim_delta = alloc_trim.wrapping_sub(5).wrapping_sub(lm_i32);
    for band in 0..WB_BANDS {
        let n0 = EBANDS
            .get(band.wrapping_add(1))
            .copied()
            .unwrap_or(0)
            .wrapping_sub(EBANDS.get(band).copied().unwrap_or(0));
        let n0_i32 = i32::try_from(n0).unwrap_or(1);
        let n = n0_i32.wrapping_mul(i32::try_from(M_BLOCKS).unwrap_or(8));
        if let Some(dst) = thresh.get_mut(band) {
            *dst = (3_i32.wrapping_mul(n) >> 1).max(1 << BITRES);
        }
        let rem_bands = i32::try_from(WB_BANDS.wrapping_sub(band).wrapping_sub(1)).unwrap_or(0);
        let t_off = trim_delta
            .wrapping_mul(n0_i32)
            .wrapping_mul(rem_bands)
            .wrapping_mul(1 << (LM.wrapping_add(3)))
            / 64;
        if let Some(dst) = trim_offset.get_mut(band) {
            *dst = t_off;
        }
    }
    (thresh, trim_offset)
}

/// Binary-searches Table 57 quality columns and interpolates in 1/64 steps.
fn search_and_interpolate_bits(
    total: i32,
    trim_offset: &[i32; WB_BANDS],
    offsets: &[i32; WB_BANDS],
    caps: &[i32; WB_BANDS],
    thresh: &[i32; WB_BANDS],
) -> ([i32; WB_BANDS], i32) {
    let mut lo = 0_usize;
    let mut hi = 10_usize;
    while hi.wrapping_sub(lo) > 1 {
        let mid = (lo.wrapping_add(hi)) >> 1;
        let mut psum = 0_i32;
        let mut coded = false;
        for band in (0..WB_BANDS).rev() {
            let q = static_band_bits(band, mid, trim_offset, offsets, caps, thresh, &mut coded);
            psum = psum.saturating_add(q);
        }
        if psum > total {
            hi = mid;
        } else {
            lo = mid;
        }
    }

    let mut bits1 = [0_i32; WB_BANDS];
    let mut bits2 = [0_i32; WB_BANDS];
    for band in 0..WB_BANDS {
        let n0 = EBANDS
            .get(band.wrapping_add(1))
            .copied()
            .unwrap_or(0)
            .wrapping_sub(EBANDS.get(band).copied().unwrap_or(0));
        let n0_i32 = i32::try_from(n0).unwrap_or(1);
        let off = offsets.get(band).copied().unwrap_or(0);
        let t_off = trim_offset.get(band).copied().unwrap_or(0);
        let v_lo = i32::from(
            ALLOC_VECTORS
                .get(band)
                .and_then(|row| row.get(lo))
                .copied()
                .unwrap_or(0),
        );
        let v_hi = i32::from(
            ALLOC_VECTORS
                .get(band)
                .and_then(|row| row.get(hi))
                .copied()
                .unwrap_or(0),
        );
        let mut b1 = (v_lo.wrapping_mul(n0_i32) << LM) >> 2;
        let mut b2 = (v_hi.wrapping_mul(n0_i32) << LM) >> 2;
        if b1 > 0 {
            b1 = b1.saturating_add(t_off).max(0);
        }
        if b2 > 0 {
            b2 = b2.saturating_add(t_off).max(0);
        }
        if let (Some(d1), Some(d2)) = (bits1.get_mut(band), bits2.get_mut(band)) {
            *d1 = if lo > 0 { b1.saturating_add(off) } else { 0 };
            *d2 = b2.saturating_add(off);
        }
    }

    interpolate_band_bits(total, &bits1, &bits2, caps, thresh)
}

/// Interpolates between adjacent Table 57 bracket rows in 1/64 steps with per-band caps and top-down threshold clamping.
fn interpolate_band_bits(
    total: i32,
    bits1: &[i32; WB_BANDS],
    bits2: &[i32; WB_BANDS],
    caps: &[i32; WB_BANDS],
    thresh: &[i32; WB_BANDS],
) -> ([i32; WB_BANDS], i32) {
    let alloc_floor = 1_i32 << BITRES;
    let mut ilo = 0_i32;
    let mut ihi = 64_i32;
    for _ in 0..6 {
        let mid = (ilo.wrapping_add(ihi)) >> 1;
        let mut psum = 0_i32;
        let mut coded = false;
        for band in (0..WB_BANDS).rev() {
            let b1 = bits1.get(band).copied().unwrap_or(0);
            let b2 = bits2.get(band).copied().unwrap_or(0);
            let cap = caps.get(band).copied().unwrap_or(0);
            let th = thresh.get(band).copied().unwrap_or(0);
            let tmp = b1
                .wrapping_add((mid.wrapping_mul(b2.wrapping_sub(b1))) >> 6)
                .min(cap);
            if tmp >= th {
                coded = true;
                psum = psum.saturating_add(tmp);
            } else if coded {
                psum = psum.saturating_add(tmp);
            } else if tmp >= alloc_floor {
                psum = psum.saturating_add(alloc_floor);
            }
        }
        if psum > total {
            ihi = mid;
        } else {
            ilo = mid;
        }
    }

    let mut bits = [0_i32; WB_BANDS];
    let mut psum = 0_i32;
    let mut coded = false;
    for band in (0..WB_BANDS).rev() {
        let b1 = bits1.get(band).copied().unwrap_or(0);
        let b2 = bits2.get(band).copied().unwrap_or(0);
        let cap = caps.get(band).copied().unwrap_or(0);
        let th = thresh.get(band).copied().unwrap_or(0);
        let mut tmp = b1
            .wrapping_add((ilo.wrapping_mul(b2.wrapping_sub(b1))) >> 6)
            .min(cap);
        if tmp >= th {
            coded = true;
        } else if !coded {
            tmp = if tmp >= alloc_floor { alloc_floor } else { 0 };
        }
        if let Some(dst) = bits.get_mut(band) {
            *dst = tmp;
        }
        psum = psum.saturating_add(tmp);
    }
    (bits, psum)
}

/// Decodes top-down band-skip decisions from `WB_BANDS` down to `skip_start` (RFC 6716 Section 4.3.3).
fn apply_band_skipping(
    ec: &mut RangeDecoder<'_>,
    skip_start: usize,
    skip_rsv: i32,
    thresh: &[i32; WB_BANDS],
    bits: &mut [i32; WB_BANDS],
    psum: &mut i32,
    total: &mut i32,
) -> usize {
    let alloc_floor = 1_i32 << BITRES;
    let mut coded_bands = WB_BANDS;
    let mut j = WB_BANDS;
    let tell_before_skip = ec.tell_frac();
    while j > skip_start {
        let band = j.wrapping_sub(1);
        let band_bits = bits.get(band).copied().unwrap_or(0);
        let th = thresh.get(band).copied().unwrap_or(0);
        if band_bits >= th {
            if skip_rsv > 0 {
                let stop = ec.dec_bit_logp(1);
                if stop != 0 {
                    coded_bands = j;
                    break;
                }
            } else {
                coded_bands = j;
                break;
            }
        }
        let keep = if band_bits >= alloc_floor {
            alloc_floor
        } else {
            0
        };
        *psum = psum.saturating_sub(band_bits.saturating_sub(keep));
        if let Some(dst) = bits.get_mut(band) {
            *dst = keep;
        }
        if j == skip_start.wrapping_add(1) {
            coded_bands = skip_start;
        }
        j = band;
    }
    let skip_used = ec.tell_frac().saturating_sub(tell_before_skip);
    *total = total.saturating_add(skip_rsv).saturating_sub(skip_used);
    coded_bands
}

/// Distributes leftover bits proportionally across `0..coded_bands` in base-bin (`n0`) units.
fn distribute_leftover_bits(coded_bands: usize, total: i32, psum: i32, bits: &mut [i32; WB_BANDS]) {
    let left = total.saturating_sub(psum).max(0);
    let coded_n0 = i32::try_from(EBANDS.get(coded_bands).copied().unwrap_or(1)).unwrap_or(1);
    let percoeff = left.checked_div(coded_n0).unwrap_or(0);
    let mut rem_left = left.saturating_sub(percoeff.wrapping_mul(coded_n0));
    for band in 0..coded_bands {
        let n0 = i32::try_from(
            EBANDS
                .get(band.wrapping_add(1))
                .copied()
                .unwrap_or(0)
                .wrapping_sub(EBANDS.get(band).copied().unwrap_or(0)),
        )
        .unwrap_or(1);
        let add_rem = rem_left.min(n0);
        rem_left = rem_left.saturating_sub(add_rem);
        if let Some(dst) = bits.get_mut(band) {
            *dst = dst
                .saturating_add(percoeff.wrapping_mul(n0))
                .saturating_add(add_rem);
        }
    }
}

/// Splits each band's allocation into fine energy bits, priorities, and shape pulse bits, carrying excess above band caps forward.
fn split_fine_and_pulses(
    coded_bands: usize,
    lm_i32: i32,
    caps: &[i32; WB_BANDS],
    bits: &[i32; WB_BANDS],
) -> ([i32; WB_BANDS], [i32; WB_BANDS], [u8; WB_BANDS], i32) {
    let mut fine_quant = [0_i32; WB_BANDS];
    let mut fine_priority = [1_u8; WB_BANDS];
    let mut pulses = [0_i32; WB_BANDS];
    let mut carry = 0_i32;
    for band in 0..WB_BANDS {
        let n0 = EBANDS
            .get(band.wrapping_add(1))
            .copied()
            .unwrap_or(0)
            .wrapping_sub(EBANDS.get(band).copied().unwrap_or(0));
        let n = i32::try_from(n0.wrapping_mul(M_BLOCKS)).unwrap_or(8);
        let cap = caps.get(band).copied().unwrap_or(0);
        let mut b = bits.get(band).copied().unwrap_or(0);
        if band >= coded_bands {
            let eb = (b >> BITRES).clamp(0, MAX_FINE_BITS);
            if let (Some(dfq), Some(dfp), Some(dp)) = (
                fine_quant.get_mut(band),
                fine_priority.get_mut(band),
                pulses.get_mut(band),
            ) {
                *dfq = eb;
                *dfp = u8::from(eb == 0);
                *dp = 0;
            }
            continue;
        }
        b = b.saturating_add(carry);
        let capped = b > cap;
        let mut excess = if capped {
            let ex = b.saturating_sub(cap);
            b = cap;
            ex
        } else {
            0
        };
        let log_m = LOG_N
            .get(band)
            .copied()
            .unwrap_or(0)
            .wrapping_add(lm_i32 << BITRES);
        let nc_log_n = n.wrapping_mul(log_m);
        let mut offset = (nc_log_n >> 1).wrapping_sub(n.wrapping_mul(FINE_OFFSET));
        let den = n << BITRES;
        if b.saturating_add(offset) < 2_i32.wrapping_mul(den) {
            offset = offset.saturating_add(nc_log_n >> 2);
        } else if b.saturating_add(offset) < 3_i32.wrapping_mul(den) {
            offset = offset.saturating_add(nc_log_n >> 3);
        }
        let tmp = b.saturating_add(offset);
        let mut ebits = if tmp > 0 {
            tmp.saturating_add(den >> 1).checked_div(den).unwrap_or(0)
        } else {
            0
        };
        if ebits > MAX_FINE_BITS {
            ebits = MAX_FINE_BITS;
        }
        // Bands below 2 bits cannot code PVQ pulses, so spend whole bits on fine energy (RFC 6716 Section 4.3.3).
        if ebits == 0 && ((1 << BITRES)..(2 << BITRES)).contains(&b) {
            ebits = 1;
        }
        if ebits.wrapping_mul(1 << BITRES) > b {
            ebits = b >> BITRES;
        }
        let shape_bits = b.saturating_sub(ebits << BITRES);
        if capped {
            while excess >= (1 << BITRES) && ebits < MAX_FINE_BITS {
                ebits = ebits.wrapping_add(1);
                excess = excess.saturating_sub(1 << BITRES);
            }
        }
        carry = excess;
        let prio = if capped {
            0
        } else {
            u8::from(ebits.wrapping_mul(den) >= tmp)
        };
        if let (Some(dfq), Some(dfp), Some(dp)) = (
            fine_quant.get_mut(band),
            fine_priority.get_mut(band),
            pulses.get_mut(band),
        ) {
            *dfq = ebits;
            *dfp = prio;
            *dp = shape_bits;
        }
    }
    (pulses, fine_quant, fine_priority, carry)
}

/// Evaluates one Table 57 static allocation entry with trim, boost, cap, and top-down threshold.
fn static_band_bits(
    band: usize,
    q_idx: usize,
    trim_offset: &[i32; WB_BANDS],
    offsets: &[i32; WB_BANDS],
    caps: &[i32; WB_BANDS],
    thresh: &[i32; WB_BANDS],
    coded: &mut bool,
) -> i32 {
    let n0 = EBANDS
        .get(band.wrapping_add(1))
        .copied()
        .unwrap_or(0)
        .wrapping_sub(EBANDS.get(band).copied().unwrap_or(0));
    let n0_i32 = i32::try_from(n0).unwrap_or(1);
    let val = i32::from(
        ALLOC_VECTORS
            .get(band)
            .and_then(|row| row.get(q_idx))
            .copied()
            .unwrap_or(0),
    );
    let mut q = (val.wrapping_mul(n0_i32) << LM) >> 2;
    if q > 0 {
        let t_off = trim_offset.get(band).copied().unwrap_or(0);
        q = q.saturating_add(t_off).max(0);
    }
    let off = offsets.get(band).copied().unwrap_or(0);
    let cap = caps.get(band).copied().unwrap_or(0);
    let th = thresh.get(band).copied().unwrap_or(0);
    q = q.saturating_add(off).min(cap);
    let alloc_floor = 1_i32 << BITRES;
    if q >= th {
        *coded = true;
        q
    } else if *coded {
        q
    } else if q >= alloc_floor {
        alloc_floor
    } else {
        0
    }
}
