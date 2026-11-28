//! RFC 6716 Section 4.3.7 exact `f64` CELT IMDCT (long 960 and 8x120 transient) via Good-Thomas PFA FFT.

use std::f64::consts::PI;
use std::sync::LazyLock;

use crate::tables::{M_BLOCKS, OVERLAP_48K, tables};

/// Long-block MDCT size (`960` bins, `P = 480 = 3 * 5 * 32` complex FFT).
pub(crate) const N_LONG: usize = 960;
const P_LONG: usize = N_LONG / 2;
/// Short-block MDCT size (`120` bins, `P = 60 = 3 * 5 * 4` complex FFT).
const N_SHORT: usize = N_LONG / M_BLOCKS;
const P_SHORT: usize = N_SHORT / 2;
/// Wideband spectrum active bin count (`40 * 8 = 320`; bins `320..960` are always zero).
const WB_ACTIVE_BINS: usize = 320;
const WB_ACTIVE_HALF: usize = WB_ACTIVE_BINS / 2;
/// Short-block active bin count (`40`; bins `40..120` are always zero in wideband).
const WB_SHORT_ACTIVE: usize = WB_ACTIVE_BINS / M_BLOCKS;
const WB_SHORT_HALF: usize = WB_SHORT_ACTIVE / 2;

const SQRT3_2: f64 = 0.866_025_403_784_438_6;
const COS_2PI_5: f64 = 0.309_016_994_374_947_45;
const SIN_2PI_5: f64 = 0.951_056_516_295_153_5;
const COS_4PI_5: f64 = -0.809_016_994_374_947_5;
const SIN_4PI_5: f64 = 0.587_785_252_292_473_1;

/// Precomputed DCT-IV twiddles and `[15][32]`/`[15][4]` CRT maps for `P_LONG = 480` and `P_SHORT = 60`.
struct MdctTables {
    long_tw_c: [f64; P_LONG],
    long_tw_s: [f64; P_LONG],
    long_scatter_lo: [(u8, u8); WB_ACTIVE_HALF],
    long_scatter_hi: [(u8, u8); WB_ACTIVE_HALF],
    long_gather: [[u16; 32]; 15],
    short_tw_c: [f64; P_SHORT],
    short_tw_s: [f64; P_SHORT],
    short_scatter_lo: [(u8, u8); WB_SHORT_HALF],
    short_scatter_hi: [(u8, u8); WB_SHORT_HALF],
    short_gather: [[u8; 4]; 15],
    tw8_c: [f64; 4],
    tw8_s: [f64; 4],
    tw16_c: [f64; 8],
    tw16_s: [f64; 8],
    tw32_c: [f64; 16],
    tw32_s: [f64; 16],
}

static MDCT_TABLES: LazyLock<MdctTables> = LazyLock::new(build_mdct_tables);

/// Warms up IMDCT tables during decoder construction so decode never allocates.
pub(crate) fn warmup() {
    let _ = &*MDCT_TABLES;
}

type LongTables = (
    [f64; P_LONG],
    [f64; P_LONG],
    [(u8, u8); WB_ACTIVE_HALF],
    [(u8, u8); WB_ACTIVE_HALF],
    [[u16; 32]; 15],
);

type ShortTables = (
    [f64; P_SHORT],
    [f64; P_SHORT],
    [(u8, u8); WB_SHORT_HALF],
    [(u8, u8); WB_SHORT_HALF],
    [[u8; 4]; 15],
);

