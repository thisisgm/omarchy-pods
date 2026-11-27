//! ER AAC-ELD 480 synthesis (ISO/IEC 14496-3 Clause 4.6.11.3.4): `f64` DCT-IV via a 240-point Good-Thomas FFT, then the 1800-tap window.

use std::f64::consts::{FRAC_1_SQRT_2, PI};
use std::sync::LazyLock;

use crate::SAMPLES_PER_UNIT;
use crate::tables::window::ELD_WINDOW_480;

const M: usize = SAMPLES_PER_UNIT; // 480
const P: usize = M / 2; // 240
const OVERLAP_LEN: usize = 3 * M; // 1440

/// Precomputed pre/post DCT-IV twiddles and CRT scatter/gather indices for `P = 240`.
struct ImcdtTables {
    pre_cos: [f64; P],
    pre_sin: [f64; P],
    post_cos: [f64; P],
    post_sin: [f64; P],
    /// `scatter_r[idx] = (15*r1 + 80*r2 + 48*r3) mod 240` for `idx = r1*15 + r2*5 + r3`.
    scatter_r: [u8; P],
    /// `gather_q[idx] = (225*q1 + 160*q2 + 96*q3) mod 240` for `idx = q1*15 + q2*5 + q3`.
    gather_q: [u8; P],
}

static IMDCT_TABLES: LazyLock<ImcdtTables> = LazyLock::new(|| {
    let mut pre_cos = [0.0_f64; P];
    let mut pre_sin = [0.0_f64; P];
    let mut post_cos = [0.0_f64; P];
    let mut post_sin = [0.0_f64; P];
    let mut scatter_r = [0_u8; P];
    let mut gather_q = [0_u8; P];
    let scale = 1.0 / 480.0;
    let step = PI / 480.0;
    for r in 0..P {
        let angle = step * (f64::from(u16::try_from(r).unwrap_or(0)) + 0.125);
        let (s, c) = angle.sin_cos();
        if let (Some(pc), Some(ps), Some(qc), Some(qs)) = (
            pre_cos.get_mut(r),
            pre_sin.get_mut(r),
            post_cos.get_mut(r),
            post_sin.get_mut(r),
        ) {
            *pc = c;
            *ps = s;
            *qc = c * scale;
            *qs = s * scale;
        }
    }
    for d1 in 0..16_usize {
        let base1 = d1.wrapping_mul(15);
        for d2 in 0..3_usize {
            let base2 = base1.wrapping_add(d2.wrapping_mul(5));
            for d3 in 0..5_usize {
                let idx = base2.wrapping_add(d3);
                let r = d1
                    .wrapping_mul(15)
                    .wrapping_add(d2.wrapping_mul(80))
                    .wrapping_add(d3.wrapping_mul(48))
                    .wrapping_rem(P);
                let q = d1
                    .wrapping_mul(225)
                    .wrapping_add(d2.wrapping_mul(160))
                    .wrapping_add(d3.wrapping_mul(96))
                    .wrapping_rem(P);
                if let (Some(sr), Some(gq)) = (scatter_r.get_mut(idx), gather_q.get_mut(idx)) {
                    *sr = u8::try_from(r).unwrap_or(0);
                    *gq = u8::try_from(q).unwrap_or(0);
                }
            }
        }
    }
    ImcdtTables {
        pre_cos,
        pre_sin,
        post_cos,
        post_sin,
        scatter_r,
        gather_q,
    }
});

/// Preallocated ELD-480 synthesis filterbank with 3-frame overlap history (zero heap allocations per frame).
#[derive(Debug, Clone)]
pub(crate) struct Filterbank {
    overlap: [f64; OVERLAP_LEN],
    buf_re: [f64; P],
    buf_im: [f64; P],
    dct: [f64; M],
    imdct_half: [f64; 2 * M],
}

impl Filterbank {
    #[must_use]
    pub(crate) fn new() -> Self {
        // Force one-time initialization of twiddle tables during decoder construction.
        let _ = &*IMDCT_TABLES;
        Self {
            overlap: [0.0; OVERLAP_LEN],
            buf_re: [0.0; P],
            buf_im: [0.0; P],
            dct: [0.0; M],
            imdct_half: [0.0; 2 * M],
        }
    }

    pub(crate) fn reset(&mut self) {
        self.overlap.fill(0.0);
    }

    /// Synthesizes one 480-sample frame from `spec` into `out` and advances the 3-frame overlap buffer.
    pub(crate) fn synthesize(&mut self, spec: &[f64; M], out: &mut [f32; M]) {
        self.compute_dct4(spec);
        self.expand_eld_imdct();
        self.window_and_overlap_add(out);
    }

