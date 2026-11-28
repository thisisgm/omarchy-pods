//! RFC 6716 Sections 4.3.4.3 and 4.3.4.5 CELT spreading rotations and Haar TF resolution transforms.

use std::f64::consts::{FRAC_1_SQRT_2, PI};

use crate::cwrs::MAX_BAND_N;
use crate::tables::{M_BLOCKS, SPREAD_FACTOR};

/// Applies one strided back-and-forth Givens rotation sweep (RFC 6716 Section 4.3.4.3).
#[inline]
fn exp_rotation1(x: &mut [f64], stride: usize, c: f64, s: f64) {
    let len = x.len();
    if stride == 0 || len <= stride {
        return;
    }
    let lim = len.wrapping_sub(stride);
    for i in 0..lim {
        let j = i.wrapping_add(stride);
        if let (Some(&xi), Some(&xj)) = (x.get(i), x.get(j)) {
            if let Some(di) = x.get_mut(i) {
                *di = c * xi + s * xj;
            }
            if let Some(dj) = x.get_mut(j) {
                *dj = -s * xi + c * xj;
            }
        }
    }
    if lim > stride {
        let rev_lim = lim.wrapping_sub(stride);
        for i in (0..rev_lim).rev() {
            let j = i.wrapping_add(stride);
            if let (Some(&xi), Some(&xj)) = (x.get(i), x.get(j)) {
                if let Some(di) = x.get_mut(i) {
                    *di = c * xi + s * xj;
                }
                if let Some(dj) = x.get_mut(j) {
                    *dj = -s * xi + c * xj;
                }
            }
        }
    }
}

/// Applies CELT spreading rotation to `vec` across `blocks` time blocks (RFC 6716 Section 4.3.4.3).
pub(crate) fn exp_rotation(vec: &mut [f64], dir: i32, blocks: usize, pulses: usize, spread: usize) {
    let dim = vec.len();
    if spread == 0 || pulses == 0 || blocks == 0 || pulses.wrapping_mul(2) >= dim {
        return;
    }
    let stride_len = dim.checked_div(blocks).unwrap_or(0);
    if stride_len <= 1 {
        return;
    }
    let fr = SPREAD_FACTOR.get(spread).copied().unwrap_or(0.0);
    let nf = f64::from(u32::try_from(dim).unwrap_or(1));
    let kf = f64::from(u32::try_from(pulses).unwrap_or(0));
    let gr = nf / (nf + fr * kf);
    let theta = 0.25 * PI * gr * gr;
    let (mut sin_t, cos_t) = theta.sin_cos();
    if dir < 0 {
        sin_t = -sin_t;
    }
    let stride2 = if stride_len >= 8 {
        let root = f64::from(u32::try_from(stride_len).unwrap_or(8))
            .sqrt()
            .round();
        let mut best = 1_usize;
        for cand in 2..=7_usize {
            let cf = f64::from(u32::try_from(cand).unwrap_or(1));
            if (cf - root).abs() < 0.25 {
                best = cand;
                break;
            }
        }
        best
    } else {
        0
    };
    let s2 = if dir < 0 { -cos_t } else { cos_t };
    let c2 = sin_t.abs();
    for slice in vec.chunks_exact_mut(stride_len) {
        if stride2 > 0 {
            exp_rotation1(slice, stride2, c2, s2);
        }
        exp_rotation1(slice, 1, cos_t, sin_t);
    }
}

/// Reorders elements between time-interleaved and sequency order for Hadamard TF changes (RFC 6716 Section 4.3.4.5).
pub(crate) fn interleave_hadamard(x: &mut [f64], stride: usize, hadamard: bool) {
    let n = x.len();
    if stride <= 1 || n == 0 || stride > n {
        return;
    }
    let mut tmp = [0.0_f64; MAX_BAND_N];
    let n0 = n.checked_div(stride).unwrap_or(0);
    if hadamard {
        let ordery: &[usize] = match stride {
            2 => &[0, 1],
            4 => &[0, 2, 3, 1],
            8 => &[0, 4, 6, 2, 3, 7, 5, 1],
            _ => return,
        };
        for i in 0..stride {
            let ord = ordery.get(i).copied().unwrap_or(i);
            for j in 0..n0 {
                let src = ord.wrapping_mul(n0).wrapping_add(j);
                let dst = j.wrapping_mul(stride).wrapping_add(i);
                if let (Some(&v), Some(out)) = (x.get(src), tmp.get_mut(dst)) {
                    *out = v;
                }
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                let src = i.wrapping_mul(n0).wrapping_add(j);
                let dst = j.wrapping_mul(stride).wrapping_add(i);
                if let (Some(&v), Some(out)) = (x.get(src), tmp.get_mut(dst)) {
                    *out = v;
                }
            }
        }
    }
    if let Some(src) = tmp.get(..n) {
        x.copy_from_slice(src);
    }
}