fn build_long_tables() -> LongTables {
    let mut tw_c = [0.0_f64; P_LONG];
    let mut tw_s = [0.0_f64; P_LONG];
    let mut scatter_lo = [(0_u8, 0_u8); WB_ACTIVE_HALF];
    let mut scatter_hi = [(0_u8, 0_u8); WB_ACTIVE_HALF];
    let mut gather = [[0_u16; 32]; 15];
    let step = PI / f64::from(u16::try_from(N_LONG).unwrap_or(960));
    for (r, (dc, ds)) in tw_c.iter_mut().zip(tw_s.iter_mut()).enumerate() {
        let a = step * (f64::from(u16::try_from(r).unwrap_or(0)) + 0.125);
        let (s, c) = a.sin_cos();
        *dc = c;
        *ds = s;
    }
    let hi_start = P_LONG.wrapping_sub(WB_ACTIVE_HALF);
    // CRT for `480 = 3 * 5 * 32`: `r = (15*d1 + 160*d2 + 96*d3) mod 480`, `q = (225*q1 + 160*q2 + 96*q3) mod 480`.
    for d2 in 0..3_usize {
        for d3 in 0..5_usize {
            let lane = d2.wrapping_mul(5).wrapping_add(d3);
            for d1 in 0..32_usize {
                let r = d1
                    .wrapping_mul(15)
                    .wrapping_add(d2.wrapping_mul(160))
                    .wrapping_add(d3.wrapping_mul(96))
                    .wrapping_rem(P_LONG);
                let q = d1
                    .wrapping_mul(225)
                    .wrapping_add(d2.wrapping_mul(160))
                    .wrapping_add(d3.wrapping_mul(96))
                    .wrapping_rem(P_LONG);
                let coord = (
                    u8::try_from(lane).unwrap_or(0),
                    u8::try_from(d1).unwrap_or(0),
                );
                if r < WB_ACTIVE_HALF {
                    if let Some(dst) = scatter_lo.get_mut(r) {
                        *dst = coord;
                    }
                } else if r >= hi_start
                    && let Some(dst) = scatter_hi.get_mut(r.wrapping_sub(hi_start))
                {
                    *dst = coord;
                }
                if let Some(dst) = gather.get_mut(lane).and_then(|row| row.get_mut(d1)) {
                    *dst = u16::try_from(q).unwrap_or(0);
                }
            }
        }
    }
    (tw_c, tw_s, scatter_lo, scatter_hi, gather)
}

fn build_short_tables() -> ShortTables {
    let mut tw_c = [0.0_f64; P_SHORT];
    let mut tw_s = [0.0_f64; P_SHORT];
    let mut scatter_lo = [(0_u8, 0_u8); WB_SHORT_HALF];
    let mut scatter_hi = [(0_u8, 0_u8); WB_SHORT_HALF];
    let mut gather = [[0_u8; 4]; 15];
    let step = PI / f64::from(u16::try_from(N_SHORT).unwrap_or(120));
    for (r, (dc, ds)) in tw_c.iter_mut().zip(tw_s.iter_mut()).enumerate() {
        let a = step * (f64::from(u16::try_from(r).unwrap_or(0)) + 0.125);
        let (s, c) = a.sin_cos();
        *dc = c;
        *ds = s;
    }
    let hi_start = P_SHORT.wrapping_sub(WB_SHORT_HALF);
    // CRT for `60 = 3 * 5 * 4`: `r = (15*d1 + 20*d2 + 12*d3) mod 60`, `q = (45*q1 + 40*q2 + 36*q3) mod 60`.
    for d2 in 0..3_usize {
        for d3 in 0..5_usize {
            let lane = d2.wrapping_mul(5).wrapping_add(d3);
            for d1 in 0..4_usize {
                let r = d1
                    .wrapping_mul(15)
                    .wrapping_add(d2.wrapping_mul(20))
                    .wrapping_add(d3.wrapping_mul(12))
                    .wrapping_rem(P_SHORT);
                let q = d1
                    .wrapping_mul(45)
                    .wrapping_add(d2.wrapping_mul(40))
                    .wrapping_add(d3.wrapping_mul(36))
                    .wrapping_rem(P_SHORT);
                let coord = (
                    u8::try_from(lane).unwrap_or(0),
                    u8::try_from(d1).unwrap_or(0),
                );
                if r < WB_SHORT_HALF {
                    if let Some(dst) = scatter_lo.get_mut(r) {
                        *dst = coord;
                    }
                } else if r >= hi_start
                    && let Some(dst) = scatter_hi.get_mut(r.wrapping_sub(hi_start))
                {
                    *dst = coord;
                }
                if let Some(dst) = gather.get_mut(lane).and_then(|row| row.get_mut(d1)) {
                    *dst = u8::try_from(q).unwrap_or(0);
                }
            }
        }
    }
    (tw_c, tw_s, scatter_lo, scatter_hi, gather)
}

