//! Normative RFC 6716 Section 4.3 tables and precomputed math caches for CELT WB 20 ms mono.

use std::f64::consts::PI;
use std::sync::LazyLock;

use crate::range::log2_frac;

/// Number of MDCT bands in standard 48 kHz CELT mode (RFC 6716 Table 55).
pub(crate) const MODE_BANDS: usize = 21;
/// Number of coded bands in Wideband mode: 0..8000 Hz (RFC 6716 Table 55).
pub(crate) const WB_BANDS: usize = 17;
/// Frame size log-multiplier for 20 ms (`log2(960 / 120) = 3`, RFC 6716 Section 4.3.3).
pub(crate) const LM: usize = 3;
/// Short-block count for 20 ms (`2^LM = 8`, RFC 6716 Table 55).
pub(crate) const M_BLOCKS: usize = 1 << LM;
/// Bit allocation fractional resolution: 1/8th bits (`BITRES = 3`, RFC 6716 Section 4.3.3).
pub(crate) const BITRES: i32 = 3;
/// Fine-energy allocation offset in 1/8th bits (RFC 6716 Section 4.3.3).
pub(crate) const FINE_OFFSET: i32 = 21;
/// Maximum explicit fine-energy bits per band (RFC 6716 Section 4.3.2.2).
pub(crate) const MAX_FINE_BITS: i32 = 8;
/// Maximum pseudo-pulse index (`K <= 128`, RFC 6716 Section 4.3.4.1).
pub(crate) const MAX_PSEUDO: usize = 40;
/// Fixed-point `2^(i/8)` table in Q14 for exact split-angle `qn` computation (RFC 6716 Section 4.3.4.4).
pub(crate) const EXP2_TABLE8: [u32; 8] = [16384, 17866, 19483, 21247, 23170, 25267, 27554, 30048];
/// Overlap length in 48 kHz samples (`120`, RFC 6716 Section 4.3.7).
pub(crate) const OVERLAP_48K: usize = 120;
/// De-emphasis filter coefficient `27853 / 32768 = 0.850006103515625` (RFC 6716 Section 4.3.7.2).
pub(crate) const DEEMPHASIS_ALPHA: f64 = 27853.0 / 32768.0;
/// Output PCM scale factor converting CELT Q15 energy units to normalized `[-1.0, 1.0]` floats.
pub(crate) const CELT_PCM_SCALE: f64 = 1.0 / 32768.0;

/// Band edges in 2.5 ms bins (RFC 6716 Table 55); multiply by `M_BLOCKS = 8` for 20 ms bins.
pub(crate) const EBANDS: [usize; MODE_BANDS + 1] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 34, 40, 48, 60, 78, 100,
];

/// Base-2 log of 2.5 ms band widths in 1/8th-bit units (RFC 6716 Section 4.3.3).
pub(crate) const LOG_N: [i32; MODE_BANDS] = [
    0, 0, 0, 0, 0, 0, 0, 0, 8, 8, 8, 8, 16, 16, 16, 21, 21, 24, 29, 34, 36,
];

/// Mean log2 energy per band in 6 dB units (RFC 6716 Section 4.3.2).
pub(crate) const E_MEANS: [f64; WB_BANDS] = [
    103.0 / 16.0,
    100.0 / 16.0,
    92.0 / 16.0,
    85.0 / 16.0,
    81.0 / 16.0,
    77.0 / 16.0,
    72.0 / 16.0,
    70.0 / 16.0,
    78.0 / 16.0,
    75.0 / 16.0,
    73.0 / 16.0,
    71.0 / 16.0,
    78.0 / 16.0,
    74.0 / 16.0,
    69.0 / 16.0,
    72.0 / 16.0,
    70.0 / 16.0,
];

/// Opus 20 ms WB inter-frame Laplace `(p0, decay)` wire model (`e_prob_model` named in RFC 6716 Section 4.3.2.1; literal pairs verified via black-box `libopus`/`ffopus` parity).
pub(crate) const E_PROB_MODEL_INTER: [(u8, u8); WB_BANDS] = [
    (42, 121),
    (96, 66),
    (108, 43),
    (111, 40),
    (117, 44),
    (123, 32),
    (120, 36),
    (119, 33),
    (127, 33),
    (134, 34),
    (139, 21),
    (147, 23),
    (152, 20),
    (158, 25),
    (154, 26),
    (166, 21),
    (173, 16),
];