/// Applies one stride-`stride` normalized Haar butterfly pass `(a+b, b-a)` or `(a+b, a-b)` for TF synthesis.
fn haar1_tf(vec: &mut [f64], count: usize, stride: usize, forward_diff: bool) {
    let half = count >> 1;
    for lane in 0..stride {
        for pair in 0..half {
            let idx0 = lane.wrapping_add(stride.wrapping_mul(pair.wrapping_mul(2)));
            let idx1 = lane.wrapping_add(stride.wrapping_mul(pair.wrapping_mul(2).wrapping_add(1)));
            if let (Some(&va), Some(&vb)) = (vec.get(idx0), vec.get(idx1)) {
                let sum = FRAC_1_SQRT_2 * (va + vb);
                let diff = FRAC_1_SQRT_2 * if forward_diff { va - vb } else { vb - va };
                if let Some(da) = vec.get_mut(idx0) {
                    *da = sum;
                }
                if let Some(db) = vec.get_mut(idx1) {
                    *db = diff;
                }
            }
        }
    }
}

/// Inverse of [`interleave_hadamard`]: maps time-interleaved MDCT bins back to block-contiguous order.
fn deinterleave_hadamard(x: &mut [f64], stride: usize, hadamard: bool) {
    let n = x.len();
    if stride <= 1 || n == 0 || stride > n {
        return;
    }
    let mut tmp = [0.0_f64; MAX_BAND_N];
    let n0 = n.checked_div(stride).unwrap_or(0);
    if hadamard {
        let ordery: &[usize] = match stride {
            2 => &[0, 1],
            4 => &[0, 2, 3, 1],
            8 => &[0, 4, 6, 2, 3, 7, 5, 1],
            _ => return,
        };
        for i in 0..stride {
            let ord = ordery.get(i).copied().unwrap_or(i);
            for j in 0..n0 {
                let src = j.wrapping_mul(stride).wrapping_add(i);
                let dst = ord.wrapping_mul(n0).wrapping_add(j);
                if let (Some(&v), Some(out)) = (x.get(src), tmp.get_mut(dst)) {
                    *out = v;
                }
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                let src = j.wrapping_mul(stride).wrapping_add(i);
                let dst = i.wrapping_mul(n0).wrapping_add(j);
                if let (Some(&v), Some(out)) = (x.get(src), tmp.get_mut(dst)) {
                    *out = v;
                }
            }
        }
    }
    if let Some(src) = tmp.get(..n) {
        x.copy_from_slice(src);
    }
}

/// Applies the inverse of `haar1_tf(..., false)`: `(u, v) -> ((u - v)/sqrt(2), (u + v)/sqrt(2))`.
fn inv_haar1_tf_rev(vec: &mut [f64], count: usize, stride: usize) {
    let half = count >> 1;
    for lane in 0..stride {
        for pair in 0..half {
            let idx0 = lane.wrapping_add(stride.wrapping_mul(pair.wrapping_mul(2)));
            let idx1 = lane.wrapping_add(stride.wrapping_mul(pair.wrapping_mul(2).wrapping_add(1)));
            if let (Some(&u), Some(&v)) = (vec.get(idx0), vec.get(idx1)) {
                if let Some(da) = vec.get_mut(idx0) {
                    *da = FRAC_1_SQRT_2 * (u - v);
                }
                if let Some(db) = vec.get_mut(idx1) {
                    *db = FRAC_1_SQRT_2 * (u + v);
                }
            }
        }
    }
}

