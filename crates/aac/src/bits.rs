//! Zero-allocation MSB-first bit reader with a 64-bit prefetch buffer.

use crate::Error;

/// Fast MSB-first bit reader over a byte slice.
///
/// Tracks exact remaining input bits separately from the 64-bit prefetch buffer so trailing
/// zero padding in the register never masks a truncated bitstream.
#[derive(Debug, Clone)]
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    buf: u64,
    bits_in_buf: u32,
    bits_left: usize,
}

impl<'a> BitReader<'a> {
    #[must_use]
    pub(crate) fn new(data: &'a [u8]) -> Self {
        let mut reader = Self {
            data,
            byte_pos: 0,
            buf: 0,
            bits_in_buf: 0,
            bits_left: data.len().saturating_mul(8),
        };
        reader.refill();
        reader
    }

    /// Exact number of unread bits remaining in the underlying slice.
    #[must_use]
    pub(crate) fn bits_left(&self) -> usize {
        self.bits_left
    }

    /// Refills the top of `self.buf` so at least 56 bits are available whenever input permits.
    #[inline]
    pub(crate) fn refill(&mut self) {
        while self.bits_in_buf <= 56 {
            let Some(&byte) = self.data.get(self.byte_pos) else {
                break;
            };
            self.byte_pos = self.byte_pos.wrapping_add(1);
            let shift = 56_u32.wrapping_sub(self.bits_in_buf);
            self.buf |= u64::from(byte) << shift;
            self.bits_in_buf = self.bits_in_buf.wrapping_add(8);
        }
    }

    /// Peeks the next `n` bits (`1..=32`) MSB-aligned into the lowest `n` bits of a `u32`.
    /// Missing bits past EOF read as zero; callers validate consumption against `bits_left`.
    #[inline]
    #[must_use]
    pub(crate) fn peek_padded(&mut self, n: u32) -> u32 {
        if self.bits_in_buf < n {
            self.refill();
        }
        let shifted = self.buf >> (64_u32.wrapping_sub(n));
        // `n` is at most 32, so the shifted value fits in `u32`.
        u32::try_from(shifted & u64::from(u32::MAX)).unwrap_or(0)
    }

    /// Consumes `n` bits (`0..=32`), failing if fewer than `n` unread bits remain.
    #[inline]
    pub(crate) fn consume(&mut self, n: u32) -> Result<(), Error> {
        let count = usize::try_from(n).ok().ok_or(Error::Truncated)?;
        if count > self.bits_left {
            return Err(Error::Truncated);
        }
        if n > 0 {
            if self.bits_in_buf < n {
                self.refill();
            }
            self.buf = if n >= 64 { 0 } else { self.buf << n };
            self.bits_in_buf = self.bits_in_buf.saturating_sub(n);
            self.bits_left = self.bits_left.wrapping_sub(count);
        }
        Ok(())
    }

    /// Reads one bit as `bool`.
    #[inline]
    pub(crate) fn read_bool(&mut self) -> Result<bool, Error> {
        if self.bits_left == 0 {
            return Err(Error::Truncated);
        }
        if self.bits_in_buf == 0 {
            self.refill();
        }
        let bit = (self.buf >> 63) != 0;
        self.buf <<= 1;
        self.bits_in_buf = self.bits_in_buf.saturating_sub(1);
        self.bits_left = self.bits_left.wrapping_sub(1);
        Ok(bit)
    }

    /// Reads `n` bits (`0..=32`) as an unsigned `u32`.
    #[inline]
    pub(crate) fn read_bits(&mut self, n: u32) -> Result<u32, Error> {
        if n == 0 {
            return Ok(0);
        }
        let count = usize::try_from(n).ok().ok_or(Error::Truncated)?;
        if count > self.bits_left {
            return Err(Error::Truncated);
        }
        let value = self.peek_padded(n);
        self.buf = if n >= 64 { 0 } else { self.buf << n };
        self.bits_in_buf = self.bits_in_buf.saturating_sub(n);
        self.bits_left = self.bits_left.wrapping_sub(count);
        Ok(value)
    }

    /// Reads `n` bits (`1..=16`) as a two's-complement signed integer.
    #[inline]
    pub(crate) fn read_signed_bits(&mut self, n: u32) -> Result<i32, Error> {
        let raw = self.read_bits(n)?;
        let sign_bit = 1_u32
            .checked_shl(n.wrapping_sub(1))
            .ok_or(Error::Truncated)?;
        let raw_i = i32::try_from(raw).ok().ok_or(Error::Truncated)?;
        if (raw & sign_bit) != 0 {
            let wrap = 1_i32.checked_shl(n).ok_or(Error::Truncated)?;
            Ok(raw_i.wrapping_sub(wrap))
        } else {
            Ok(raw_i)
        }
    }
}
