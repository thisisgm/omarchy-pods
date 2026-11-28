//! RFC 6716 Section 4.4 CELT Packet Loss Concealment (pitch-synchronous periodic extrapolation).

use crate::Error;
use crate::energy::ENERGY_FLOOR;
use crate::mdct::N_LONG;
use crate::postfilter::{POSTFILTER_HIST, PostfilterState};
use crate::tables::{OVERLAP_48K, WB_BANDS, tables};

/// Minimum and maximum pitch period searched in 48 kHz samples (`15..=720`, covering 66 Hz..3200 Hz).
const PITCH_MIN: usize = 24;
const PITCH_MAX: usize = 720;
/// Decimation factor for coarse pitch search (`4x` -> 12 kHz).
const DECIM: usize = 4;
/// Window length for correlation search (`240` samples = 5 ms at 48 kHz).
const CORR_LEN: usize = 240;

/// Persistent PLC state across consecutive lost frames.
#[derive(Debug, Clone)]
pub(crate) struct PlcState {
    loss_count: u32,
    pitch: usize,
    decay: f64,
}

impl PlcState {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            loss_count: 0,
            pitch: 120,
            decay: 1.0,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.loss_count = 0;
        self.pitch = 120;
        self.decay = 1.0;
    }

    /// Resets consecutive loss counter when a valid packet arrives.
    #[inline]
    pub(crate) fn on_valid_frame(&mut self) {
        self.loss_count = 0;
        self.decay = 1.0;
    }

    #[cfg(test)]
    pub(crate) fn state_for_test(&self) -> (u32, usize, u64) {
        (self.loss_count, self.pitch, self.decay.to_bits())
    }

    /// Generates one concealed 960-sample frame with TDAC-compatible overlap, committing state only when output `f32` PCM is finite.
    pub(crate) fn try_conceal(
        &mut self,
        postfilter: &mut PostfilterState,
        overlap: &mut [f64; OVERLAP_48K],
        old_e: &mut [f64; WB_BANDS],
        pcm_out: &mut [f32; N_LONG],
    ) -> Result<(), Error> {
        let hist = postfilter.history();
        let (next_pitch, next_decay) = if self.loss_count == 0 {
            (find_pitch(hist), 0.92)
        } else {
            (self.pitch, self.decay * 0.85)
        };
        let next_loss_count = self.loss_count.saturating_add(1);

        let pitch = next_pitch.clamp(PITCH_MIN, PITCH_MAX);
        let win = &tables().window_48k;
        let mut frame = [0.0_f64; N_LONG];
        let mut next_overlap = *overlap;
        let mut next_old_e = *old_e;

        // If loss run is long or history is silent, smoothly drain overlap to silence.
        let hist_energy: f64 = hist
            .get(POSTFILTER_HIST.saturating_sub(CORR_LEN)..)
            .unwrap_or(&[])
            .iter()
            .map(|&v| v * v)
            .sum();
        if next_loss_count > 20 || hist_energy < 1e-12 {
            let ov_energy: f64 = next_overlap.iter().map(|&v| v * v).sum();
            if ov_energy >= 1e-24 || !ov_energy.is_finite() {
                for i in 0..OVERLAP_48K {
                    let w = win
                        .get(OVERLAP_48K.wrapping_sub(1).wrapping_sub(i))
                        .copied()
                        .unwrap_or(0.0);
                    if let Some(d) = frame.get_mut(i) {
                        *d = next_overlap.get(i).copied().unwrap_or(0.0) * w;
                    }
                }
                next_overlap.fill(0.0);
            }
            if hist_energy >= 1e-12 || ov_energy >= 1e-24 {
                for e in &mut next_old_e {
                    *e = (*e - 1.0).max(ENERGY_FLOOR);
                }
            }
            postfilter.try_apply_concealed(&frame, pcm_out)?;
            self.pitch = next_pitch;
            self.decay = next_decay;
            self.loss_count = next_loss_count;
            *overlap = next_overlap;
            *old_e = next_old_e;
            return Ok(());
        }

        // Synthesize `N_LONG + OVERLAP_48K` periodic samples from `hist` at stride `pitch` with smooth decay.
        let mut synth = [0.0_f64; N_LONG + OVERLAP_48K];
        let mut g = 1.0_f64;
        let step_decay = next_decay.powf(1.0 / f64::from(u16::try_from(N_LONG).unwrap_or(960)));
        for i in 0..(N_LONG + OVERLAP_48K) {
            let src = if i < pitch {
                let h_idx = POSTFILTER_HIST.saturating_sub(pitch).wrapping_add(i);
                hist.get(h_idx).copied().unwrap_or(0.0)
            } else {
                synth.get(i.wrapping_sub(pitch)).copied().unwrap_or(0.0)
            };
            let val = src * g;
            if let Some(d) = synth.get_mut(i) {
                *d = val;
            }
            g *= step_decay;
        }

        // Cross-fade first `OVERLAP_48K` samples with saved TDAC `overlap` using `window^2` for C1 continuity.
        let win_sq = &tables().window_48k_sq;
        for i in 0..OVERLAP_48K {
            let w2 = win_sq.get(i).copied().unwrap_or(0.0);
            let ov = next_overlap.get(i).copied().unwrap_or(0.0);
            let s = synth.get(i).copied().unwrap_or(0.0);
            if let Some(d) = frame.get_mut(i) {
                *d = ov + w2 * s;
            }
        }
        if let (Some(dst), Some(src)) = (
            frame.get_mut(OVERLAP_48K..N_LONG),
            synth.get(OVERLAP_48K..N_LONG),
        ) {
            dst.copy_from_slice(src);
        }

        // Prepare windowed right-overlap tail so the next valid IMDCT frame joins with zero step.
        for i in 0..OVERLAP_48K {
            let w2_fall = win_sq
                .get(OVERLAP_48K.wrapping_sub(1).wrapping_sub(i))
                .copied()
                .unwrap_or(0.0);
            let s = synth.get(N_LONG.wrapping_add(i)).copied().unwrap_or(0.0);
            if let Some(d) = next_overlap.get_mut(i) {
                *d = w2_fall * s;
            }
        }

        // Gently decay stored log-energies so the next inter frame recovers rapidly.
        for e in &mut next_old_e {
            *e = (*e - 0.25).max(ENERGY_FLOOR);
        }

        postfilter.try_apply_concealed(&frame, pcm_out)?;
        self.pitch = next_pitch;
        self.decay = next_decay;
        self.loss_count = next_loss_count;
        *overlap = next_overlap;
        *old_e = next_old_e;
        Ok(())
    }
}