/// Opus 20 ms WB intra-frame Laplace `(p0, decay)` wire model (`e_prob_model` named in RFC 6716 Section 4.3.2.1; literal pairs verified via black-box `libopus`/`ffopus` parity).
pub(crate) const E_PROB_MODEL_INTRA: [(u8, u8); WB_BANDS] = [
    (22, 178),
    (63, 114),
    (74, 82),
    (84, 83),
    (92, 82),
    (103, 62),
    (96, 72),
    (96, 67),
    (101, 73),
    (107, 72),
    (113, 55),
    (118, 52),
    (125, 52),
    (118, 52),
    (117, 55),
    (135, 49),
    (137, 39),
];

/// Static bit allocation table `[band][quality]` in 1/32 bit per MDCT bin (RFC 6716 Table 57).
pub(crate) const ALLOC_VECTORS: [[u8; 11]; WB_BANDS] = [
    [0, 90, 110, 118, 126, 134, 144, 152, 162, 172, 200],
    [0, 80, 100, 110, 119, 127, 137, 145, 155, 165, 200],
    [0, 75, 90, 103, 112, 120, 130, 138, 148, 158, 200],
    [0, 69, 84, 93, 104, 114, 124, 132, 142, 152, 200],
    [0, 63, 78, 86, 95, 103, 113, 123, 133, 143, 200],
    [0, 56, 71, 80, 89, 97, 107, 117, 127, 137, 200],
    [0, 49, 65, 75, 83, 91, 101, 111, 121, 131, 200],
    [0, 40, 58, 70, 78, 85, 95, 105, 115, 125, 200],
    [0, 34, 51, 65, 72, 78, 88, 98, 108, 118, 198],
    [0, 29, 45, 59, 66, 72, 82, 92, 102, 112, 193],
    [0, 20, 39, 53, 60, 66, 76, 86, 96, 106, 188],
    [0, 18, 32, 47, 54, 60, 70, 80, 90, 100, 183],
    [0, 10, 26, 40, 47, 54, 64, 74, 84, 94, 178],
    [0, 0, 20, 31, 39, 47, 57, 67, 77, 87, 173],
    [0, 0, 12, 23, 32, 41, 51, 61, 71, 81, 168],
    [0, 0, 0, 15, 25, 35, 45, 55, 65, 75, 163],
    [0, 0, 0, 4, 17, 29, 39, 49, 59, 69, 158],
];

/// Inverse CDF for allocation trim with `ftb = 7` (RFC 6716 Table 58).
pub(crate) const TRIM_ICDF: [u8; 11] = [126, 124, 119, 109, 87, 41, 19, 9, 4, 2, 0];
/// Inverse CDF for spreading parameter with `ftb = 5` (RFC 6716 Table 56).
pub(crate) const SPREAD_ICDF: [u8; 4] = [25, 23, 2, 0];
/// Inverse CDF for post-filter tapset with `ftb = 2` (RFC 6716 Table 56).
pub(crate) const TAPSET_ICDF: [u8; 3] = [2, 1, 0];
/// Inverse CDF for tight-budget coarse energy with `ftb = 2` (RFC 6716 Section 4.3.2.1).
pub(crate) const SMALL_ENERGY_ICDF: [u8; 3] = [2, 1, 0];

/// Time-frequency resolution adjustments for 20 ms (`[2*transient + tf_select][tf_change]`, RFC 6716 Tables 60-63).
pub(crate) const TF_SELECT_LM3: [[i8; 2]; 4] = [[0, -2], [0, -3], [3, 0], [1, -1]];

/// Spreading denominator `f_r` for spread values 1, 2, 3 (RFC 6716 Table 59).
pub(crate) const SPREAD_FACTOR: [f64; 4] = [0.0, 15.0, 10.0, 5.0];

/// Post-filter comb tap gains `(g0, g1, g2)` for tapsets 0, 1, 2 (RFC 6716 Section 4.3.7.1).
pub(crate) const POSTFILTER_TAPS: [[f64; 3]; 3] = [
    [10048.0 / 32768.0, 7112.0 / 32768.0, 4248.0 / 32768.0],
    [15200.0 / 32768.0, 8784.0 / 32768.0, 0.0],
    [26208.0 / 32768.0, 3280.0 / 32768.0, 0.0],
];

/// Maps pseudo-pulse index `0..=40` to integer pulse count `K` (RFC 6716 Section 4.3.4.1).
#[inline]
#[must_use]
pub(crate) const fn pseudo_to_pulses(idx: usize) -> usize {
    if idx < 8 {
        idx
    } else {
        (8_usize.wrapping_add(idx & 7)) << ((idx >> 3).wrapping_sub(1))
    }
}

