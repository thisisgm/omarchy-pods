//! RFC 6716 Section 4.3.4.1-4.3.4.2 CWRS pulse decoding, pulse-cache lookup, and CELT LCG PRNG.

use std::sync::LazyLock;

use crate::range::RangeDecoder;
use crate::tables::{CeltTables, M_BLOCKS, ncwrs_checked};

/// Maximum dimension of a single WB 20 ms band (`6 * 8 = 48`).
pub(crate) const MAX_BAND_N: usize = 48;

/// Precomputed compact CWRS `V(n, k)` table for `0 <= n <= 48` so PVQ decoding runs in O(n) with zero recurrences.
pub(crate) struct CwrsTable {
    row_offs: [usize; MAX_BAND_N + 1],
    row_max_k: [usize; MAX_BAND_N + 1],
    values: Vec<u32>,
}

static CWRS_TABLE: LazyLock<CwrsTable> = LazyLock::new(build_cwrs_table);

/// Returns a reference to the process-global CWRS lookup table.
#[inline]
#[must_use]
pub(crate) fn cwrs_table() -> &'static CwrsTable {
    &CWRS_TABLE
}

/// Warms up the CWRS lookup table during decoder initialization so decoding never allocates.
pub(crate) fn warmup() {
    let _ = &*CWRS_TABLE;
}

fn build_cwrs_table() -> CwrsTable {
    let mut row_offs = [0_usize; MAX_BAND_N + 1];
    let mut row_max_k = [0_usize; MAX_BAND_N + 1];
    let mut values = Vec::new();
    for n in 0..=MAX_BAND_N {
        if let Some(dst) = row_offs.get_mut(n) {
            *dst = values.len();
        }
        let max_k = if n <= 5 { 128 } else { 96 };
        let mut last_k = 0_usize;
        for k in 0..=max_k {
            if let Some(v) = ncwrs_checked(n, k) {
                values.push(v);
                last_k = k;
            } else {
                break;
            }
        }
        if let Some(dst) = row_max_k.get_mut(n) {
            *dst = last_k;
        }
    }
    CwrsTable {
        row_offs,
        row_max_k,
        values,
    }
}

/// Returns the precomputed `V(n, 0..=row_max_k[n])` slice for `n <= MAX_BAND_N`.
#[inline]
fn row_slice(tbl: &CwrsTable, n: usize) -> &[u32] {
    if let (Some(&off), Some(&max_k)) = (tbl.row_offs.get(n), tbl.row_max_k.get(n))
        && let Some(slice) = tbl.values.get(off..=off.wrapping_add(max_k))
    {
        return slice;
    }
    &[]
}

/// Fast `O(1)` lookup of `V(n, k)` (RFC 6716 Section 4.3.4.2).
#[inline]
#[must_use]
fn v_lookup(tbl: &CwrsTable, n: usize, k: usize) -> u32 {
    if let Some(&v) = row_slice(tbl, n).get(k) {
        return v;
    }
    ncwrs_checked(n, k).unwrap_or(0)
}

