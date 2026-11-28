//! RFC 6716 Section 4.1 bit-exact Opus range decoder and Section 4.3.2.1 Laplace decoder.

/// Minimum range threshold after renormalization (`2^23`, RFC 6716 Section 4.1.2.1).
const EC_CODE_BOT: u32 = 1 << 23;
/// Bitmask keeping the lower 31 bits of `val` during renormalization (RFC 6716 Section 4.1.2.1).
const EC_CODE_MASK: u32 = 0x7FFF_FFFF;
/// Laplace probability precision (`1 << 15 = 32768`, RFC 6716 Section 4.3.2.1).
const LAPLACE_FTB: u32 = 15;
const LAPLACE_FT: u32 = 1 << LAPLACE_FTB;
/// Minimum tail probability floor per Laplace symbol and tail count (`NMIN = 16`).
const LAPLACE_MINP: u32 = 1;
const LAPLACE_NMIN: u32 = 16;

/// Bit-exact range and backward raw-bit decoder over one CELT frame payload (RFC 6716 Section 4.1).
#[derive(Debug, Clone)]
pub(crate) struct RangeDecoder<'a> {
    buf: &'a [u8],
    offs: usize,
    end_offs: usize,
    end_window: u32,
    nend_bits: u32,
    rem: u32,
    val: u32,
    rng: u32,
    ext: u32,
    nbits_total: i32,
}