/// Exact orthogonal inverse of [`apply_tf_hadamard`] (plus transient interleave) to align `lowband` before folding.
pub(crate) fn invert_tf_hadamard(x: &mut [f64], tf_change: i8, is_transient: bool) {
    let n = x.len();
    if n <= 1 {
        return;
    }
    if tf_change == 0 {
        if is_transient {
            deinterleave_hadamard(x, M_BLOCKS, false);
        }
        return;
    }
    let levels = usize::from(tf_change.unsigned_abs()).min(3);
    let stride = 1_usize << levels;
    if tf_change < 0 {
        if is_transient {
            let fine_stride = M_BLOCKS << levels;
            if n >= fine_stride {
                haar1_tf(x, n / M_BLOCKS, M_BLOCKS, true);
                deinterleave_hadamard(x, fine_stride, false);
            } else {
                deinterleave_hadamard(x, M_BLOCKS, false);
            }
            return;
        }
        if stride > n {
            return;
        }
        deinterleave_hadamard(x, stride, true);
        let group = n.checked_div(stride).unwrap_or(1);
        let mut st = group << levels.saturating_sub(1);
        let mut rem = 2_usize;
        for _ in 0..levels {
            inv_haar1_tf_rev(x, rem, st);
            st >>= 1;
            rem <<= 1;
        }
    } else {
        if !is_transient || M_BLOCKS < stride {
            return;
        }
        if levels == 3 {
            // Full transient frequency merge (`tf = +3`, `blocks = 1`) operates per 8-block frequency bin.
            for chunk in x.as_chunks_mut::<M_BLOCKS>().0 {
                haar1_tf(chunk, 2, 4, true);
                haar1_tf(chunk, 4, 2, true);
                haar1_tf(chunk, 8, 1, true);
            }
            return;
        }
        deinterleave_hadamard(x, M_BLOCKS, false);
        let group = n.checked_div(M_BLOCKS).unwrap_or(1);
        let mut st = group << levels.saturating_sub(1);
        let mut rem = M_BLOCKS >> levels.saturating_sub(1);
        for _ in 0..levels {
            haar1_tf(x, rem, st, true);
            st >>= 1;
            rem <<= 1;
        }
    }
}

/// Applies `|tf_change|` levels of normalized Hadamard TF transform (RFC 6716 Section 4.3.4.5).
pub(crate) fn apply_tf_hadamard(x: &mut [f64], tf_change: i8, is_transient: bool) {
    let n = x.len();
    if tf_change == 0 || n <= 1 {
        return;
    }
    let levels = usize::from(tf_change.unsigned_abs()).min(3);
    let stride = 1_usize << levels;
    if tf_change < 0 {
        if is_transient {
            // Transient `tf_change = -1` splits frequency pairs per short block when `n > M_BLOCKS`.
            let fine_stride = M_BLOCKS << levels;
            if n >= fine_stride {
                interleave_hadamard(x, fine_stride, false);
                haar1_tf(x, n / M_BLOCKS, M_BLOCKS, true);
            } else {
                interleave_hadamard(x, M_BLOCKS, false);
            }
            return;
        }
        if stride > n {
            return;
        }
        // Increase time resolution: run `levels` Haar passes across blocks, then interleave in sequency order.
        let group = n.checked_div(stride).unwrap_or(1);
        let mut st = group;
        let mut rem = stride;
        for _ in 0..levels {
            haar1_tf(x, rem, st, false);
            st <<= 1;
            rem >>= 1;
        }
        interleave_hadamard(x, stride, true);
    } else {
        // Increase frequency resolution on a transient frame across the `M_BLOCKS = 8` blocks.
        if !is_transient || M_BLOCKS < stride {
            return;
        }
        if levels == 3 {
            // `tf = +3` (`blocks = 1`) stores each frequency bin's 8 sequency bins contiguously.
            for chunk in x.as_chunks_mut::<M_BLOCKS>().0 {
                haar1_tf(chunk, 8, 1, true);
                haar1_tf(chunk, 4, 2, true);
                haar1_tf(chunk, 2, 4, true);
            }
            return;
        }
        let group = n.checked_div(M_BLOCKS).unwrap_or(1);
        let mut st = group;
        let mut rem = M_BLOCKS;
        for _ in 0..levels {
            haar1_tf(x, rem, st, true);
            st <<= 1;
            rem >>= 1;
        }
        interleave_hadamard(x, M_BLOCKS, false);
    }
}
