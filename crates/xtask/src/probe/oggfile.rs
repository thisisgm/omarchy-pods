//! Writes Opus frames as one Ogg logical stream, a page for every packet, which FFmpeg reads as Ogg Opus.

use std::io::{self, Write};

/// The four bytes every page opens with.
const CAPTURE_PATTERN: &[u8; 4] = b"OggS";
const STREAM_VERSION: u8 = 0;
/// Header type flags: the first page of the stream and the last.
const FIRST_PAGE: u8 = 0x02;
const LAST_PAGE: u8 = 0x04;
/// The stream's serial number, the only stream of the file.
const SERIAL: u32 = 1;
/// Where the checksum sits in a page, after the pattern, the version, the flags, the position, the serial and the sequence number.
const CHECKSUM_AT: usize = 22;
/// The generator polynomial of the page checksum: no reflection, zero at the start, nothing inverted at the end.
const CHECKSUM_POLYNOMIAL: u32 = 0x04c1_1db7;
const TOP_BIT: u32 = 0x8000_0000;
const BITS_PER_BYTE: u8 = 8;
/// A lacing value under this ends its packet; a page lists at most that many values.
const FULL_SEGMENT: usize = 255;
/// The identification header, version 1 of it.
const HEAD_MAGIC: &[u8; 8] = b"OpusHead";
const HEAD_VERSION: u8 = 1;
const CHANNELS: u8 = 1;
/// Samples at 48 kHz a player drops from the start: none, so every decoded sample is kept.
const PRE_SKIP: u16 = 0;
/// The rate the head states: the sample clock of the microphone, which no decoder reads.
const INPUT_RATE_HZ: u32 = 16_000;
const OUTPUT_GAIN: i16 = 0;
/// One stream of one or two channels in their coded order.
const MAPPING_FAMILY: u8 = 0;
const TAGS_MAGIC: &[u8; 8] = b"OpusTags";
const VENDOR: &[u8] = b"probe";
const NO_COMMENTS: u32 = 0;
/// What one 20 ms frame adds to the position of its page: its samples at 48 kHz, whatever rate the head states.
const POSITION_STEP: u64 = 960;

fn too_large(what: &str) -> io::Error {
    let message = format!("{what} is more than an Ogg stream of one packet a page holds");
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn checksum(page: &[u8]) -> u32 {
    let mut register = 0_u32;
    for byte in page {
        register ^= u32::from(*byte) << 24;
        for _ in 0..BITS_PER_BYTE {
            let carry = register & TOP_BIT != 0;
            register <<= 1;
            if carry {
                register ^= CHECKSUM_POLYNOMIAL;
            }
        }
    }
    register
}

/// The lacing values of one packet: a full segment for every 255 bytes, then the rest, which may be 0.
fn lacing(packet_len: usize) -> io::Result<Vec<u8>> {
    let full = packet_len.checked_div(FULL_SEGMENT).unwrap_or(0);
    let rest = packet_len.checked_rem(FULL_SEGMENT).unwrap_or(0);
    if full >= FULL_SEGMENT {
        return Err(too_large(&format!("a packet of {packet_len} bytes")));
    }
    let mut values = vec![u8::MAX; full];
    values.push(u8::try_from(rest).unwrap_or(u8::MAX));
    Ok(values)
}

/// One page that holds one whole packet.
fn page(flags: u8, position: u64, sequence: u32, packet: &[u8]) -> io::Result<Vec<u8>> {
    let lacing = lacing(packet.len())?;
    let segments = u8::try_from(lacing.len()).map_err(|_longer| too_large("a lacing table"))?;
    let mut page = [
        &CAPTURE_PATTERN[..],
        &[STREAM_VERSION, flags],
        &position.to_le_bytes(),
        &SERIAL.to_le_bytes(),
        &sequence.to_le_bytes(),
        &[0; 4],
        &[segments],
        &lacing,
        packet,
    ]
    .concat();
    let sum = checksum(&page).to_le_bytes();
    if let Some(field) = page.get_mut(CHECKSUM_AT..CHECKSUM_AT.saturating_add(sum.len())) {
        field.copy_from_slice(&sum);
    }
    Ok(page)
}

/// Writes `frames` as Ogg Opus: mono, no pre-skip, and a position that steps 960 a frame.
pub(crate) fn write(out: &mut dyn Write, frames: &[&[u8]]) -> io::Result<()> {
    let Some((last, before)) = frames.split_last() else {
        let none = "an Ogg Opus stream needs at least one frame";
        return Err(io::Error::new(io::ErrorKind::InvalidInput, none));
    };
    let head = [
        &HEAD_MAGIC[..],
        &[HEAD_VERSION, CHANNELS],
        &PRE_SKIP.to_le_bytes(),
        &INPUT_RATE_HZ.to_le_bytes(),
        &OUTPUT_GAIN.to_le_bytes(),
        &[MAPPING_FAMILY],
    ]
    .concat();
    let vendor_len = u32::try_from(VENDOR.len()).unwrap_or(u32::MAX);
    let tags = [
        &TAGS_MAGIC[..],
        &vendor_len.to_le_bytes(),
        VENDOR,
        &NO_COMMENTS.to_le_bytes(),
    ]
    .concat();
    out.write_all(&page(FIRST_PAGE, 0, 0, &head)?)?;
    out.write_all(&page(0, 0, 1, &tags)?)?;
    let mut position = 0_u64;
    let mut sequence = 1_u32;
    let flags = before.iter().map(|_| 0).chain([LAST_PAGE]);
    for (frame, flags) in before.iter().chain([last]).zip(flags) {
        position = position.saturating_add(POSITION_STEP);
        sequence = sequence
            .checked_add(1)
            .ok_or_else(|| too_large("the count of frames"))?;
        out.write_all(&page(flags, position, sequence, frame)?)?;
    }
    Ok(())
}