impl<'a> RangeDecoder<'a> {
    /// Initializes the range decoder over a CELT payload slice (e.g. `&[0x7f, 0x54, 0xce, ...]` after TOC `0xb8`) per RFC 6716 Section 4.1.1.
    #[must_use]
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        let (rem, offs) = match buf.first() {
            Some(&first) => (u32::from(first), 1),
            None => (0, 0),
        };
        let mut dec = Self {
            buf,
            offs,
            end_offs: 0,
            end_window: 0,
            nend_bits: 0,
            rem,
            val: 127_u32.wrapping_sub(rem >> 1),
            rng: 128,
            ext: 0,
            nbits_total: 9,
        };
        dec.normalize();
        dec
    }

    /// Returns the current range register (used to seed the CELT LCG).
    #[inline]
    #[must_use]
    pub(crate) const fn rng(&self) -> u32 {
        self.rng
    }

    /// Returns the total frame payload size in whole bits.
    #[inline]
    #[must_use]
    pub(crate) fn storage_bits(&self) -> i32 {
        i32::try_from(self.buf.len().saturating_mul(8)).unwrap_or(i32::MAX)
    }

    /// Renormalizes `rng` until `rng > 2^23` per RFC 6716 Section 4.1.2.1.
    #[inline]
    fn normalize(&mut self) {
        while self.rng <= EC_CODE_BOT {
            self.rng <<= 8;
            let mut sym = (self.rem & 1) << 7;
            self.rem = match self.buf.get(self.offs) {
                Some(&byte) => {
                    self.offs = self.offs.wrapping_add(1);
                    u32::from(byte)
                }
                None => 0,
            };
            sym |= self.rem >> 1;
            self.val = ((self.val << 8).wrapping_add(255_u32.wrapping_sub(sym))) & EC_CODE_MASK;
            self.nbits_total = self.nbits_total.saturating_add(8);
        }
    }

    /// Computes cumulative frequency `fs` for total frequency `ft` (RFC 6716 Section 4.1.2).
    #[inline]
    pub(crate) fn decode(&mut self, ft: u32) -> u32 {
        let Some(ext) = self.rng.checked_div(ft) else {
            return 0;
        };
        self.ext = ext;
        let Some(s) = self.val.checked_div(ext) else {
            return 0;
        };
        ft.saturating_sub(s.saturating_add(1).min(ft))
    }

    /// Computes cumulative frequency `fs` for power-of-two `ft = 1 << ftb` (RFC 6716 Section 4.1.3.1).
    #[inline]
    pub(crate) fn decode_bin(&mut self, ftb: u32) -> u32 {
        let ext = self.rng >> ftb;
        self.ext = ext;
        let Some(s) = self.val.checked_div(ext) else {
            return 0;
        };
        let ft = 1_u32 << ftb;
        ft.saturating_sub(s.saturating_add(1).min(ft))
    }

    /// Updates `(val, rng)` for symbol range `[fl, fh)` out of `ft` and normalizes (RFC 6716 Section 4.1.2).
    #[inline]
    pub(crate) fn update(&mut self, fl: u32, fh: u32, ft: u32) {
        let tmp = self.ext.wrapping_mul(ft.wrapping_sub(fh));
        self.val = self.val.wrapping_sub(tmp);
        if fl > 0 {
            self.rng = self.ext.wrapping_mul(fh.wrapping_sub(fl));
        } else {
            self.rng = self.rng.wrapping_sub(tmp);
        }
        self.normalize();
    }

    /// Decodes one binary symbol with `P(1) = 2^(-logp)` (RFC 6716 Section 4.1.3.2).
    #[inline]
    pub(crate) fn dec_bit_logp(&mut self, logp: u32) -> u32 {
        let r = self.rng;
        let d = self.val;
        let s = r >> logp;
        let ret = u32::from(d < s);
        if ret == 0 {
            self.val = d.wrapping_sub(s);
            self.rng = r.wrapping_sub(s);
        } else {
            self.rng = s;
        }
        self.normalize();
        ret
    }

    /// Decodes a table-driven symbol using inverse CDF `icdf` with `ft = 1 << ftb` (RFC 6716 Section 4.1.3.3).
    #[inline]
    pub(crate) fn dec_icdf(&mut self, icdf: &[u8], ftb: u32) -> usize {
        let s = self.rng >> ftb;
        let d = self.val;
        let mut prev = self.rng;
        for (idx, &entry) in icdf.iter().enumerate() {
            let cur = s.wrapping_mul(u32::from(entry));
            if d >= cur {
                self.val = d.wrapping_sub(cur);
                self.rng = prev.wrapping_sub(cur);
                self.normalize();
                return idx;
            }
            prev = cur;
        }
        0
    }

    /// Reads `bits` (`0..=25`) raw bits packed backwards from the end of the payload (RFC 6716 Section 4.1.4).
    #[inline]
    pub(crate) fn dec_bits(&mut self, bits: u32) -> u32 {
        if bits == 0 {
            return 0;
        }
        let bits = bits.min(25);
        while self.nend_bits < bits {
            let byte = if self.end_offs < self.buf.len() {
                self.end_offs = self.end_offs.wrapping_add(1);
                let idx = self.buf.len().wrapping_sub(self.end_offs);
                self.buf.get(idx).copied().unwrap_or(0)
            } else {
                0
            };
            self.end_window |= u32::from(byte) << self.nend_bits;
            self.nend_bits = self.nend_bits.wrapping_add(8);
        }
        let mask = (1_u32 << bits).wrapping_sub(1);
        let ret = self.end_window & mask;
        self.end_window >>= bits;
        self.nend_bits = self.nend_bits.wrapping_sub(bits);
        self.nbits_total = self
            .nbits_total
            .saturating_add(i32::try_from(bits).unwrap_or(0));
        ret
    }

    /// Decodes a uniform integer in `0..ft` (`1 <= ft <= u32::MAX`, RFC 6716 Section 4.1.5).
    #[inline]
    pub(crate) fn dec_uint(&mut self, ft: u32) -> u32 {
        if ft <= 1 {
            return 0;
        }
        let ft_m1 = ft.wrapping_sub(1);
        let ftb = ilog(ft_m1);
        if ftb > 8 {
            let shift = ftb.wrapping_sub(8);
            let top_ft = (ft_m1 >> shift).wrapping_add(1);
            let top = self.decode(top_ft);
            self.update(top, top.wrapping_add(1), top_ft);
            let low = self.dec_bits(shift);
            let t = (top << shift) | low;
            // RFC 6716 Section 4.1.5 saturates out-of-range codewords to `ft - 1` (matching `libopus` and `ffopus`).
            t.min(ft_m1)
        } else {
            let t = self.decode(ft);
            self.update(t, t.wrapping_add(1), ft);
            t
        }
    }

    /// Conservative upper bound on whole bits consumed so far (RFC 6716 Section 4.1.6.1).
    #[inline]
    #[must_use]
    pub(crate) fn tell(&self) -> i32 {
        let lg = i32::try_from(ilog(self.rng)).unwrap_or(0);
        self.nbits_total.saturating_sub(lg)
    }

    /// Conservative upper bound on 1/8th bits consumed so far (RFC 6716 Section 4.1.6.2).
    #[inline]
    #[must_use]
    pub(crate) fn tell_frac(&self) -> i32 {
        let lg_u = ilog(self.rng);
        let mut r = self.rng >> lg_u.saturating_sub(16);
        let mut lg = i32::try_from(lg_u).unwrap_or(0);
        for _ in 0..3 {
            r = (r.wrapping_mul(r)) >> 15;
            let bit = r >> 16;
            lg = lg
                .wrapping_mul(2)
                .wrapping_add(i32::try_from(bit).unwrap_or(0));
            r >>= bit;
        }
        self.nbits_total.saturating_mul(8).saturating_sub(lg)
    }

    /// Decodes a Laplace-distributed integer for coarse energy (RFC 6716 Section 4.3.2.1).
    #[inline]
    pub(crate) fn dec_laplace(&mut self, fs0: u32, decay: u32) -> i32 {
        let fm = self.decode_bin(LAPLACE_FTB);
        let mut fl = 0_u32;
        let mut fs = fs0;
        let mut val = 0_i32;
        if fm >= fs {
            val = 1;
            fl = fs;
            let rem = LAPLACE_FT
                .saturating_sub(2_u32.wrapping_mul(LAPLACE_NMIN).wrapping_mul(LAPLACE_MINP))
                .saturating_sub(fs0);
            let scale = 16384_u32.saturating_sub(decay);
            fs = ((rem.wrapping_mul(scale)) >> 15).wrapping_add(LAPLACE_MINP);
            while fs > LAPLACE_MINP && fm >= fl.saturating_add(fs.wrapping_mul(2)) {
                let step = fs.wrapping_mul(2);
                fl = fl.wrapping_add(step);
                fs = ((step
                    .saturating_sub(2_u32.wrapping_mul(LAPLACE_MINP))
                    .wrapping_mul(decay))
                    >> 15)
                    .wrapping_add(LAPLACE_MINP);
                val = val.saturating_add(1);
            }
            if fs <= LAPLACE_MINP {
                let di = fm.saturating_sub(fl) >> 1;
                val = val.saturating_add(i32::try_from(di).unwrap_or(0));
                fl = fl.saturating_add(di.wrapping_mul(2));
            }
            if fm < fl.saturating_add(fs) {
                val = val.wrapping_neg();
            } else {
                fl = fl.saturating_add(fs);
            }
        }
        let fh = fl.saturating_add(fs).min(LAPLACE_FT);
        self.update(fl, fh, LAPLACE_FT);
        val
    }
}

/// Number of bits needed to represent `v` (`0` for `0`, RFC 6716 Section 4.1.5).
#[inline]
#[must_use]
pub(crate) const fn ilog(v: u32) -> u32 {
    32_u32.wrapping_sub(v.leading_zeros())
}

/// Conservative base-2 log of `val` in `1/(1<<frac)` bit units (RFC 6716 Section 4.1.6.2).
#[must_use]
pub(crate) fn log2_frac(val: u32, frac: u32) -> i32 {
    if val == 0 {
        return 0;
    }
    let l = ilog(val);
    let mut r = if l > 16 {
        val >> l.wrapping_sub(16)
    } else {
        val << 16_u32.wrapping_sub(l)
    };
    let mut lg = i32::try_from(l.wrapping_sub(1)).unwrap_or(0);
    for _ in 0..frac {
        r = (r.wrapping_mul(r)) >> 15;
        let bit = r >> 16;
        lg = lg
            .wrapping_mul(2)
            .wrapping_add(i32::try_from(bit).unwrap_or(0));
        r >>= bit;
    }
    lg
}
