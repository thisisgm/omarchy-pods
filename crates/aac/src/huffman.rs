//! Fast 2-level flat Huffman lookup tables for ISO/IEC 14496-3 Annex 4.A codebooks.

use std::sync::LazyLock;

use crate::Error;
use crate::bits::BitReader;
use crate::tables::huffman::{CB1, CB2, CB3, CB4, CB5, CB6, CB7, CB8, CB9, CB10, CB11, HCOD_SF};

/// First-level lookup width in bits (512 entries per table; resolves >97% of codewords in 1 lookup).
const ROOT_BITS: u32 = 9;
const ROOT_SIZE: usize = 1 << ROOT_BITS;
/// Flag bit in a packed LUT entry indicating a secondary table redirect rather than a leaf symbol.
const SUBTABLE_FLAG: u32 = 1 << 31;

/// Two-level flat prefix-code table.
///
/// Leaf entry encoding: `(symbol << 8) | bit_length`.
/// Redirect entry encoding: `SUBTABLE_FLAG | (subtable_offset << 8) | subtable_bits`.
pub(crate) struct HuffTable {
    root: [u32; ROOT_SIZE],
    sub: Vec<u32>,
}

impl HuffTable {
    fn from_u16(entries: &[(u8, u16)]) -> Self {
        let widened: Vec<(u8, u32)> = entries
            .iter()
            .map(|&(len, code)| (len, u32::from(code)))
            .collect();
        Self::from_u32(&widened)
    }

    fn from_u32(entries: &[(u8, u32)]) -> Self {
        let mut root = [0_u32; ROOT_SIZE];
        let mut sub_bits = [0_u8; ROOT_SIZE];

        // Pass 1: fill direct root leaves (len <= ROOT_BITS) and measure max subtable width per bucket.
        for (sym, &(len, code)) in entries.iter().enumerate() {
            let len_u32 = u32::from(len);
            let sym_u32 = u32::try_from(sym).unwrap_or(0);
            if len_u32 <= ROOT_BITS {
                let shift = ROOT_BITS.wrapping_sub(len_u32);
                let start = usize::try_from(code << shift).unwrap_or(0);
                let span = 1_usize << shift;
                let entry = (sym_u32 << 8) | len_u32;
                if let Some(slice) = root.get_mut(start..start.wrapping_add(span)) {
                    slice.fill(entry);
                }
            } else {
                let extra = len.wrapping_sub(u8::try_from(ROOT_BITS).unwrap_or(9));
                let prefix = usize::try_from(code >> u32::from(extra)).unwrap_or(0);
                if let Some(slot) = sub_bits.get_mut(prefix) {
                    *slot = (*slot).max(extra);
                }
            }
        }

        // Pass 2: allocate contiguous secondary subtables for buckets with codewords > ROOT_BITS.
        let mut sub = Vec::new();
        for (prefix, &bits) in sub_bits.iter().enumerate() {
            if bits == 0 {
                continue;
            }
            let offset = u32::try_from(sub.len()).unwrap_or(0);
            let size = 1_usize << u32::from(bits);
            sub.resize(sub.len().wrapping_add(size), 0);
            if let Some(slot) = root.get_mut(prefix) {
                *slot = SUBTABLE_FLAG | (offset << 8) | u32::from(bits);
            }
        }

        // Pass 3: populate secondary subtables.
        for (sym, &(len, code)) in entries.iter().enumerate() {
            let len_u32 = u32::from(len);
            if len_u32 <= ROOT_BITS {
                continue;
            }
            let sym_u32 = u32::try_from(sym).unwrap_or(0);
            let extra = len_u32.wrapping_sub(ROOT_BITS);
            let prefix = usize::try_from(code >> extra).unwrap_or(0);
            let Some(&redirect) = root.get(prefix) else {
                continue;
            };
            let table_bits = redirect & 0xff;
            let offset = usize::try_from((redirect & !SUBTABLE_FLAG) >> 8).unwrap_or(0);
            let tail_mask = (1_u32 << extra).wrapping_sub(1);
            let tail = code & tail_mask;
            let shift = table_bits.wrapping_sub(extra);
            let start = offset.wrapping_add(usize::try_from(tail << shift).unwrap_or(0));
            let span = 1_usize << shift;
            let entry = (sym_u32 << 8) | extra;
            if let Some(slice) = sub.get_mut(start..start.wrapping_add(span)) {
                slice.fill(entry);
            }
        }

        Self { root, sub }
    }