/// Decodes a unit-norm PVQ vector of dimension `dim` with `pulses` pulses and returns its per-block collapse mask.
pub(crate) fn decode_pulses(
    tbl: &CwrsTable,
    ec: &mut RangeDecoder<'_>,
    vec: &mut [f64],
    pulses: usize,
    blocks: usize,
) -> u8 {
    let dim = vec.len();
    if dim == 0 || pulses == 0 {
        vec.fill(0.0);
        return 0;
    }
    let total = v_lookup(tbl, dim, pulses);
    if total == 0 {
        vec.fill(0.0);
        return 0;
    }
    let mut idx = ec.dec_uint(total);
    let mut rem_k = pulses;
    let mut energy = 0.0_f64;
    let slen = dim.checked_div(blocks.max(1)).unwrap_or(1).max(1);
    let mut cm = 0_u8;
    for (pos, dst) in vec.iter_mut().enumerate() {
        if rem_k == 0 {
            *dst = 0.0;
            continue;
        }
        let rem_dim = dim.wrapping_sub(pos);
        let prev_dim = rem_dim.wrapping_sub(1);
        let prev_row = row_slice(tbl, prev_dim);
        let vm = v_lookup(tbl, rem_dim, rem_k);
        let vm1 = prev_row
            .get(rem_k)
            .copied()
            .unwrap_or_else(|| v_lookup(tbl, prev_dim, rem_k));
        // Avoid `u32` overflow when `vm + vm1 >= 2^32` (e.g. `N=16, K=12`).
        let mut pivot =
            u32::try_from((u64::from(vm1).wrapping_add(u64::from(vm))) >> 1).unwrap_or(0);
        let sgn = if idx < pivot {
            1.0_f64
        } else {
            idx = idx.wrapping_sub(pivot);
            -1.0_f64
        };
        let k0 = rem_k;
        pivot = pivot.wrapping_sub(vm1);
        while pivot > idx && rem_k > 0 {
            rem_k = rem_k.wrapping_sub(1);
            let step = prev_row
                .get(rem_k)
                .copied()
                .unwrap_or_else(|| v_lookup(tbl, prev_dim, rem_k));
            pivot = pivot.saturating_sub(step);
        }
        let count = k0.wrapping_sub(rem_k);
        let mag = f64::from(u32::try_from(count).unwrap_or(0));
        let val = sgn * mag;
        *dst = val;
        if count > 0 {
            cm |= 1_u8 << (pos.checked_div(slen).unwrap_or(0) % M_BLOCKS);
        }
        energy += val * val;
        idx = idx.saturating_sub(pivot);
    }
    if energy > 0.0 {
        let inv = 1.0 / energy.sqrt();
        for sample in vec.iter_mut() {
            *sample *= inv;
        }
    }
    cm
}

/// Advances the RFC 6716 CELT 32-bit LCG PRNG (`seed = seed * 1664525 + 1013904223`).
#[inline]
pub(crate) fn celt_lcg(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *seed
}

/// Scales `x` to unit L2 norm multiplied by `gain`.
#[inline]
pub(crate) fn renormalise_vector(x: &mut [f64], gain: f64) {
    let energy: f64 = x.iter().map(|&v| v * v).sum();
    if energy > 0.0 {
        let scale = gain / energy.sqrt();
        for v in x.iter_mut() {
            *v *= scale;
        }
    }
}

/// Searches the pulse cache for the nearest pulse count `K` not exceeding `rem_avail` (RFC 6716 Section 4.3.4.1).
#[inline]
pub(crate) fn bits_to_pulses(
    celt: &CeltTables,
    base_slot: usize,
    lm: i32,
    b: i32,
    rem_avail: i32,
) -> usize {
    let lm_slot = usize::try_from(lm.wrapping_add(1)).unwrap_or(0).min(4);
    let Some(row) = celt.pulse_rows.get(base_slot).and_then(|r| r.get(lm_slot)) else {
        return 0;
    };
    let mut lo = 0_usize;
    let mut hi = row.max_idx;
    while lo < hi {
        let mid = (lo.wrapping_add(hi)) >> 1;
        let cost = row.bits.get(mid).copied().unwrap_or(i32::MAX);
        if cost < b {
            lo = mid.wrapping_add(1);
        } else {
            hi = mid;
        }
    }
    // Pick the closer entry, rounding down on exact tie or duplicate cost (e.g. `N=2, K=30` vs `32`).
    if lo > 0 {
        let cost_hi = row.bits.get(lo).copied().unwrap_or(i32::MAX);
        let cost_lo = row.bits.get(lo.wrapping_sub(1)).copied().unwrap_or(0);
        if cost_hi.saturating_sub(b) >= b.saturating_sub(cost_lo) {
            lo = lo.wrapping_sub(1);
        }
    }
    while lo > 0 && row.bits.get(lo).copied().unwrap_or(i32::MAX) > rem_avail {
        lo = lo.wrapping_sub(1);
    }
    lo
}

/// Returns the maximum non-split pulse bit budget for `(base_slot, lm)`.
#[inline]
pub(crate) fn max_direct_bits(celt: &CeltTables, base_slot: usize, lm: i32) -> i32 {
    let lm_slot = usize::try_from(lm.wrapping_add(1)).unwrap_or(0).min(4);
    let Some(row) = celt.pulse_rows.get(base_slot).and_then(|r| r.get(lm_slot)) else {
        return 0;
    };
    row.bits.get(row.max_idx).copied().unwrap_or(0)
}