/// Computes `V(n, k)` if it fits in `u32`, else `None` (RFC 6716 Section 4.3.4.2).
#[must_use]
pub(crate) fn ncwrs_checked(n: usize, k: usize) -> Option<u32> {
    if k == 0 {
        return Some(1);
    }
    if n == 0 {
        return Some(0);
    }
    if n == 1 {
        return Some(2);
    }
    let mut row = [0_u64; 129];
    if k >= row.len() {
        return None;
    }
    for (j, slot) in row.iter_mut().take(k.wrapping_add(1)).enumerate() {
        *slot = if j == 0 { 1 } else { 2 };
    }
    for _ in 1..n {
        let mut prev_diag = 1_u64;
        for j in 1..=k {
            let cur_slot = row.get(j).copied()?;
            let prev_left = row.get(j.wrapping_sub(1)).copied().unwrap_or(0);
            let next = cur_slot.checked_add(prev_left)?.checked_add(prev_diag)?;
            if next > u64::from(u32::MAX) {
                return None;
            }
            prev_diag = cur_slot;
            if let Some(dst) = row.get_mut(j) {
                *dst = next;
            }
        }
    }
    let v = row.get(k).copied()?;
    u32::try_from(v).ok()
}

/// Precomputed pulse-bit table for one `(N, split_lm)` pair (RFC 6716 Section 4.3.3 & 4.3.4.1).
#[derive(Debug, Clone)]
pub(crate) struct PulseRow {
    pub(crate) max_idx: usize,
    pub(crate) bits: [i32; MAX_PSEUDO + 1],
}

/// Precomputed CELT tables initialized once on first decoder construction.
#[derive(Debug)]
pub(crate) struct CeltTables {
    /// Low-overlap MDCT window for 48 kHz (`L = 120`, RFC 6716 Section 4.3.7).
    pub(crate) window_48k: [f64; OVERLAP_48K],
    /// Squared MDCT window for smooth post-filter gain transitions (RFC 6716 Section 4.3.7.1).
    pub(crate) window_48k_sq: [f64; OVERLAP_48K],
    /// Pulse-bit rows indexed by `[base_idx][lm_slot]` where `base_idx` is `0:1, 1:2, 2:4, 3:6` and `lm_slot` is `lm + 1` (`0..=4`).
    pub(crate) pulse_rows: [[PulseRow; 5]; 4],
    /// Per-band maximum bit allocation `cap[band]` for WB 20 ms mono in 1/8th bits (RFC 6716 Section 4.3.3).
    pub(crate) cap_lm3_mono: [i32; WB_BANDS],
}

static CELT_TABLES: LazyLock<CeltTables> = LazyLock::new(build_tables);

/// Warms up CELT lookup tables during decoder construction so decode never allocates.
pub(crate) fn warmup() {
    let _ = &*CELT_TABLES;
}

/// Returns a reference to the process-global precomputed CELT tables.
#[inline]
#[must_use]
pub(crate) fn tables() -> &'static CeltTables {
    &CELT_TABLES
}

/// Maps 2.5 ms base band width `{1, 2, 4, 6}` to `pulse_rows` index `0..4`.
#[inline]
#[must_use]
pub(crate) const fn base_width_slot(base_n: usize) -> usize {
    match base_n {
        1 => 0,
        2 => 1,
        4 => 2,
        _ => 3,
    }
}

/// Computes the direct PVQ pulse cache row for dimension `n` (RFC 6716 Section 4.3.4.1).
fn compute_pulse_row(n: usize) -> PulseRow {
    let mut bits = [0_i32; MAX_PSEUDO + 1];
    let mut max_idx = 0_usize;
    if n <= 1 {
        return PulseRow { max_idx: 0, bits };
    }
    for idx in 1..=MAX_PSEUDO {
        let k = pseudo_to_pulses(idx);
        let Some(v) = ncwrs_checked(n, k) else {
            break;
        };
        let floor_b = log2_frac(v, 3);
        let b = if v.is_power_of_two() {
            floor_b
        } else {
            floor_b.saturating_add(1)
        };
        if b > 256 {
            break;
        }
        if let Some(dst) = bits.get_mut(idx) {
            *dst = b;
        }
        max_idx = idx;
    }
    PulseRow { max_idx, bits }
}