    /// Computes `C[m] = (1/480) * sum_{k=0}^{479} spec[k] * cos(pi/480 * (m + 0.5) * (k + 0.5))`.
    fn compute_dct4(&mut self, spec: &[f64; M]) {
        let tbl = &*IMDCT_TABLES;

        // 1. Pre-rotate and Good-Thomas CRT scatter into `[16][3][5]` using precomputed indices.
        for ((&r_u8, dst_r), dst_i) in tbl
            .scatter_r
            .iter()
            .zip(self.buf_re.iter_mut())
            .zip(self.buf_im.iter_mut())
        {
            let r = usize::from(r_u8);
            let k_even = r.wrapping_mul(2);
            let k_odd = (M - 1).wrapping_sub(k_even);
            if let (Some(&xe), Some(&xo), Some(&c), Some(&s)) = (
                spec.get(k_even),
                spec.get(k_odd),
                tbl.pre_cos.get(r),
                tbl.pre_sin.get(r),
            ) {
                // (xe - i * xo) * (c + i * s)
                *dst_r = xe * c + xo * s;
                *dst_i = xe * s - xo * c;
            }
        }

        // 2. Fifteen 16-point positive-exponent FFTs along axis 0 (stride 15).
        for col in 0..15_usize {
            fft16_strided(&mut self.buf_re, &mut self.buf_im, col);
        }

        // 3. Eighty 3-point positive-exponent FFTs along axis 1 (stride 5 within each of 16 blocks).
        for r1 in 0..16_usize {
            let base = r1.wrapping_mul(15);
            for r3 in 0..5_usize {
                fft3_strided(&mut self.buf_re, &mut self.buf_im, base.wrapping_add(r3));
            }
        }

        // 4. Forty-eight 5-point positive-exponent FFTs along axis 2 (contiguous chunks of 5).
        for (re5, im5) in self
            .buf_re
            .as_chunks_mut::<5>()
            .0
            .iter_mut()
            .zip(self.buf_im.as_chunks_mut::<5>().0.iter_mut())
        {
            fft5_contiguous(re5, im5);
        }

        // 5. Good-Thomas CRT gather and post-rotate using precomputed indices.
        for ((&q_u8, &yr), &yi) in tbl
            .gather_q
            .iter()
            .zip(self.buf_re.iter())
            .zip(self.buf_im.iter())
        {
            let q = usize::from(q_u8);
            let m_even = q.wrapping_mul(2);
            let m_odd = (M - 1).wrapping_sub(m_even);
            if let (Some(&c), Some(&s)) = (tbl.post_cos.get(q), tbl.post_sin.get(q)) {
                // (yr + i * yi) * (c + i * s)
                if let Some(dst_e) = self.dct.get_mut(m_even) {
                    *dst_e = yr * c - yi * s;
                }
                if let Some(dst_o) = self.dct.get_mut(m_odd) {
                    *dst_o = yr * s + yi * c;
                }
            }
        }
    }

    /// Expands the DCT-IV into the first 960 IMDCT samples (`n0 = 840.5`); the next 960 are `x[n + 960] = -x[n]`.
    fn expand_eld_imdct(&mut self) {
        let (seg0, rest) = self.imdct_half.split_at_mut(120);
        let (seg1, seg2) = rest.split_at_mut(M);
        // n in 0..120: x[n] = -C[119 - n]
        if let Some(src0) = self.dct.get(0..120) {
            for (dst, &c) in seg0.iter_mut().zip(src0.iter().rev()) {
                *dst = -c;
            }
        }
        // n in 120..600: x[n] = -C[n - 120]
        for (dst, &c) in seg1.iter_mut().zip(self.dct.iter()) {
            *dst = -c;
        }
        // n in 600..960: x[n] = +C[1079 - n]
        if let Some(src2) = self.dct.get(120..M) {
            for (dst, &c) in seg2.iter_mut().zip(src2.iter().rev()) {
                *dst = c;
            }
        }
    }

