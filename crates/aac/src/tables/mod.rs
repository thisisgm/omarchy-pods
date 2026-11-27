//! Normative ISO/IEC 14496-3 tables and formula-derived lookup tables for ER AAC-ELD 480 @ 48 kHz.

#[rustfmt::skip]
pub(crate) mod huffman;
#[rustfmt::skip]
pub(crate) mod window;

use std::sync::LazyLock;

/// Number of scalefactor bands in a 480-sample ER AAC-ELD frame at 48 kHz (ISO/IEC 14496-3 Table 4.183).
pub(crate) const NUM_SWB: usize = 35;

/// Upper scalefactor band bound for TNS at 48 kHz LD/ELD (ISO/IEC 14496-3 Table 4.185).
pub(crate) const TNS_MAX_BANDS: usize = 31;

/// Maximum TNS filter order for ER AAC-LD/ELD (ISO/IEC 14496-3 Table 4.185).
pub(crate) const TNS_MAX_ORDER: usize = 12;

/// Scalefactor band edge offsets `swb_offset_480_48` (ISO/IEC 14496-3 Table 4.183).
pub(crate) const BAND_EDGES: [usize; NUM_SWB + 1] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64, 72, 80, 88, 96, 108, 120, 132,
    144, 156, 172, 188, 212, 240, 272, 304, 336, 368, 400, 432, 480,
];

/// Size of the precomputed `|q|^(4/3)` inverse-quantization lookup table.
pub(crate) const IQ_LUT_LEN: usize = 1025;

/// Formula-derived tables initialized once per process, before any hot-path decode.
pub(crate) struct MathTables {
    /// `2^(0.25 * (sf - 160))` for `sf in 0..=255` (ISO/IEC 14496-3 Clause 4.6.2.3.3, normalized by `1/32768 = 2^-15`).
    pub(crate) sf_gain: [f64; 256],
    /// `q^(4/3)` for `q in 0..1025` (ISO/IEC 14496-3 Clause 4.6.2.2).
    pub(crate) iq_pow43: [f64; IQ_LUT_LEN],
}

static MATH_TABLES: LazyLock<MathTables> = LazyLock::new(|| {
    let mut sf_gain = [0.0_f64; 256];
    for (sf, slot) in sf_gain.iter_mut().enumerate() {
        let exp = (f64::from(u16::try_from(sf).unwrap_or(0)) - 160.0) * 0.25;
        *slot = exp.exp2();
    }
    let mut iq_pow43 = [0.0_f64; IQ_LUT_LEN];
    for (q, slot) in iq_pow43.iter_mut().enumerate() {
        let qf = f64::from(u16::try_from(q).unwrap_or(0));
        *slot = qf.powf(4.0 / 3.0);
    }
    MathTables { sf_gain, iq_pow43 }
});

#[inline]
#[must_use]
pub(crate) fn math() -> &'static MathTables {
    &MATH_TABLES
}

/// Computes `|q|^(4/3)` with table lookup for `q < IQ_LUT_LEN` and exact formula fallback above.
#[inline]
#[must_use]
pub(crate) fn inv_quant_mag(q: u32) -> f64 {
    let tables = math();
    if let Ok(idx) = usize::try_from(q)
        && let Some(&val) = tables.iq_pow43.get(idx)
    {
        return val;
    }
    f64::from(q).powf(4.0 / 3.0)
}