fn fill_radix2_twiddles(tw_c: &mut [f64], tw_s: &mut [f64], fft_n: f64) {
    for (k, (dc, ds)) in tw_c.iter_mut().zip(tw_s.iter_mut()).enumerate() {
        let (s, c) = (-2.0 * PI * f64::from(u16::try_from(k).unwrap_or(0)) / fft_n).sin_cos();
        *dc = c;
        *ds = s;
    }
}

fn build_mdct_tables() -> MdctTables {
    let (long_tw_c, long_tw_s, long_scatter_lo, long_scatter_hi, long_gather) = build_long_tables();
    let (short_tw_c, short_tw_s, short_scatter_lo, short_scatter_hi, short_gather) =
        build_short_tables();
    let mut tw8_c = [0.0_f64; 4];
    let mut tw8_s = [0.0_f64; 4];
    let mut tw16_c = [0.0_f64; 8];
    let mut tw16_s = [0.0_f64; 8];
    let mut tw32_c = [0.0_f64; 16];
    let mut tw32_s = [0.0_f64; 16];
    fill_radix2_twiddles(&mut tw8_c, &mut tw8_s, 8.0);
    fill_radix2_twiddles(&mut tw16_c, &mut tw16_s, 16.0);
    fill_radix2_twiddles(&mut tw32_c, &mut tw32_s, 32.0);
    MdctTables {
        long_tw_c,
        long_tw_s,
        long_scatter_lo,
        long_scatter_hi,
        long_gather,
        short_tw_c,
        short_tw_s,
        short_scatter_lo,
        short_scatter_hi,
        short_gather,
        tw8_c,
        tw8_s,
        tw16_c,
        tw16_s,
        tw32_c,
        tw32_s,
    }
}

/// Forward 4-point complex DFT (`exp(-j 2 pi k n / 4)`).
#[inline]
fn fft4(r: &mut [f64; 4], i: &mut [f64; 4]) {
    let [r0, r1, r2, r3] = *r;
    let [i0, i1, i2, i3] = *i;
    let a0r = r0 + r2;
    let a0i = i0 + i2;
    let a1r = r0 - r2;
    let a1i = i0 - i2;
    let b0r = r1 + r3;
    let b0i = i1 + i3;
    let b1r = r1 - r3;
    let b1i = i1 - i3;
    *r = [a0r + b0r, a1r + b1i, a0r - b0r, a1r - b1i];
    *i = [a0i + b0i, a1i - b1r, a0i - b0i, a1i + b1r];
}

/// Applies one radix-2 butterfly stage on paired halves with linear twiddles and zero bounds checks.
#[inline]
fn radix2_stage(
    lo_r: &mut [f64],
    lo_i: &mut [f64],
    hi_r: &mut [f64],
    hi_i: &mut [f64],
    tw_c: &[f64],
    tw_s: &[f64],
) {
    for (((((r0, i0), r1), i1), &c), &s) in lo_r
        .iter_mut()
        .zip(lo_i.iter_mut())
        .zip(hi_r.iter_mut())
        .zip(hi_i.iter_mut())
        .zip(tw_c.iter())
        .zip(tw_s.iter())
    {
        let a_r = *r0;
        let a_i = *i0;
        let b_r = *r1;
        let b_i = *i1;
        let tr = b_r * c - b_i * s;
        let ti = b_r * s + b_i * c;
        *r0 = a_r + tr;
        *i0 = a_i + ti;
        *r1 = a_r - tr;
        *i1 = a_i - ti;
    }
}

