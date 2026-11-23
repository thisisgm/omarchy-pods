//! Synthetic uplink audio: AAC low delay units written bit by bit, and the SDU that carries them.

use rusty_aac::encode::{BitWriter, spectral_emit};

use super::packet;

const OPCODE_UPLINK: u16 = 0x0058;
const SUBTYPE_AUDIO: u16 = 0x0001;
/// Bytes 8 to 21 of an audio SDU, which no known code reads.
const OPAQUE: [u8; 14] = [0x00; 14];
/// What one unit adds to the stamp: the samples it decodes to.
pub const STAMP_STEP: u32 = 480;
/// The length byte in front of a unit holds no more.
const MAX_UNIT_LEN: usize = 255;

const GAIN_BITS: u32 = 8;
const BAND_COUNT_BITS: u32 = 6;
const BOOK_BITS: u32 = 4;
const SECTION_LEN_BITS: u32 = 5;
/// A section length of all ones says another length follows.
const SECTION_LEN_ESCAPE: u32 = 31;
/// The codebook of a band that holds no line.
const ZERO_BOOK: u8 = 0;
/// The first spectral codebook: four lines a codeword, each -1, 0 or 1.
const QUAD_BOOK: u8 = 1;
const LINES_PER_QUAD: u16 = 4;
/// Where each band of a 480 line frame starts at the 48 kHz the configuration declares, and where the last one ends.
const BAND_EDGES: [u16; 36] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64, 72, 80, 88, 96, 108, 120, 132,
    144, 156, 172, 188, 212, 240, 272, 304, 336, 368, 400, 432, 480,
];

/// `00 00`: a gain of 0, no band and no noise shaping, which decodes to silence.
#[must_use]
pub fn silent_unit() -> Vec<u8> {
    let mut bits = BitWriter::new();
    bits.write(0, GAIN_BITS);
    bits.write(0, BAND_COUNT_BITS);
    bits.write_bool(false);
    bits.into_bytes()
}

/// One unit that holds 1 at `line` and 0 at every other, under scale factor `level`, 1.5 dB a step; a line no band holds gives the silent unit.
#[must_use]
pub fn tone_unit(line: u16, level: u8) -> Vec<u8> {
    let mut bands = BAND_EDGES.windows(2).zip(0_u32..);
    let held = bands.find_map(|(edges, before)| match *edges {
        [from, to] if (from..to).contains(&line) => Some((from, to, before)),
        _ => None,
    });
    let Some((from, to, before)) = held else {
        return silent_unit();
    };
    let mut bits = BitWriter::new();
    bits.write(u32::from(level), GAIN_BITS);
    bits.write(before.saturating_add(1), BAND_COUNT_BITS);
    if before > 0 {
        section(&mut bits, ZERO_BOOK, before);
    }
    section(&mut bits, QUAD_BOOK, 1);
    // The band's scale factor is the gain itself: a step of nothing is one 0 bit.
    bits.write_bool(false);
    // No noise shaping follows.
    bits.write_bool(false);
    for quad in (from..to).step_by(usize::from(LINES_PER_QUAD)) {
        let lines = quad..quad.saturating_add(LINES_PER_QUAD);
        let held: Vec<i32> = lines.map(|at| i32::from(at == line)).collect();
        spectral_emit(usize::from(QUAD_BOOK), &held, &mut bits);
    }
    bits.into_bytes()
}

fn section(bits: &mut BitWriter, book: u8, bands: u32) {
    bits.write(u32::from(book), BOOK_BITS);
    let mut left = bands;
    while left >= SECTION_LEN_ESCAPE {
        bits.write(SECTION_LEN_ESCAPE, SECTION_LEN_BITS);
        left = left.saturating_sub(SECTION_LEN_ESCAPE);
    }
    bits.write(left, SECTION_LEN_BITS);
}

/// `04 00 04 00 58 00 01 00`, 14 opaque bytes, then each unit as its stamp, little endian, its length and its bytes, cut to the 255 the length can say.
#[must_use]
pub fn audio_sdu(first_stamp: u32, units: &[Vec<u8>]) -> Vec<u8> {
    let mut body = SUBTYPE_AUDIO.to_le_bytes().to_vec();
    body.extend(OPAQUE);
    let mut stamp = first_stamp;
    for unit in units {
        let unit = unit.get(..MAX_UNIT_LEN).unwrap_or(unit);
        body.extend(stamp.to_le_bytes());
        body.push(u8::try_from(unit.len()).unwrap_or(u8::MAX));
        body.extend(unit);
        stamp = stamp.wrapping_add(STAMP_STEP);
    }
    packet(OPCODE_UPLINK, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_no_band_holds_gives_the_silent_unit() {
        assert_eq!(silent_unit(), [0x00, 0x00]);
        assert_eq!(tone_unit(480, 0xb4), silent_unit());
        assert_eq!(tone_unit(u16::MAX, 0xb4), silent_unit());
        assert_ne!(tone_unit(479, 0xb4), silent_unit());
    }

    #[test]
    fn an_audio_sdu_steps_its_stamps_and_cuts_a_unit_the_length_byte_cannot_say() {
        let sdu = audio_sdu(0xffff_ff00, &[vec![0xa1], vec![], vec![0xb1; 300]]);
        let mut expected = vec![0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x01, 0x00];
        expected.extend(OPAQUE);
        expected.extend([0x00, 0xff, 0xff, 0xff, 0x01, 0xa1]);
        expected.extend(0xffff_ff00_u32.wrapping_add(STAMP_STEP).to_le_bytes());
        expected.push(0x00);
        // Twice the step past the first stamp wraps: 0xffffff00 + 960 is 0x000002c0.
        expected.extend([0xc0, 0x02, 0x00, 0x00, 0xff]);
        expected.extend([0xb1; MAX_UNIT_LEN]);
        assert_eq!(sdu, expected);
    }
}
