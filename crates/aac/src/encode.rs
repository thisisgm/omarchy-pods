//! Minimal MSB-first bitstream writer and spectral codebook 1 emitter for synthetic test units.

use crate::tables::huffman::CB1;

/// MSB-first bitstream writer.
#[derive(Debug, Default, Clone)]
pub struct BitWriter {
    bytes: Vec<u8>,
    current: u8,
    bits_used: u8,
}

impl BitWriter {
    /// Creates an empty writer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes the lowest `count` bits of `value` (`0..=32`), MSB first.
    pub fn write(&mut self, value: u32, count: u32) {
        let mut remaining = count;
        while remaining > 0 {
            remaining = remaining.wrapping_sub(1);
            let bit = ((value >> remaining) & 1) != 0;
            self.write_bool(bit);
        }
    }

    /// Writes one bit.
    pub fn write_bool(&mut self, bit: bool) {
        self.current = (self.current << 1) | u8::from(bit);
        self.bits_used = self.bits_used.wrapping_add(1);
        if self.bits_used == 8 {
            self.bytes.push(self.current);
            self.current = 0;
            self.bits_used = 0;
        }
    }

    /// Zero-pads the trailing partial byte (if any) and returns the written bytes.
    #[must_use]
    pub fn into_bytes(mut self) -> Vec<u8> {
        if self.bits_used > 0 {
            let pad = 8_u8.wrapping_sub(self.bits_used);
            self.bytes.push(self.current << pad);
        }
        self.bytes
    }
}

/// Emits one signed 4-tuple using ISO/IEC 14496-3 Table 4.A.2 (codebook 1, values in `-1..=1`).
///
/// Values outside `-1..=1` are clamped into `-1..=1`.
pub fn emit_cb1_quad(quad: [i32; 4], bits: &mut BitWriter) {
    let mut index = 0_usize;
    for value in quad {
        let digit = usize::try_from(value.clamp(-1, 1).wrapping_add(1)).unwrap_or(1);
        index = index.wrapping_mul(3).wrapping_add(digit);
    }
    if let Some(&(len, code)) = CB1.get(index) {
        bits.write(u32::from(code), u32::from(len));
    }
}