/// Computes `cap[band]` for WB 20 ms mono matching RFC 6716 Section 4.3.3.
fn compute_cap_lm3_mono(pulse_rows: &[[PulseRow; 5]; 4]) -> [i32; WB_BANDS] {
    let mut max_rec = [[0_i32; 5]; 4];
    let base_widths: [usize; 4] = [1, 2, 4, 6];
    for slot in 0..4_usize {
        let base_n = base_widths.get(slot).copied().unwrap_or(1);
        for lm_slot in 0..5_usize {
            let dim = if lm_slot == 0 {
                base_n >> 1
            } else {
                base_n << lm_slot.wrapping_sub(1)
            };
            let direct = pulse_rows
                .get(slot)
                .and_then(|r| r.get(lm_slot))
                .and_then(|row| row.bits.get(row.max_idx).copied())
                .unwrap_or(0);
            let split = if lm_slot > 0 && dim > 2 && (dim & 1) == 0 {
                let half = max_rec
                    .get(slot)
                    .and_then(|r| r.get(lm_slot.wrapping_sub(1)))
                    .copied()
                    .unwrap_or(0);
                // Recursive PVQ split adds `itheta` overhead (54 eighth-bits per split level, giving `cap = 514` for `N = 8`).
                half.saturating_mul(2).saturating_add(54)
            } else {
                0
            };
            if let Some(dst) = max_rec.get_mut(slot).and_then(|r| r.get_mut(lm_slot)) {
                *dst = direct.max(split);
            }
        }
    }
    let mut caps = [0_i32; WB_BANDS];
    for band in 0..WB_BANDS {
        let n0 = EBANDS
            .get(band.wrapping_add(1))
            .copied()
            .unwrap_or(0)
            .wrapping_sub(EBANDS.get(band).copied().unwrap_or(0));
        let n = n0.wrapping_mul(M_BLOCKS);
        let slot = base_width_slot(n0);
        let max_bits = max_rec
            .get(slot)
            .and_then(|r| r.get(4))
            .copied()
            .unwrap_or(0);
        // RFC 6716 Section 4.3.3: band cap covers max recursive PVQ bits plus `MAX_FINE_BITS = 8` fine bits.
        let total = max_bits.saturating_add(MAX_FINE_BITS << BITRES);
        let n_i32 = i32::try_from(n).unwrap_or(1);
        let cap_byte = ((total.wrapping_mul(4)).checked_div(n_i32).unwrap_or(0))
            .saturating_sub(64)
            .clamp(0, 255);
        let cap = (cap_byte.wrapping_add(64)).wrapping_mul(n_i32) >> 2;
        if let Some(dst) = caps.get_mut(band) {
            *dst = cap;
        }
    }
    caps
}

fn build_tables() -> CeltTables {
    let mut window_48k = [0.0_f64; OVERLAP_48K];
    let mut window_48k_sq = [0.0_f64; OVERLAP_48K];
    let l_f = f64::from(u16::try_from(OVERLAP_48K).unwrap_or(120));
    for n in 0..OVERLAP_48K {
        let nf = f64::from(u16::try_from(n).unwrap_or(0)) + 0.5;
        let inner = (0.5 * PI * nf / l_f).sin();
        let w = (0.5 * PI * inner * inner).sin();
        if let (Some(dw), Some(dsq)) = (window_48k.get_mut(n), window_48k_sq.get_mut(n)) {
            *dw = w;
            *dsq = w * w;
        }
    }
    let empty_row = PulseRow {
        max_idx: 0,
        bits: [0; MAX_PSEUDO + 1],
    };
    let mut pulse_rows = [
        [
            empty_row.clone(),
            empty_row.clone(),
            empty_row.clone(),
            empty_row.clone(),
            empty_row.clone(),
        ],
        [
            empty_row.clone(),
            empty_row.clone(),
            empty_row.clone(),
            empty_row.clone(),
            empty_row.clone(),
        ],
        [
            empty_row.clone(),
            empty_row.clone(),
            empty_row.clone(),
            empty_row.clone(),
            empty_row.clone(),
        ],
        [
            empty_row.clone(),
            empty_row.clone(),
            empty_row.clone(),
            empty_row.clone(),
            empty_row,
        ],
    ];
    let base_widths = [1_usize, 2, 4, 6];
    for (slot, &base_n) in base_widths.iter().enumerate() {
        for lm_slot in 0..5_usize {
            let lm = i32::try_from(lm_slot).unwrap_or(0).wrapping_sub(1);
            let n = if lm < 0 {
                // `lm == -1` corresponds to half of `lm == 0` when `base_n` is even.
                if base_n >= 2 { base_n >> 1 } else { 1 }
            } else {
                base_n << usize::try_from(lm).unwrap_or(0)
            };
            let row = compute_pulse_row(n);
            if let Some(dst) = pulse_rows.get_mut(slot).and_then(|r| r.get_mut(lm_slot)) {
                *dst = row;
            }
        }
    }
    let cap_lm3_mono = compute_cap_lm3_mono(&pulse_rows);
    CeltTables {
        window_48k,
        window_48k_sq,
        pulse_rows,
        cap_lm3_mono,
    }
}
