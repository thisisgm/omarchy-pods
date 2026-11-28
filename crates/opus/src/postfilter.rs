//! RFC 6716 Section 4.3.7.1 pitch comb post-filter and Section 4.3.7.2 de-emphasis filter.

use crate::Error;
use crate::mdct::N_LONG;
use crate::tables::{CELT_PCM_SCALE, DEEMPHASIS_ALPHA, OVERLAP_48K, POSTFILTER_TAPS, tables};

/// Post-filter history length (`MAX_PERIOD = 1024`, RFC 6716 Section 4.3.7.1).
pub(crate) const POSTFILTER_HIST: usize = 1024;
/// Buffer size holding `1024` past samples plus `960` current frame samples.
const BUF_LEN: usize = POSTFILTER_HIST + N_LONG;

/// Parameters decoded from the CELT frame header for the pitch comb post-filter.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PostfilterParams {
    pub(crate) period: usize,
    pub(crate) gain: f64,
    pub(crate) tapset: usize,
}

/// Persistent time-domain state for post-filtering and de-emphasis across frames.
#[derive(Debug, Clone)]
pub(crate) struct PostfilterState {
    buf: [f64; BUF_LEN],
    prev_period: usize,
    prev_gain: f64,
    prev_tapset: usize,
    deemph_mem: f64,
}

