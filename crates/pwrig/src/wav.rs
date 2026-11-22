//! The mono 16 bit PCM WAV files the stock player reads and the stock recorder writes.

use std::fs;
use std::path::Path;

const RIFF: &[u8; 4] = b"RIFF";
const WAVE: &[u8; 4] = b"WAVE";
const FMT: &[u8; 4] = b"fmt ";
const DATA: &[u8; 4] = b"data";
const PCM: u16 = 1;
const BITS: u16 = 16;
const MONO: u16 = 1;
pub(crate) const BYTES_PER_SAMPLE: u16 = BITS / 8;
/// The length of a PCM `fmt ` chunk's body.
const FMT_LEN: u32 = 16;
/// The bytes of the header that the RIFF length counts: `WAVE`, the `fmt ` chunk and the `data` chunk's header.
const COUNTED_HEADER_LEN: u32 = 36;
/// `RIFF` and its length field, the bytes the RIFF length does not count.
const RIFF_FIELDS_LEN: u32 = 8;
/// The bytes before the first sample in what `encode` writes, and in every file pw-record wrote here (measured at 16 and 48 kHz).
pub(crate) const HEADER_LEN: u32 = RIFF_FIELDS_LEN + COUNTED_HEADER_LEN;

/// A WAV file as the recorder left it.
#[derive(Debug)]
pub(crate) struct Wav {
    pub(crate) rate: u32,
    pub(crate) channels: u16,
    /// The data length the header states: zero until the recorder closes the file.
    declared: u32,
    /// Whether the RIFF length counts every byte after its own field, which `pw-record` writes only on close.
    riff_counts_the_file: bool,
    /// Every sample after the data header, however much the header states.
    pub(crate) samples: Vec<i16>,
    /// Where in the file the first sample starts.
    pub(crate) samples_at: usize,
}

impl Wav {
    /// Whether the header counts exactly the samples on disk, as it does once the recorder closed the file.
    pub(crate) fn closed(&self) -> bool {
        self.riff_counts_the_file
            && usize::try_from(self.declared).ok()
                == self
                    .samples
                    .len()
                    .checked_mul(usize::from(BYTES_PER_SAMPLE))
    }
}

/// Writes `samples` as a mono 16 bit PCM file at `rate`.
pub fn write_wav(path: &Path, rate: u32, samples: &[i16]) {
    let bytes = encode(rate, samples)
        .unwrap_or_else(|| panic!("{} samples is too long for a WAV file", samples.len()));
    fs::write(path, bytes).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

fn encode(rate: u32, samples: &[i16]) -> Option<Vec<u8>> {
    let data_len = u32::try_from(samples.len().checked_mul(usize::from(BYTES_PER_SAMPLE))?).ok()?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(RIFF);
    bytes.extend_from_slice(&COUNTED_HEADER_LEN.checked_add(data_len)?.to_le_bytes());
    bytes.extend_from_slice(WAVE);
    bytes.extend_from_slice(FMT);
    bytes.extend_from_slice(&FMT_LEN.to_le_bytes());
    bytes.extend_from_slice(&PCM.to_le_bytes());
    bytes.extend_from_slice(&MONO.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    let byte_rate = rate.checked_mul(u32::from(BYTES_PER_SAMPLE))?;
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&BYTES_PER_SAMPLE.to_le_bytes());
    bytes.extend_from_slice(&BITS.to_le_bytes());
    bytes.extend_from_slice(DATA);
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    Some(bytes)
}

/// The file's format and samples, or `None` for bytes that are not yet, or never, a 16 bit PCM WAV file.
// Sample input: `RIFF` 36+n `WAVE` `fmt ` 16 [01 00 01 00 80 3e 00 00 00 7d 00 00 02 00 10 00] `data` n, then n bytes of samples.
pub(crate) fn parse(bytes: &[u8]) -> Option<Wav> {
    let (riff, rest) = bytes.split_first_chunk::<4>()?;
    let (riff_len, rest) = rest.split_first_chunk::<4>()?;
    let riff_counts_the_file =
        usize::try_from(u32::from_le_bytes(*riff_len)).ok() == Some(rest.len());
    let (wave, mut rest) = rest.split_first_chunk::<4>()?;
    if riff != RIFF || wave != WAVE {
        return None;
    }
    let mut format = None;
    loop {
        let (id, after_id) = rest.split_first_chunk::<4>()?;
        let (len, body) = after_id.split_first_chunk::<4>()?;
        let declared = u32::from_le_bytes(*len);
        if id == DATA {
            let (rate, channels) = format?;
            let samples_at = bytes.len().checked_sub(body.len())?;
            let samples = body
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| i16::from_le_bytes(*pair))
                .collect();
            return Some(Wav {
                rate,
                channels,
                declared,
                riff_counts_the_file,
                samples,
                samples_at,
            });
        }
        // A chunk of odd length is followed by one byte of padding.
        let padded = usize::try_from(declared)
            .ok()?
            .checked_add(usize::from(declared % 2 == 1))?;
        let (chunk, next) = body.split_at_checked(padded)?;
        if id == FMT {
            format = Some(pcm_format(chunk)?);
        }
        rest = next;
    }
}

