//! Writes decoded audio as a mono 32 bit float WAV, and reads the WAV files a decode or a recorder wrote.

use std::io::{self, Write};

/// The format tag of integer samples.
const INTEGER_PCM: u16 = 1;
/// The format tag of IEEE float samples.
const IEEE_FLOAT: u16 = 3;
/// The format tag of a header whose format identifier, 24 bytes into the chunk, opens with the tag that counts.
const EXTENSIBLE: u16 = 0xfffe;
const INNER_TAG_AT: usize = 24;
/// The fixed fields every format chunk starts with.
const FORMAT_FIELDS_LEN: usize = 16;
/// What the RIFF length counts before the first chunk: the word `WAVE`.
const FORM_LEN: usize = 4;
/// One more than the largest 16 bit and the largest 24 bit sample.
const FULL_SCALE_16: f32 = 32_768.0;
const FULL_SCALE_24: f32 = 8_388_608.0;
/// The weight of the upper two bytes of a 24 bit sample.
const BYTE_WEIGHT: f32 = 256.0;
const CHANNELS: u16 = 1;
const BYTES_PER_SAMPLE: u16 = 4;
const BITS_PER_SAMPLE: u16 = 32;
/// The bytes of a `fmt ` chunk body without an extension.
const FORMAT_LEN: u32 = 16;
/// What the RIFF size counts before the samples: `WAVE`, the `fmt ` chunk whole and the header of `data`.
const BEFORE_SAMPLES_LEN: u32 = 36;

/// Writes `samples` as one mono WAV file labelled `rate` Hz.
pub(crate) fn write_f32(out: &mut dyn Write, rate: u32, samples: &[f32]) -> io::Result<()> {
    let sizes = u32::try_from(samples.len())
        .ok()
        .and_then(|count| count.checked_mul(u32::from(BYTES_PER_SAMPLE)))
        .and_then(|data_len| Some((data_len, data_len.checked_add(BEFORE_SAMPLES_LEN)?)));
    let Some((data_len, riff_len)) = sizes else {
        let too_long = "the samples do not fit the 32 bit sizes of a WAV file";
        return Err(io::Error::new(io::ErrorKind::InvalidInput, too_long));
    };
    let bytes_per_second = rate.saturating_mul(u32::from(BYTES_PER_SAMPLE));
    out.write_all(b"RIFF")?;
    out.write_all(&riff_len.to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&FORMAT_LEN.to_le_bytes())?;
    out.write_all(&IEEE_FLOAT.to_le_bytes())?;
    out.write_all(&CHANNELS.to_le_bytes())?;
    out.write_all(&rate.to_le_bytes())?;
    out.write_all(&bytes_per_second.to_le_bytes())?;
    out.write_all(&BYTES_PER_SAMPLE.to_le_bytes())?;
    out.write_all(&BITS_PER_SAMPLE.to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&data_len.to_le_bytes())?;
    for sample in samples {
        out.write_all(&sample.to_le_bytes())?;
    }
    Ok(())
}

/// What a WAV file holds: the rate its header states, its channels, and the samples of the first of them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Wav {
    pub(crate) rate: u32,
    pub(crate) channels: u16,
    pub(crate) samples: Vec<f32>,
}

/// Why a WAV file was not read; none of them holds a sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum WavError {
    #[error("it does not open with the RIFF header of a WAVE file")]
    NotWave,
    #[error("its header counts {stated} bytes after the RIFF length and the file holds {held}")]
    Length { stated: u64, held: u64 },
    #[error("the chunk at byte {at} says it runs past the end of the file")]
    ChunkPastEnd { at: usize },
    #[error("its samples come before any format chunk")]
    NoFormat,
    #[error("it has no data chunk")]
    NoData,
    #[error("its format chunk is {len} bytes, too short for the format it names")]
    ShortFormat { len: usize },
    #[error(
        "its samples are {bits} bit with format tag {tag}; what is read is 16 and 24 bit integer PCM (tag {INTEGER_PCM}) and 32 bit float (tag {IEEE_FLOAT})"
    )]
    Unsupported { tag: u16, bits: u16 },
    #[error(
        "its header states no channel, no rate, or a frame that is not its channels times its sample size"
    )]
    Layout,
    #[error("its data chunk is {len} bytes, which is not a whole number of {frame} byte frames")]
    PartFrame { len: usize, frame: usize },
    #[error("sample {sample} is not a finite number")]
    NotFinite { sample: usize },
}

/// How one sample is coded.
#[derive(Debug, Clone, Copy)]
enum Coding {
    Integer16,
    Integer24,
    Float32,
}

/// What the format chunk says of the samples.
#[derive(Debug, Clone, Copy)]
struct Format {
    coding: Coding,
    channels: u16,
    rate: u32,
    frame_len: usize,
}