/// Forward 32-point radix-2 decimation-in-time complex DFT (`exp(-j 2 pi k n / 32)`).
#[inline]
fn fft32(re: &mut [f64; 32], im: &mut [f64; 32], tbl: &MdctTables) {
    // Group into 8 natural-order stride-8 quadruples ordered by 3-bit bit-reversal for fused radix-4 stages 1-2.
    let mut br_r = [
        re[0], re[8], re[16], re[24], re[4], re[12], re[20], re[28], re[2], re[10], re[18], re[26],
        re[6], re[14], re[22], re[30], re[1], re[9], re[17], re[25], re[5], re[13], re[21], re[29],
        re[3], re[11], re[19], re[27], re[7], re[15], re[23], re[31],
    ];
    let mut br_i = [
        im[0], im[8], im[16], im[24], im[4], im[12], im[20], im[28], im[2], im[10], im[18], im[26],
        im[6], im[14], im[22], im[30], im[1], im[9], im[17], im[25], im[5], im[13], im[21], im[29],
        im[3], im[11], im[19], im[27], im[7], im[15], im[23], im[31],
    ];
    for (r4, i4) in br_r
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(br_i.as_chunks_mut::<4>().0.iter_mut())
    {
        fft4(r4, i4);
    }
    for (r8, i8) in br_r
        .as_chunks_mut::<8>()
        .0
        .iter_mut()
        .zip(br_i.as_chunks_mut::<8>().0.iter_mut())
    {
        let (r_lo, r_hi) = r8.split_at_mut(4);
        let (i_lo, i_hi) = i8.split_at_mut(4);
        radix2_stage(r_lo, i_lo, r_hi, i_hi, &tbl.tw8_c, &tbl.tw8_s);
    }
    for (r16, i16) in br_r
        .as_chunks_mut::<16>()
        .0
        .iter_mut()
        .zip(br_i.as_chunks_mut::<16>().0.iter_mut())
    {
        let (r_lo, r_hi) = r16.split_at_mut(8);
        let (i_lo, i_hi) = i16.split_at_mut(8);
        radix2_stage(r_lo, i_lo, r_hi, i_hi, &tbl.tw16_c, &tbl.tw16_s);
    }
    let (r_lo, r_hi) = br_r.split_at_mut(16);
    let (i_lo, i_hi) = br_i.split_at_mut(16);
    radix2_stage(r_lo, i_lo, r_hi, i_hi, &tbl.tw32_c, &tbl.tw32_s);
    *re = br_r;
    *im = br_i;
}

/// Applies vectorized 5-point complex DFT across 5 contiguous rows of width `W`.
#[inline]
fn fft5_rows<const W: usize>(re: &mut [[f64; W]; 5], im: &mut [[f64; W]; 5]) {
    let [r0, r1, r2, r3, r4] = re;
    let [i0, i1, i2, i3, i4] = im;
    for (((((a0, a1), (a2, a3)), a4), ((b0, b1), (b2, b3))), b4) in r0
        .iter_mut()
        .zip(r1.iter_mut())
        .zip(r2.iter_mut().zip(r3.iter_mut()))
        .zip(r4.iter_mut())
        .zip(
            i0.iter_mut()
                .zip(i1.iter_mut())
                .zip(i2.iter_mut().zip(i3.iter_mut())),
        )
        .zip(i4.iter_mut())
    {
        let (v0r, v1r, v2r, v3r, v4r) = (*a0, *a1, *a2, *a3, *a4);
        let (v0i, v1i, v2i, v3i, v4i) = (*b0, *b1, *b2, *b3, *b4);
        let t1r = v1r + v4r;
        let t1i = v1i + v4i;
        let t2r = v2r + v3r;
        let t2i = v2i + v3i;
        let t3r = v1r - v4r;
        let t3i = v1i - v4i;
        let t4r = v2r - v3r;
        let t4i = v2i - v3i;
        let m1r = v0r + COS_2PI_5 * t1r + COS_4PI_5 * t2r;
        let m1i = v0i + COS_2PI_5 * t1i + COS_4PI_5 * t2i;
        let m2r = v0r + COS_4PI_5 * t1r + COS_2PI_5 * t2r;
        let m2i = v0i + COS_4PI_5 * t1i + COS_2PI_5 * t2i;
        let p1r = SIN_2PI_5 * t3i + SIN_4PI_5 * t4i;
        let p1i = -(SIN_2PI_5 * t3r + SIN_4PI_5 * t4r);
        let p2r = SIN_4PI_5 * t3i - SIN_2PI_5 * t4i;
        let p2i = -(SIN_4PI_5 * t3r - SIN_2PI_5 * t4r);
        *a0 = v0r + t1r + t2r;
        *a1 = m1r + p1r;
        *a2 = m2r + p2r;
        *a3 = m2r - p2r;
        *a4 = m1r - p1r;
        *b0 = v0i + t1i + t2i;
        *b1 = m1i + p1i;
        *b2 = m2i + p2i;
        *b3 = m2i - p2i;
        *b4 = m1i - p1i;
    }
}