    /// Applies the 1800-sample ELD-480 window and updates the 3-frame (1440-sample) overlap buffer.
    fn window_and_overlap_add(&mut self, out: &mut [f32; M]) {
        let (x0, x1) = self.imdct_half.split_at(M);
        let (w0, w_rest) = ELD_WINDOW_480.split_at(M);
        let (w1, w_rest) = w_rest.split_at(M);
        let (w2, w3) = w_rest.split_at(M);
        let (ov0, ov_rest) = self.overlap.split_at_mut(M);
        let (ov1, ov2) = ov_rest.split_at_mut(M);

        // Frame 0 (0..480): output = x[n] * w[n] + overlap[n]
        for (((dst, &x), &w), &ov) in out.iter_mut().zip(x0).zip(w0).zip(ov0.iter()) {
            #[expect(
                clippy::as_conversions,
                clippy::cast_possible_truncation,
                reason = "final f64-to-f32 sample conversion after double-precision synthesis"
            )]
            {
                *dst = (x * w + ov) as f32;
            }
        }

        // Frame 1 (480..960): new ov0 = x1 * w1 + old ov1
        for (((dst, &x), &w), &prev) in ov0.iter_mut().zip(x1).zip(w1).zip(ov1.iter()) {
            *dst = x * w + prev;
        }

        // Frame 2 (960..1440): new ov1 = (-x0) * w2 + old ov2
        for (((dst, &x), &w), &prev) in ov1.iter_mut().zip(x0).zip(w2).zip(ov2.iter()) {
            *dst = -x * w + prev;
        }

        // Frame 3 (1440..1800): new ov2[..360] = (-x1[..360]) * w3; ov2[360..] = 0
        let (ov2_head, ov2_tail) = ov2.split_at_mut(360);
        for ((dst, &x), &w) in ov2_head.iter_mut().zip(x1).zip(w3) {
            *dst = -x * w;
        }
        ov2_tail.fill(0.0);
    }
}

/// 3-point positive-exponent DFT (`exp(+i * 2*pi/3 * k * m)`) at stride 5.
#[inline]
fn fft3_strided(re: &mut [f64; P], im: &mut [f64; P], start: usize) {
    // sin(2*pi/3) = sqrt(3)/2
    const S3: f64 = 0.866_025_403_784_438_6;
    let i1 = start.wrapping_add(5);
    let i2 = start.wrapping_add(10);
    let (Some(&r0), Some(&r1), Some(&r2)) = (re.get(start), re.get(i1), re.get(i2)) else {
        return;
    };
    let (Some(&m0), Some(&m1), Some(&m2)) = (im.get(start), im.get(i1), im.get(i2)) else {
        return;
    };
    let t1r = r1 + r2;
    let t1i = m1 + m2;
    let t2r = r0 - 0.5 * t1r;
    let t2i = m0 - 0.5 * t1i;
    let t3r = S3 * (r1 - r2);
    let t3i = S3 * (m1 - m2);
    if let Some(d0r) = re.get_mut(start) {
        *d0r = r0 + t1r;
    }
    if let Some(d0i) = im.get_mut(start) {
        *d0i = m0 + t1i;
    }
    if let Some(d1r) = re.get_mut(i1) {
        *d1r = t2r - t3i;
    }
    if let Some(d1i) = im.get_mut(i1) {
        *d1i = t2i + t3r;
    }
    if let Some(d2r) = re.get_mut(i2) {
        *d2r = t2r + t3i;
    }
    if let Some(d2i) = im.get_mut(i2) {
        *d2i = t2i - t3r;
    }
}

/// 5-point positive-exponent Winograd DFT (`exp(+i * 2*pi/5 * k * m)`) on an array of length 5.
#[inline]
fn fft5_contiguous(re: &mut [f64; 5], im: &mut [f64; 5]) {
    // C2 = (cos(2pi/5) - cos(4pi/5)) / 2 = sqrt(5)/4; the matching sum term is the exact -1/4 below.
    const C2: f64 = 0.559_016_994_374_947_5;
    const S1: f64 = 0.951_056_516_295_153_5; // sin(2*pi/5)
    const S2: f64 = 0.587_785_252_292_473_1; // sin(4*pi/5)

    let [r0, r1, r2, r3, r4] = *re;
    let [m0, m1, m2, m3, m4] = *im;

    let t1r = r1 + r4;
    let t1i = m1 + m4;
    let t2r = r2 + r3;
    let t2i = m2 + m3;
    let t3r = r1 - r4;
    let t3i = m1 - m4;
    let t4r = r2 - r3;
    let t4i = m2 - m3;

    let t5r = t1r + t2r;
    let t5i = t1i + t2i;
    let m1r = r0 - 0.25 * t5r;
    let m1i = m0 - 0.25 * t5i;
    let m2r = C2 * (t1r - t2r);
    let m2i = C2 * (t1i - t2i);

    let a1r = m1r + m2r;
    let a1i = m1i + m2i;
    let a2r = m1r - m2r;
    let a2i = m1i - m2i;

    let b1r = S1 * t3r + S2 * t4r;
    let b1i = S1 * t3i + S2 * t4i;
    let b2r = S2 * t3r - S1 * t4r;
    let b2i = S2 * t3i - S1 * t4i;

    // exp(+i*theta) => multiply imaginary part of odd sum by +i: (a) + i*(b) = (ar - bi) + i*(ai + br)
    *re = [r0 + t5r, a1r - b1i, a2r - b2i, a2r + b2i, a1r + b1i];
    *im = [m0 + t5i, a1i + b1r, a2i + b2r, a2i - b2r, a1i - b1r];
}