// Sample input: `01 00 02 00 44 ac 00 00 10 b1 02 00 04 00 10 00`, 16 bit integer PCM in two channels at 44100 Hz with four byte frames.
fn read_format(body: &[u8]) -> Result<Format, WavError> {
    let short = WavError::ShortFormat { len: body.len() };
    let Some(&[t0, t1, c0, c1, r0, r1, r2, r3, _, _, _, _, f0, f1, b0, b1]) =
        body.first_chunk::<FORMAT_FIELDS_LEN>()
    else {
        return Err(short);
    };
    let mut tag = u16::from_le_bytes([t0, t1]);
    if tag == EXTENSIBLE {
        let inner = body.get(INNER_TAG_AT..).and_then(<[u8]>::first_chunk::<2>);
        tag = u16::from_le_bytes(*inner.ok_or(short)?);
    }
    let bits = u16::from_le_bytes([b0, b1]);
    let (coding, sample_len) = match (tag, bits) {
        (INTEGER_PCM, 16) => (Coding::Integer16, 2_u16),
        (INTEGER_PCM, 24) => (Coding::Integer24, 3),
        (IEEE_FLOAT, 32) => (Coding::Float32, 4),
        _ => return Err(WavError::Unsupported { tag, bits }),
    };
    let channels = u16::from_le_bytes([c0, c1]);
    let rate = u32::from_le_bytes([r0, r1, r2, r3]);
    let frame_len = u16::from_le_bytes([f0, f1]);
    if channels == 0 || rate == 0 || channels.checked_mul(sample_len) != Some(frame_len) {
        return Err(WavError::Layout);
    }
    Ok(Format {
        coding,
        channels,
        rate,
        frame_len: usize::from(frame_len),
    })
}

// Sample input: `00 40 ff ff`, one frame of 16 bit stereo, read as 0.5: the first channel, and the second is passed over.
fn first_channel(format: Format, data: &[u8]) -> Result<Vec<f32>, WavError> {
    let frames = data.chunks_exact(format.frame_len);
    if !frames.remainder().is_empty() {
        return Err(WavError::PartFrame {
            len: data.len(),
            frame: format.frame_len,
        });
    }
    let mut samples = Vec::with_capacity(frames.len());
    for (sample, frame) in frames.enumerate() {
        let value = match (format.coding, frame) {
            (Coding::Integer16, &[low, high, ..]) => {
                f32::from(i16::from_le_bytes([low, high])) / FULL_SCALE_16
            }
            (Coding::Integer24, &[low, middle, high, ..]) => {
                let upper = f32::from(i16::from_le_bytes([middle, high]));
                (upper * BYTE_WEIGHT + f32::from(low)) / FULL_SCALE_24
            }
            (Coding::Float32, &[b0, b1, b2, b3, ..]) => f32::from_le_bytes([b0, b1, b2, b3]),
            // A frame is at least one sample long, which `read_format` checked.
            _ => return Err(WavError::Layout),
        };
        if !value.is_finite() {
            return Err(WavError::NotFinite { sample });
        }
        samples.push(value);
    }
    Ok(samples)
}

/// Reads a whole WAV file; every length in it is checked against the bytes that are there, so none sizes anything.
// Sample input: `RIFF` and a length, `WAVE`, a `fmt ` chunk, any other chunks, then `data` and the samples.
pub(crate) fn read(file: &[u8]) -> Result<Wav, WavError> {
    let Some((
        &[
            b'R',
            b'I',
            b'F',
            b'F',
            l0,
            l1,
            l2,
            l3,
            b'W',
            b'A',
            b'V',
            b'E',
        ],
        mut chunks,
    )) = file.split_first_chunk()
    else {
        return Err(WavError::NotWave);
    };
    let stated = u64::from(u32::from_le_bytes([l0, l1, l2, l3]));
    let held = u64::try_from(chunks.len().saturating_add(FORM_LEN)).unwrap_or(u64::MAX);
    if stated != held {
        return Err(WavError::Length { stated, held });
    }
    let mut format = None;
    while let Some((&[n0, n1, n2, n3, l0, l1, l2, l3], after)) = chunks.split_first_chunk() {
        let at = file.len().saturating_sub(chunks.len());
        let len = usize::try_from(u32::from_le_bytes([l0, l1, l2, l3])).unwrap_or(usize::MAX);
        let body = after.get(..len).ok_or(WavError::ChunkPastEnd { at })?;
        match &[n0, n1, n2, n3] {
            b"fmt " => format = Some(read_format(body)?),
            b"data" => {
                let format = format.ok_or(WavError::NoFormat)?;
                return Ok(Wav {
                    rate: format.rate,
                    channels: format.channels,
                    samples: first_channel(format, body)?,
                });
            }
            _ => {}
        }
        // A chunk of odd length is followed by one pad byte.
        let padded = len.saturating_add(len % 2);
        chunks = after.get(padded..).unwrap_or(&[]);
    }
    Err(WavError::NoData)
}

#[cfg(test)]
pub(crate) mod fixture;
#[cfg(test)]
mod tests;