/// Applies vectorized 3-point complex DFT across 3 contiguous `[5][W]` slabs.
#[inline]
fn fft3_slabs<const W: usize>(re: &mut [[f64; W]; 15], im: &mut [[f64; W]; 15]) {
    let (r_slabs, _) = re.as_chunks_mut::<5>();
    let (i_slabs, _) = im.as_chunks_mut::<5>();
    if let ([rs0, rs1, rs2], [is0, is1, is2]) = (r_slabs, i_slabs) {
        for (((r0, r1), r2), ((i0, i1), i2)) in rs0
            .iter_mut()
            .zip(rs1.iter_mut())
            .zip(rs2.iter_mut())
            .zip(is0.iter_mut().zip(is1.iter_mut()).zip(is2.iter_mut()))
        {
            for (((a0, a1), a2), ((b0, b1), b2)) in r0
                .iter_mut()
                .zip(r1.iter_mut())
                .zip(r2.iter_mut())
                .zip(i0.iter_mut().zip(i1.iter_mut()).zip(i2.iter_mut()))
            {
                let (v0r, v1r, v2r) = (*a0, *a1, *a2);
                let (v0i, v1i, v2i) = (*b0, *b1, *b2);
                let sr = v1r + v2r;
                let si = v1i + v2i;
                let dr = SQRT3_2 * (v1r - v2r);
                let di = SQRT3_2 * (v1i - v2i);
                let mr = v0r - 0.5 * sr;
                let mi = v0i - 0.5 * si;
                *a0 = v0r + sr;
                *a1 = mr + di;
                *a2 = mr - di;
                *b0 = v0i + si;
                *b1 = mi - dr;
                *b2 = mi + dr;
            }
        }
    }
}