/// 16-point positive-exponent radix-4 DIT FFT (`exp(+i * 2*pi/16 * k * m)`) at stride 15.
#[inline]
fn fft16_strided(re: &mut [f64; P], im: &mut [f64; P], col: usize) {
    const C8_1: f64 = 0.923_879_532_511_286_7; // cos(pi/8)
    const S8_1: f64 = 0.382_683_432_365_089_8; // sin(pi/8)

    let mut xr = [0.0_f64; 16];
    let mut xi = [0.0_f64; 16];
    for (k, (dr, di)) in xr.iter_mut().zip(xi.iter_mut()).enumerate() {
        let idx = k.wrapping_mul(15).wrapping_add(col);
        if let (Some(&r), Some(&i)) = (re.get(idx), im.get(idx)) {
            *dr = r;
            *di = i;
        }
    }

    // Stage 1: four 4-point positive-exponent DFTs on radix-4 groups `(g, g+4, g+8, g+12)`.
    let g0 = dft4_pos([
        (xr[0], xi[0]),
        (xr[4], xi[4]),
        (xr[8], xi[8]),
        (xr[12], xi[12]),
    ]);
    let g1 = dft4_pos([
        (xr[1], xi[1]),
        (xr[5], xi[5]),
        (xr[9], xi[9]),
        (xr[13], xi[13]),
    ]);
    let g2 = dft4_pos([
        (xr[2], xi[2]),
        (xr[6], xi[6]),
        (xr[10], xi[10]),
        (xr[14], xi[14]),
    ]);
    let g3 = dft4_pos([
        (xr[3], xi[3]),
        (xr[7], xi[7]),
        (xr[11], xi[11]),
        (xr[15], xi[15]),
    ]);

    // Stage 2: twiddles W_16^{+k*g} = exp(+i * pi/8 * k * g), then four 4-point positive-exponent DFTs.
    let row0 = dft4_pos([g0[0], g1[0], g2[0], g3[0]]);
    let row1 = dft4_pos([
        g0[1],
        cmul(g1[1], C8_1, S8_1),
        cmul(g2[1], FRAC_1_SQRT_2, FRAC_1_SQRT_2),
        cmul(g3[1], S8_1, C8_1),
    ]);
    let row2 = dft4_pos([
        g0[2],
        cmul(g1[2], FRAC_1_SQRT_2, FRAC_1_SQRT_2),
        (-g2[2].1, g2[2].0),
        cmul(g3[2], -FRAC_1_SQRT_2, FRAC_1_SQRT_2),
    ]);
    let row3 = dft4_pos([
        g0[3],
        cmul(g1[3], S8_1, C8_1),
        cmul(g2[3], -FRAC_1_SQRT_2, FRAC_1_SQRT_2),
        cmul(g3[3], -C8_1, -S8_1),
    ]);

    let out = [
        row0[0], row1[0], row2[0], row3[0], row0[1], row1[1], row2[1], row3[1], row0[2], row1[2],
        row2[2], row3[2], row0[3], row1[3], row2[3], row3[3],
    ];
    for (k, &(r, i)) in out.iter().enumerate() {
        let idx = k.wrapping_mul(15).wrapping_add(col);
        if let (Some(dr), Some(di)) = (re.get_mut(idx), im.get_mut(idx)) {
            *dr = r;
            *di = i;
        }
    }
}

#[inline]
fn cmul(a: (f64, f64), br: f64, bi: f64) -> (f64, f64) {
    (a.0 * br - a.1 * bi, a.0 * bi + a.1 * br)
}

/// 4-point positive-exponent DFT (`exp(+i * pi/2 * k * m)`).
#[inline]
fn dft4_pos(x: [(f64, f64); 4]) -> [(f64, f64); 4] {
    let [x0, x1, x2, x3] = x;
    let t0r = x0.0 + x2.0;
    let t0i = x0.1 + x2.1;
    let t1r = x0.0 - x2.0;
    let t1i = x0.1 - x2.1;
    let t2r = x1.0 + x3.0;
    let t2i = x1.1 + x3.1;
    // +i * (x1 - x3) = (-(x1.i - x3.i), x1.r - x3.r)
    let t3r = x3.1 - x1.1;
    let t3i = x1.0 - x3.0;
    [
        (t0r + t2r, t0i + t2i),
        (t1r + t3r, t1i + t3i),
        (t0r - t2r, t0i - t2i),
        (t1r - t3r, t1i - t3i),
    ]
}