/// Fast two-stage pitch estimator on the pre-de-emphasis history buffer (`O(1500)` MACs).
fn find_pitch(hist: &[f64]) -> usize {
    const DS_LEN: usize = POSTFILTER_HIST / DECIM;
    let Some(h_full) = hist.get(..POSTFILTER_HIST) else {
        return 120;
    };
    // Downsample by 4 using boxcar sum into a 256-sample coarse buffer.
    let mut ds = [0.0_f64; DS_LEN];
    for (dst, chunk) in ds.iter_mut().zip(h_full.as_chunks::<DECIM>().0.iter()) {
        *dst = chunk[0] + chunk[1] + chunk[2] + chunk[3];
    }

    let corr_ds = CORR_LEN / DECIM;
    let tail_start = DS_LEN.saturating_sub(corr_ds);
    let min_lag = PITCH_MIN / DECIM;
    let max_lag = (PITCH_MAX / DECIM).min(tail_start);
    let mut best_lag = 30_usize;
    let mut best_score = -1.0_f64;
    let ds_tail = ds.get(tail_start..).unwrap_or(&[]);
    for lag in min_lag..=max_lag {
        let start = tail_start.wrapping_sub(lag);
        let Some(ref_slice) = ds.get(start..start.wrapping_add(corr_ds)) else {
            continue;
        };
        let mut dot = 0.0_f64;
        let mut en = 1e-12_f64;
        for (&a, &b) in ds_tail.iter().zip(ref_slice.iter()) {
            dot += a * b;
            en += b * b;
        }
        let score = if dot > 0.0 { (dot * dot) / en } else { -1.0 };
        if score > best_score {
            best_score = score;
            best_lag = lag;
        }
    }

    // Refine at full 48 kHz resolution within `+/-DECIM` around `best_lag * DECIM`.
    let center = best_lag.wrapping_mul(DECIM);
    let lo = center.saturating_sub(DECIM).max(PITCH_MIN);
    let hi = center.saturating_add(DECIM).min(PITCH_MAX);
    let full_tail = POSTFILTER_HIST.saturating_sub(CORR_LEN);
    let h_tail = h_full.get(full_tail..).unwrap_or(&[]);
    let mut best_full = center.clamp(PITCH_MIN, PITCH_MAX);
    let mut best_full_score = -1.0_f64;
    for lag in lo..=hi {
        let start = full_tail.wrapping_sub(lag);
        let Some(ref_slice) = h_full.get(start..start.wrapping_add(CORR_LEN)) else {
            continue;
        };
        let mut dot = 0.0_f64;
        let mut en = 1e-12_f64;
        for (&a, &b) in h_tail.iter().zip(ref_slice.iter()) {
            dot += a * b;
            en += b * b;
        }
        let score = if dot > 0.0 { (dot * dot) / en } else { -1.0 };
        if score > best_full_score {
            best_full_score = score;
            best_full = lag;
        }
    }
    best_full
}