/// Computes unscaled `N = 960` DCT-IV via `[3][5][32]` Good-Thomas PFA FFT with zero transposes.
fn dct4_960(spec: &[f64; N_LONG], out: &mut [f64; N_LONG]) {
    let tbl = &*MDCT_TABLES;
    let mut c_re = [[0.0_f64; 32]; 15];
    let mut c_im = [[0.0_f64; 32]; 15];
    // Wideband `spec[320..960]` is zero, so pre-twiddle scatters directly into the 320 active `[15][32]` slots.
    if let (Some(sp_act), Some(c_lo), Some(s_lo)) = (
        spec.get(..WB_ACTIVE_BINS),
        tbl.long_tw_c.get(..WB_ACTIVE_HALF),
        tbl.long_tw_s.get(..WB_ACTIVE_HALF),
    ) {
        for (((pair, &(lane, d1)), &c), &s) in sp_act
            .as_chunks::<2>()
            .0
            .iter()
            .zip(tbl.long_scatter_lo.iter())
            .zip(c_lo.iter())
            .zip(s_lo.iter())
        {
            let x_e = pair[0];
            if let (Some(dr), Some(di)) = (
                c_re.get_mut(usize::from(lane))
                    .and_then(|row| row.get_mut(usize::from(d1))),
                c_im.get_mut(usize::from(lane))
                    .and_then(|row| row.get_mut(usize::from(d1))),
            ) {
                *dr = x_e * c;
                *di = -x_e * s;
            }
        }
    }
    let hi_start = P_LONG.wrapping_sub(WB_ACTIVE_HALF);
    if let (Some(sp_act), Some(c_hi), Some(s_hi)) = (
        spec.get(..WB_ACTIVE_BINS),
        tbl.long_tw_c.get(hi_start..),
        tbl.long_tw_s.get(hi_start..),
    ) {
        for (((pair, &(lane, d1)), &c), &s) in sp_act
            .as_chunks::<2>()
            .0
            .iter()
            .rev()
            .zip(tbl.long_scatter_hi.iter())
            .zip(c_hi.iter())
            .zip(s_hi.iter())
        {
            let x_o = pair[1];
            if let (Some(dr), Some(di)) = (
                c_re.get_mut(usize::from(lane))
                    .and_then(|row| row.get_mut(usize::from(d1))),
                c_im.get_mut(usize::from(lane))
                    .and_then(|row| row.get_mut(usize::from(d1))),
            ) {
                *dr = x_o * s;
                *di = x_o * c;
            }
        }
    }

    // Axis 1: 15 contiguous 32-point FFTs; Axis 3: vectorized 5-point; Axis 2: vectorized 3-point.
    for (r32, i32_arr) in c_re.iter_mut().zip(c_im.iter_mut()) {
        fft32(r32, i32_arr, tbl);
    }
    for (r5, i5) in c_re
        .as_chunks_mut::<5>()
        .0
        .iter_mut()
        .zip(c_im.as_chunks_mut::<5>().0.iter_mut())
    {
        fft5_rows(r5, i5);
    }
    fft3_slabs(&mut c_re, &mut c_im);

    for ((g_row, r_row), i_row) in tbl.long_gather.iter().zip(c_re.iter()).zip(c_im.iter()) {
        for ((&q_u16, &vr), &vi) in g_row.iter().zip(r_row.iter()).zip(i_row.iter()) {
            let q = usize::from(q_u16);
            let c = tbl.long_tw_c.get(q).copied().unwrap_or(1.0);
            let s = tbl.long_tw_s.get(q).copied().unwrap_or(0.0);
            let yr = vr * c + vi * s;
            let yi = vi * c - vr * s;
            if let Some(de) = out.get_mut(q.wrapping_mul(2)) {
                *de = yr;
            }
            if let Some(do_slot) =
                out.get_mut(N_LONG.wrapping_sub(1).wrapping_sub(q.wrapping_mul(2)))
            {
                *do_slot = -yi;
            }
        }
    }
}

