//! Temporal Noise Shaping (TNS) parsing and all-pole LPC spectral filtering (ISO/IEC 14496-3 Clause 4.6.9).

use std::f64::consts::FRAC_PI_2;

use crate::Error;
use crate::bits::BitReader;
use crate::tables::{BAND_EDGES, NUM_SWB, TNS_MAX_BANDS, TNS_MAX_ORDER};

/// Maximum number of TNS filters signalled by the 2-bit `n_filt` field (`0..=3`).
const MAX_FILTERS: usize = 3;

#[derive(Debug, Default, Clone, Copy)]
struct TnsFilter {
    length: usize,
    order: usize,
    direction: bool,
    coef_res_4bit: bool,
    coefs: [i8; TNS_MAX_ORDER],
}

/// Parsed TNS filter set for one ER AAC-ELD frame, stored inline without heap allocation.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct TnsData {
    n_filt: usize,
    filters: [TnsFilter; MAX_FILTERS],
}

impl TnsData {
    /// Parses `tns_data()` (Table 4.54, Clause 4.6.9); sample `01 0 100011 00010 0 0 001 111` is one 3-bit filter over 35 bands, order 2, upward, +1 and -1.
    pub(crate) fn parse(reader: &mut BitReader<'_>) -> Result<Self, Error> {
        let mut data = Self::default();
        let n_filt = usize::try_from(reader.read_bits(2)?)
            .ok()
            .ok_or(Error::InvalidBitstream)?;
        if n_filt == 0 {
            return Ok(data);
        }
        let coef_res_4bit = reader.read_bool()?;
        let res_bits: u32 = if coef_res_4bit { 4 } else { 3 };
        data.n_filt = n_filt;

        for filter in data.filters.iter_mut().take(n_filt) {
            let length = usize::try_from(reader.read_bits(6)?)
                .ok()
                .ok_or(Error::InvalidBitstream)?;
            let order = usize::try_from(reader.read_bits(5)?)
                .ok()
                .ok_or(Error::InvalidBitstream)?;
            if order > TNS_MAX_ORDER {
                return Err(Error::TnsOrderOutOfRange(order));
            }
            filter.length = length;
            filter.order = order;
            filter.coef_res_4bit = coef_res_4bit;
            if order == 0 {
                continue;
            }
            filter.direction = reader.read_bool()?;
            let coef_compress = u32::from(reader.read_bool()?);
            let coef_bits = res_bits.wrapping_sub(coef_compress);
            for slot in filter.coefs.iter_mut().take(order) {
                let raw = reader.read_signed_bits(coef_bits)?;
                *slot = i8::try_from(raw).ok().ok_or(Error::InvalidBitstream)?;
            }
        }
        Ok(data)
    }

    /// Applies the parsed TNS all-pole synthesis filters in place to `spec` (ISO/IEC 14496-3 Clause 4.6.9.3).
    pub(crate) fn apply(&self, max_sfb: usize, spec: &mut [f64; 480]) {
        if self.n_filt == 0 {
            return;
        }
        let limit = max_sfb.min(TNS_MAX_BANDS);
        let mut bottom = NUM_SWB;

        for filter in self.filters.iter().take(self.n_filt) {
            let top = bottom;
            bottom = top.saturating_sub(filter.length);
            if filter.order == 0 {
                continue;
            }
            let start_band = bottom.min(limit);
            let end_band = top.min(limit);
            if start_band >= end_band {
                continue;
            }
            let (Some(&start), Some(&end)) = (BAND_EDGES.get(start_band), BAND_EDGES.get(end_band))
            else {
                continue;
            };
            let (start, end) = (usize::from(start), usize::from(end));
            if start >= end {
                continue;
            }
            let lpc = build_lpc(filter);
            filter_range(spec, start, end, filter.direction, filter.order, &lpc);
        }
    }
}

/// Dequantizes TNS reflection coefficients and converts them to direct-form LPC via step-up.
fn build_lpc(filter: &TnsFilter) -> [f64; TNS_MAX_ORDER + 1] {
    let half_steps = if filter.coef_res_4bit { 8.0 } else { 4.0 };
    let iqfac_pos = FRAC_PI_2 / (half_steps - 0.5);
    let iqfac_neg = FRAC_PI_2 / (half_steps + 0.5);

    let mut lpc = [0.0_f64; TNS_MAX_ORDER + 1];
    let mut prev = [0.0_f64; TNS_MAX_ORDER + 1];
    lpc[0] = 1.0;

    for (m_idx, &coef) in filter.coefs.iter().take(filter.order).enumerate() {
        let m = m_idx.wrapping_add(1);
        let c = f64::from(coef);
        let rc = (c * if coef >= 0 { iqfac_pos } else { iqfac_neg }).sin();
        prev.copy_from_slice(&lpc);
        for i in 1..m {
            let mirror = m.wrapping_sub(i);
            if let (Some(&p_i), Some(&p_m), Some(dst)) =
                (prev.get(i), prev.get(mirror), lpc.get_mut(i))
            {
                *dst = p_i + rc * p_m;
            }
        }
        if let Some(dst) = lpc.get_mut(m) {
            *dst = rc;
        }
    }
    lpc
}

/// All-pole IIR filter `y[n] = x[n] - sum_{i=1..=order} lpc[i] * y[n - i]`.
fn filter_range(
    spec: &mut [f64; 480],
    start: usize,
    end: usize,
    direction_down: bool,
    order: usize,
    lpc: &[f64; TNS_MAX_ORDER + 1],
) {
    let mut state = [0.0_f64; TNS_MAX_ORDER];
    let count = end.wrapping_sub(start);
    for step in 0..count {
        let idx = if direction_down {
            end.wrapping_sub(1).wrapping_sub(step)
        } else {
            start.wrapping_add(step)
        };
        let Some(sample) = spec.get_mut(idx) else {
            break;
        };
        let mut y = *sample;
        for (a, &s) in lpc.iter().skip(1).take(order).zip(state.iter()) {
            y -= a * s;
        }
        *sample = y;
        // Shift delay line by 1 sample.
        let mut i = order.saturating_sub(1);
        while i > 0 {
            let prev = i.wrapping_sub(1);
            if let (Some(val), Some(dst)) = (state.get(prev).copied(), state.get_mut(i)) {
                *dst = val;
            }
            i = prev;
        }
        state[0] = y;
    }
}