    /// Decodes one symbol from `reader` in O(1) (1 table lookup for <= 9 bits, 2 for <= 19 bits).
    #[inline]
    pub(crate) fn decode(&self, reader: &mut BitReader<'_>) -> Result<u16, Error> {
        let peek = usize::try_from(reader.peek_padded(ROOT_BITS)).unwrap_or(0);
        let &entry = self.root.get(peek).ok_or(Error::InvalidBitstream)?;
        if (entry & SUBTABLE_FLAG) == 0 {
            let len = entry & 0xff;
            if len == 0 {
                return Err(Error::InvalidBitstream);
            }
            reader.consume(len)?;
            return u16::try_from(entry >> 8)
                .ok()
                .ok_or(Error::InvalidBitstream);
        }
        reader.consume(ROOT_BITS)?;
        let sub_bits = entry & 0xff;
        let offset = usize::try_from((entry & !SUBTABLE_FLAG) >> 8).unwrap_or(0);
        let sub_idx =
            offset.wrapping_add(usize::try_from(reader.peek_padded(sub_bits)).unwrap_or(0));
        let &sub_entry = self.sub.get(sub_idx).ok_or(Error::InvalidBitstream)?;
        let len = sub_entry & 0xff;
        if len == 0 {
            return Err(Error::InvalidBitstream);
        }
        reader.consume(len)?;
        u16::try_from(sub_entry >> 8)
            .ok()
            .ok_or(Error::InvalidBitstream)
    }
}

/// All 12 compiled Huffman lookup tables (`hcod_sf` and spectral codebooks 1..=11).
pub(crate) struct HuffTables {
    pub(crate) sf: HuffTable,
    pub(crate) spectral: [HuffTable; 11],
}

static HUFF_TABLES: LazyLock<HuffTables> = LazyLock::new(|| HuffTables {
    sf: HuffTable::from_u32(&HCOD_SF),
    spectral: [
        HuffTable::from_u16(&CB1),
        HuffTable::from_u16(&CB2),
        HuffTable::from_u16(&CB3),
        HuffTable::from_u16(&CB4),
        HuffTable::from_u16(&CB5),
        HuffTable::from_u16(&CB6),
        HuffTable::from_u16(&CB7),
        HuffTable::from_u16(&CB8),
        HuffTable::from_u16(&CB9),
        HuffTable::from_u16(&CB10),
        HuffTable::from_u16(&CB11),
    ],
});

#[inline]
#[must_use]
pub(crate) fn tables() -> &'static HuffTables {
    &HUFF_TABLES
}

/// Decodes a signed 4-tuple index from CB 1 or CB 2 into values in `-1..=1`.
#[inline]
#[must_use]
pub(crate) fn unpack_quad_signed(sym: u16) -> [i32; 4] {
    let mut rem = i32::from(sym);
    let q3 = rem.wrapping_rem(3).wrapping_sub(1);
    rem = rem.wrapping_div(3);
    let q2 = rem.wrapping_rem(3).wrapping_sub(1);
    rem = rem.wrapping_div(3);
    let q1 = rem.wrapping_rem(3).wrapping_sub(1);
    let q0 = rem.wrapping_div(3).wrapping_sub(1);
    [q0, q1, q2, q3]
}

/// Decodes an unsigned 4-tuple index from CB 3 or CB 4 into values in `0..=2`.
#[inline]
#[must_use]
pub(crate) fn unpack_quad_unsigned(sym: u16) -> [u32; 4] {
    let mut rem = u32::from(sym);
    let q3 = rem.wrapping_rem(3);
    rem = rem.wrapping_div(3);
    let q2 = rem.wrapping_rem(3);
    rem = rem.wrapping_div(3);
    let q1 = rem.wrapping_rem(3);
    let q0 = rem.wrapping_div(3);
    [q0, q1, q2, q3]
}

/// Decodes a signed 2-tuple index from CB 5 or CB 6 into values in `-4..=4`.
#[inline]
#[must_use]
pub(crate) fn unpack_pair_signed(sym: u16) -> [i32; 2] {
    let val = i32::from(sym);
    [
        val.wrapping_div(9).wrapping_sub(4),
        val.wrapping_rem(9).wrapping_sub(4),
    ]
}

/// Decodes an unsigned 2-tuple index from CB 7..=11 with radix `base` (`8`, `13`, or `17`).
#[inline]
#[must_use]
pub(crate) fn unpack_pair_unsigned(sym: u16, base: u32) -> [u32; 2] {
    let val = u32::from(sym);
    [
        val.checked_div(base).unwrap_or(0),
        val.checked_rem(base).unwrap_or(0),
    ]
}