/// Computes unscaled `N = 120` DCT-IV via `[3][5][4]` Good-Thomas PFA FFT over wideband bins `0..40`.
fn dct4_120_wb(sub_spec: &[f64; WB_SHORT_ACTIVE], out: &mut [f64; N_SHORT]) {
    let tbl = &*MDCT_TABLES;
    let mut c_re = [[0.0_f64; 4]; 15];
    let mut c_im = [[0.0_f64; 4]; 15];
    if let (Some(c_lo), Some(s_lo)) = (
        tbl.short_tw_c.get(..WB_SHORT_HALF),
        tbl.short_tw_s.get(..WB_SHORT_HALF),
    ) {
        for (((pair, &(lane, d1)), &c), &s) in sub_spec
            .as_chunks::<2>()
            .0
            .iter()
            .zip(tbl.short_scatter_lo.iter())
            .zip(c_lo.iter())
            .zip(s_lo.iter())
        {
            let x_e = pair[0];
            if let (Some(dr), Some(di)) = (
                c_re.get_mut(usize::from(lane))
                    .and_then(|row| row.get_mut(usize::from(d1))),
                c_im.get_mut(usize::from(lane))
                    .and_then(|row| row.get_mut(usize::from(d1))),
            ) {
                *dr = x_e * c;
                *di = -x_e * s;
            }
        }
    }
    let hi_start = P_SHORT.wrapping_sub(WB_SHORT_HALF);
    if let (Some(c_hi), Some(s_hi)) = (
        tbl.short_tw_c.get(hi_start..),
        tbl.short_tw_s.get(hi_start..),
    ) {
        for (((pair, &(lane, d1)), &c), &s) in sub_spec
            .as_chunks::<2>()
            .0
            .iter()
            .rev()
            .zip(tbl.short_scatter_hi.iter())
            .zip(c_hi.iter())
            .zip(s_hi.iter())
        {
            let x_o = pair[1];
            if let (Some(dr), Some(di)) = (
                c_re.get_mut(usize::from(lane))
                    .and_then(|row| row.get_mut(usize::from(d1))),
                c_im.get_mut(usize::from(lane))
                    .and_then(|row| row.get_mut(usize::from(d1))),
            ) {
                *dr = x_o * s;
                *di = x_o * c;
            }
        }
    }

    for (r4, i4) in c_re.iter_mut().zip(c_im.iter_mut()) {
        fft4(r4, i4);
    }
    for (r5, i5) in c_re
        .as_chunks_mut::<5>()
        .0
        .iter_mut()
        .zip(c_im.as_chunks_mut::<5>().0.iter_mut())
    {
        fft5_rows(r5, i5);
    }
    fft3_slabs(&mut c_re, &mut c_im);

    for ((g_row, r_row), i_row) in tbl.short_gather.iter().zip(c_re.iter()).zip(c_im.iter()) {
        for ((&q_u8, &vr), &vi) in g_row.iter().zip(r_row.iter()).zip(i_row.iter()) {
            let q = usize::from(q_u8);
            let c = tbl.short_tw_c.get(q).copied().unwrap_or(1.0);
            let s = tbl.short_tw_s.get(q).copied().unwrap_or(0.0);
            let yr = vr * c + vi * s;
            let yi = vi * c - vr * s;
            if let Some(de) = out.get_mut(q.wrapping_mul(2)) {
                *de = yr;
            }
            if let Some(do_slot) =
                out.get_mut(N_SHORT.wrapping_sub(1).wrapping_sub(q.wrapping_mul(2)))
            {
                *do_slot = -yi;
            }
        }
    }
}

/// Synthesizes one 960-sample frame into `out` and updates the 120-sample `overlap` buffer (RFC 6716 Section 4.3.7).
pub(crate) fn synthesize(
    spec: &[f64; N_LONG],
    is_transient: bool,
    overlap: &mut [f64; OVERLAP_48K],
    out: &mut [f64; N_LONG],
) {
    let win = &tables().window_48k;
    if is_transient {
        synthesize_transient(spec, win, overlap, out);
    } else {
        synthesize_long(spec, win, overlap, out);
    }
}