impl PostfilterState {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            buf: [0.0; BUF_LEN],
            prev_period: 15,
            prev_gain: 0.0,
            prev_tapset: 0,
            deemph_mem: 0.0,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.buf.fill(0.0);
        self.prev_period = 15;
        self.prev_gain = 0.0;
        self.prev_tapset = 0;
        self.deemph_mem = 0.0;
    }

    /// Returns the most recent `POSTFILTER_HIST` pre-de-emphasis samples for PLC pitch search.
    #[inline]
    #[must_use]
    pub(crate) fn history(&self) -> &[f64] {
        self.buf.get(..POSTFILTER_HIST).unwrap_or(&[])
    }

    #[cfg(test)]
    pub(crate) fn set_deemph_mem_for_test(&mut self, value: f64) {
        self.deemph_mem = value;
    }

    #[cfg(test)]
    pub(crate) const fn deemph_mem_for_test(&self) -> f64 {
        self.deemph_mem
    }

    #[cfg(test)]
    pub(crate) fn state_for_test(&self) -> (usize, u64, usize, u64) {
        (
            self.prev_period,
            self.prev_gain.to_bits(),
            self.prev_tapset,
            self.deemph_mem.to_bits(),
        )
    }

    /// Applies pitch comb post-filter and de-emphasis, committing persistent state only when all output `f32` samples are finite.
    pub(crate) fn try_apply(
        &mut self,
        frame_in: &[f64; N_LONG],
        params: PostfilterParams,
        pcm_out: &mut [f32; N_LONG],
    ) -> Result<(), Error> {
        // Copy new IMDCT frame into scratch tail `buf[POSTFILTER_HIST..BUF_LEN]` without touching persistent history `0..POSTFILTER_HIST`.
        if let Some(dst) = self.buf.get_mut(POSTFILTER_HIST..BUF_LEN) {
            dst.copy_from_slice(frame_in);
        }

        let period = params.period.clamp(15, 1022);
        let tapset = params.tapset.min(2);
        let gain = params.gain;
        let prev_period = self.prev_period.clamp(15, 1022);
        let prev_tapset = self.prev_tapset.min(2);
        let prev_gain = self.prev_gain;

        let g_new = POSTFILTER_TAPS.get(tapset).copied().unwrap_or([0.0; 3]);
        let g_old = POSTFILTER_TAPS
            .get(prev_tapset)
            .copied()
            .unwrap_or([0.0; 3]);
        let win_sq = &tables().window_48k_sq;

        // Samples `0..120` continue the previous frame's steady-state post-filter (aligned to overlap shift).
        if prev_gain != 0.0 {
            for i in 0..OVERLAP_48K {
                let idx = POSTFILTER_HIST.wrapping_add(i);
                let x = self.buf.get(idx).copied().unwrap_or(0.0);
                let y = x + prev_gain * comb_tap(&self.buf, idx, prev_period, &g_old);
                if let Some(dst) = self.buf.get_mut(idx) {
                    *dst = y;
                }
            }
        }

        // Samples `120..240` crossfade from previous to new post-filter params using `window^2`.
        let xfade_end = OVERLAP_48K.wrapping_mul(2);
        if prev_gain != 0.0 || gain != 0.0 {
            let same_shape = period == prev_period && tapset == prev_tapset;
            for i in 0..OVERLAP_48K {
                let idx = POSTFILTER_HIST.wrapping_add(OVERLAP_48K).wrapping_add(i);
                let x = self.buf.get(idx).copied().unwrap_or(0.0);
                let w = win_sq.get(i).copied().unwrap_or(0.0);
                let y = if same_shape {
                    let g = (1.0 - w) * prev_gain + w * gain;
                    x + g * comb_tap(&self.buf, idx, period, &g_new)
                } else {
                    let y_old = if prev_gain == 0.0 {
                        0.0
                    } else {
                        (1.0 - w) * prev_gain * comb_tap(&self.buf, idx, prev_period, &g_old)
                    };
                    let y_new = if gain == 0.0 {
                        0.0
                    } else {
                        w * gain * comb_tap(&self.buf, idx, period, &g_new)
                    };
                    x + y_old + y_new
                };
                if let Some(dst) = self.buf.get_mut(idx) {
                    *dst = y;
                }
            }
            // Samples `240..960` run steady-state new post-filter parameters.
            if gain != 0.0 {
                for i in xfade_end..N_LONG {
                    let idx = POSTFILTER_HIST.wrapping_add(i);
                    let x = self.buf.get(idx).copied().unwrap_or(0.0);
                    let y = x + gain * comb_tap(&self.buf, idx, period, &g_new);
                    if let Some(dst) = self.buf.get_mut(idx) {
                        *dst = y;
                    }
                }
            }
        }

        // De-emphasis IIR `y[n] = x[n] + alpha * y[n-1]` staged into `staged_pcm` so `Err(NonFiniteOutput)` commits nothing.
        let mut staged_pcm = [0.0_f32; N_LONG];
        let mut mem = self.deemph_mem;
        let mut all_finite = true;
        if let Some(cur) = self.buf.get(POSTFILTER_HIST..BUF_LEN) {
            for (dst, &x) in staged_pcm.iter_mut().zip(cur.iter()) {
                let y = x + DEEMPHASIS_ALPHA * mem;
                mem = y;
                let sample = narrow_f32(y * CELT_PCM_SCALE);
                if !sample.is_finite() {
                    all_finite = false;
                }
                *dst = sample;
            }
        }
        if !all_finite || !mem.is_finite() {
            return Err(Error::NonFiniteOutput);
        }

        pcm_out.copy_from_slice(&staged_pcm);
        self.prev_period = period;
        self.prev_gain = gain;
        self.prev_tapset = tapset;
        self.deemph_mem = mem;
        self.buf.copy_within(N_LONG..BUF_LEN, 0);
        Ok(())
    }

    /// Runs de-emphasis on a concealed frame, committing persistent state only when all output `f32` samples are finite.
    pub(crate) fn try_apply_concealed(
        &mut self,
        frame_in: &[f64; N_LONG],
        pcm_out: &mut [f32; N_LONG],
    ) -> Result<(), Error> {
        let mut staged_pcm = [0.0_f32; N_LONG];
        let mut mem = self.deemph_mem;
        let mut all_finite = true;
        for (dst, &x) in staged_pcm.iter_mut().zip(frame_in.iter()) {
            let y = x + DEEMPHASIS_ALPHA * mem;
            mem = y;
            let sample = narrow_f32(y * CELT_PCM_SCALE);
            if !sample.is_finite() {
                all_finite = false;
            }
            *dst = sample;
        }
        if !all_finite || !mem.is_finite() {
            return Err(Error::NonFiniteOutput);
        }
        if let Some(dst) = self.buf.get_mut(POSTFILTER_HIST..BUF_LEN) {
            dst.copy_from_slice(frame_in);
        }
        pcm_out.copy_from_slice(&staged_pcm);
        self.prev_gain = 0.0;
        self.deemph_mem = mem;
        self.buf.copy_within(N_LONG..BUF_LEN, 0);
        Ok(())
    }
}

/// Narrows `f64` to IEEE-754 `f32` for final PCM sample output.
#[inline]
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "final PCM output narrows f64 accumulator to f32 sample"
)]
const fn narrow_f32(v: f64) -> f32 {
    v as f32
}

/// Evaluates symmetric 5-tap comb `g0*y[n-T] + g1*(y[n-T-1]+y[n-T+1]) + g2*(y[n-T-2]+y[n-T+2])`.
#[inline]
fn comb_tap(buf: &[f64; BUF_LEN], idx: usize, period: usize, g: &[f64; 3]) -> f64 {
    let center = idx.wrapping_sub(period);
    let y0 = buf.get(center).copied().unwrap_or(0.0);
    let ym1 = buf.get(center.wrapping_sub(1)).copied().unwrap_or(0.0);
    let yp1 = buf.get(center.wrapping_add(1)).copied().unwrap_or(0.0);
    let mut acc = g[0] * y0 + g[1] * (ym1 + yp1);
    if g[2] != 0.0 {
        let ym2 = buf.get(center.wrapping_sub(2)).copied().unwrap_or(0.0);
        let yp2 = buf.get(center.wrapping_add(2)).copied().unwrap_or(0.0);
        acc += g[2] * (ym2 + yp2);
    }
    acc
}
