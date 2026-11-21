//! Writes decoded audio as a WAV file of 32 bit floats, mono, with the rate the caller names.

use std::io::{self, Write};

/// The format tag of IEEE float samples.
const IEEE_FLOAT: u16 = 3;
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

#[cfg(test)]
mod tests;