/// Long-block 960-sample IMDCT with 120-sample low-overlap window.
fn synthesize_long(
    spec: &[f64; N_LONG],
    win: &[f64; OVERLAP_48K],
    overlap: &mut [f64; OVERLAP_48K],
    out: &mut [f64; N_LONG],
) {
    let half_ov = OVERLAP_48K / 2;
    let mut dct = [0.0_f64; N_LONG];
    dct4_960(spec, &mut dct);
    let (w_lo, w_hi) = win.split_at(half_ov);
    let (ov_lo, ov_hi) = overlap.split_at_mut(half_ov);
    let (out_ov, out_flat) = out.split_at_mut(OVERLAP_48K);
    let (out_lo, out_hi) = out_ov.split_at_mut(half_ov);
    if let Some(dct_tail) = dct.get(N_LONG.wrapping_sub(half_ov)..) {
        for (((d0, &ov0), &w0), &c_head) in out_lo
            .iter_mut()
            .zip(ov_lo.iter())
            .zip(w_lo.iter())
            .zip(dct_tail.iter())
        {
            *d0 = ov0 + w0 * c_head;
        }
        for (((d1, &ov1), &w1), &c_mid) in out_hi
            .iter_mut()
            .zip(ov_hi.iter())
            .zip(w_hi.iter())
            .zip(dct_tail.iter().rev())
        {
            *d1 = ov1 - w1 * c_mid;
        }
    }
    if let Some(dct_mid) = dct.get(half_ov..N_LONG.wrapping_sub(half_ov)) {
        for (dst, &c_val) in out_flat.iter_mut().zip(dct_mid.iter().rev()) {
            *dst = -c_val;
        }
    }
    if let Some(dct_head) = dct.get(..half_ov) {
        for ((d0, &wh), &c_a) in ov_lo
            .iter_mut()
            .zip(w_hi.iter().rev())
            .zip(dct_head.iter().rev())
        {
            *d0 = -wh * c_a;
        }
        for ((d1, &wl), &c_b) in ov_hi.iter_mut().zip(w_lo.iter().rev()).zip(dct_head.iter()) {
            *d1 = -wl * c_b;
        }
    }
}

/// Transient frame: 8 short 120-sample IMDCTs overlapped by 120 samples each (RFC 6716 Section 4.3.7).
fn synthesize_transient(
    spec: &[f64; N_LONG],
    win: &[f64; OVERLAP_48K],
    overlap: &mut [f64; OVERLAP_48K],
    out: &mut [f64; N_LONG],
) {
    let half_ov = OVERLAP_48K / 2;
    out.fill(0.0);
    if let Some(head) = out.get_mut(..OVERLAP_48K) {
        head.copy_from_slice(overlap);
    }
    overlap.fill(0.0);
    let mut sub_spec = [0.0_f64; WB_SHORT_ACTIVE];
    let mut dct = [0.0_f64; N_SHORT];
    let (w_lo, w_hi) = win.split_at(half_ov);
    let active_spec = spec.get(..WB_ACTIVE_BINS).unwrap_or(&[]);
    for blk in 0..M_BLOCKS {
        for (dst, row) in sub_spec
            .iter_mut()
            .zip(active_spec.as_chunks::<M_BLOCKS>().0.iter())
        {
            *dst = row.get(blk).copied().unwrap_or(0.0);
        }
        dct4_120_wb(&sub_spec, &mut dct);
        let (dct_lo, dct_hi) = dct.split_at(half_ov);
        let base = blk.wrapping_mul(N_SHORT);
        if let Some(seg) = out.get_mut(base..base.wrapping_add(N_SHORT)) {
            let (s0, s1) = seg.split_at_mut(half_ov);
            for ((d, &w), &c) in s0.iter_mut().zip(w_lo.iter()).zip(dct_hi.iter()) {
                *d += w * c;
            }
            for ((d, &w), &c) in s1.iter_mut().zip(w_hi.iter()).zip(dct_hi.iter().rev()) {
                *d -= w * c;
            }
        }
        let next_base = base.wrapping_add(N_SHORT);
        let tail_seg = if blk.wrapping_add(1) < M_BLOCKS {
            out.get_mut(next_base..next_base.wrapping_add(N_SHORT))
        } else {
            Some(&mut overlap[..])
        };
        if let Some(seg) = tail_seg {
            let (s2, s3) = seg.split_at_mut(half_ov);
            for ((d, &w), &c) in s2
                .iter_mut()
                .zip(w_hi.iter().rev())
                .zip(dct_lo.iter().rev())
            {
                *d -= w * c;
            }
            for ((d, &w), &c) in s3.iter_mut().zip(w_lo.iter().rev()).zip(dct_lo.iter()) {
                *d -= w * c;
            }
        }
    }
}