/// The rate and channels of a 16 bit PCM `fmt ` chunk body.
// Sample input: `01 00 01 00 80 3e 00 00 00 7d 00 00 02 00 10 00`: PCM, one channel, 16000 Hz, 32000 bytes a second, 2 a frame, 16 bits.
fn pcm_format(chunk: &[u8]) -> Option<(u32, u16)> {
    let (tag, rest) = chunk.split_first_chunk::<2>()?;
    let (channels, rest) = rest.split_first_chunk::<2>()?;
    let (rate, rest) = rest.split_first_chunk::<4>()?;
    let (_byte_rate_and_block, rest) = rest.split_first_chunk::<6>()?;
    let (bits, _) = rest.split_first_chunk::<2>()?;
    let pcm16 = u16::from_le_bytes(*tag) == PCM && u16::from_le_bytes(*bits) == BITS;
    pcm16.then(|| (u32::from_le_bytes(*rate), u16::from_le_bytes(*channels)))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{HEADER_LEN, encode, parse};

    proptest! {
        #[test]
        fn any_bytes_parse_without_a_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            drop(parse(&bytes));
        }

        #[test]
        fn a_written_file_reads_back(rate in 1_u32..200_000, samples in proptest::collection::vec(any::<i16>(), 0..256)) {
            let wav = parse(&encode(rate, &samples).unwrap()).unwrap();
            prop_assert_eq!((wav.rate, wav.channels), (rate, 1));
            prop_assert!(wav.closed());
            prop_assert_eq!(wav.samples_at, usize::try_from(HEADER_LEN).unwrap());
            prop_assert_eq!(wav.samples, samples);
        }
    }

    /// The data length is the last field of the header `encode` writes, and the one a recorder fills in on close.
    const DATA_LEN_AT: std::ops::Range<usize> = 40..44;

    #[test]
    fn a_file_still_being_written_is_not_closed() {
        let mut bytes = encode(16_000, &[1, 2, 3]).unwrap();
        bytes[DATA_LEN_AT].fill(0);
        let wav = parse(&bytes).unwrap();
        assert!(!wav.closed());
        assert_eq!(wav.samples, [1, 2, 3]);
    }

    /// Where `encode` ends its `fmt ` chunk and starts the `data` chunk's header.
    const DATA_CHUNK_AT: usize = 36;

    #[test]
    fn a_chunk_before_the_data_moves_the_first_sample() {
        let mut bytes = encode(16_000, &[1, 2, 3]).unwrap();
        let list = [*b"LIST", 4_u32.to_le_bytes(), *b"INFO"].concat();
        bytes.splice(DATA_CHUNK_AT..DATA_CHUNK_AT, list.iter().copied());
        let wav = parse(&bytes).unwrap();
        assert_eq!(
            wav.samples_at,
            usize::try_from(HEADER_LEN).unwrap() + list.len()
        );
        assert_eq!(wav.samples, [1, 2, 3]);
    }

    /// The RIFF length field, which a recorder fills in on close.
    const RIFF_LEN_AT: std::ops::Range<usize> = 4..8;
    /// The RIFF length `pw-record` 1.4.2 writes when it opens a file, measured.
    const RIFF_LEN_AT_OPEN: u32 = 8;

    #[test]
    fn an_empty_file_still_being_written_is_not_closed() {
        let mut bytes = encode(16_000, &[]).unwrap();
        bytes[RIFF_LEN_AT].copy_from_slice(&RIFF_LEN_AT_OPEN.to_le_bytes());
        let wav = parse(&bytes).unwrap();
        assert!(!wav.closed());
    }
}
