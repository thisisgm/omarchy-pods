//! Writes AAC-ELD access units into an ISO base media file, the container FFmpeg reads raw units from.

use std::io::{self, Write};

/// The time scale of the movie and of the track; the decoder takes its rate from the configuration, not from here.
const TIMESCALE: u32 = 48_000;
/// The time scale as the 16.16 fixed point rate of a sample entry; a scale too large for it would not build.
const ENTRY_RATE: u32 = TIMESCALE * (1 << 16);
/// `ftyp`: major brand `isom`, minor version, then the compatible brands `isom` and `mp42`.
const FILE_TYPE: &[u8] = b"isom\x00\x00\x02\x00isommp42";
/// The identity matrix every `mvhd` and `tkhd` carries, in 16.16 and 2.30 fixed point.
const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];
/// Playback rate 1.0 in 16.16 fixed point, then volume 1.0 in 8.8.
const RATE_AND_VOLUME: &[u8] = &[0x00, 0x01, 0x00, 0x00, 0x01, 0x00];
/// `tkhd` flags: the track is enabled and in the movie.
const TRACK_FLAGS: u32 = 3;
/// `mdhd` language code for undetermined, then the reserved word after it.
const LANGUAGE: &[u8] = &[0x55, 0xc4, 0x00, 0x00];
/// `url ` flags: the media is in this file.
const SELF_CONTAINED: u32 = 1;
/// The one track, the one chunk and the one sample description are each numbered 1.
const FIRST: u32 = 1;
/// The descriptor tags of ISO/IEC 14496-1 that an `esds` box nests.
const ES_DESCRIPTOR: u8 = 3;
const DECODER_CONFIG: u8 = 4;
const DECODER_SPECIFIC_INFO: u8 = 5;
const SL_CONFIG: u8 = 6;
/// Object type indication for MPEG-4 audio.
const MPEG4_AUDIO: u8 = 0x40;
/// Stream type audio in the upper six bits, then the upstream flag clear and the reserved bit set.
const AUDIO_STREAM: u8 = 0x15;
/// The predefined SL configuration an MP4 file uses.
const SL_PREDEFINED_MP4: u8 = 2;
/// The longest body a one byte descriptor length can state; a longer one would need the continuation bit.
const DESCRIPTOR_MAX_LEN: u8 = 0x7f;
/// A box starts with its size and its type, four bytes each.
const BOX_HEADER_LEN: usize = 8;

fn too_large(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{what} does not fit its field of an ISO media file"),
    )
}

fn be32(value: usize, what: &str) -> io::Result<[u8; 4]> {
    let value = u32::try_from(value).map_err(|_overflow| too_large(what))?;
    Ok(value.to_be_bytes())
}

/// One box: its size, its type, then its parts in order.
fn boxed(kind: [u8; 4], parts: &[&[u8]]) -> io::Result<Vec<u8>> {
    let body = parts.concat();
    let size = body
        .len()
        .checked_add(BOX_HEADER_LEN)
        .ok_or_else(|| too_large("a box"))?;
    Ok([&be32(size, "a box")?, &kind[..], &body].concat())
}

/// One full box: a box whose body starts with a version of 0 and 24 bits of flags.
fn full(kind: [u8; 4], flags: u32, parts: &[&[u8]]) -> io::Result<Vec<u8>> {
    let flags = flags.to_be_bytes();
    let parts: Vec<&[u8]> = [&flags[..]]
        .into_iter()
        .chain(parts.iter().copied())
        .collect();
    boxed(kind, &parts)
}

/// One descriptor of ISO/IEC 14496-1: a tag, a one byte length, then the body.
fn descriptor(tag: u8, parts: &[&[u8]]) -> io::Result<Vec<u8>> {
    let body = parts.concat();
    let len = u8::try_from(body.len())
        .ok()
        .filter(|&len| len <= DESCRIPTOR_MAX_LEN)
        .ok_or_else(|| too_large("a descriptor"))?;
    Ok([&[tag, len][..], &body].concat())
}

/// `esds`: the decoder configuration, with the four configuration bytes as its decoder specific information.
fn esds(config: [u8; 4]) -> io::Result<Vec<u8>> {
    let specific = descriptor(DECODER_SPECIFIC_INFO, &[&config])?;
    // After the two type bytes: a 24 bit buffer size, the maximum and the average bitrate, all left at 0.
    let decoder = descriptor(
        DECODER_CONFIG,
        &[&[MPEG4_AUDIO, AUDIO_STREAM], &[0; 11], &specific],
    )?;
    let sl = descriptor(SL_CONFIG, &[&[SL_PREDEFINED_MP4]])?;
    // The stream id and the flags of the stream descriptor are 0.
    let stream = descriptor(ES_DESCRIPTOR, &[&[0; 3], &decoder, &sl])?;
    full(*b"esds", 0, &[&stream])
}

/// `stsd` with one `mp4a` entry: mono, 16 bit and the time scale as its rate, none of which the decoder uses.
fn sample_description(config: [u8; 4]) -> io::Result<Vec<u8>> {
    let rate = ENTRY_RATE.to_be_bytes();
    let entry = boxed(
        *b"mp4a",
        &[
            // Six reserved bytes, the data reference index, eight reserved bytes, one channel, 16 bits, four reserved bytes.
            &[0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0],
            &[0, 1, 0, 16, 0, 0, 0, 0],
            &rate,
            &esds(config)?,
        ],
    )?;
    full(*b"stsd", 0, &[&FIRST.to_be_bytes(), &entry])
}

/// `stbl`: every unit is one sample of `samples_per_unit` ticks, and all of them are one chunk at `chunk_offset`.
fn sample_table(
    config: [u8; 4],
    units: &[&[u8]],
    samples_per_unit: u32,
    chunk_offset: usize,
) -> io::Result<Vec<u8>> {
    let count = be32(units.len(), "the number of units")?;
    let mut sizes = Vec::new();
    for unit in units {
        sizes.extend(be32(unit.len(), "a unit")?);
    }
    let one = FIRST.to_be_bytes();
    let offset = be32(chunk_offset, "the offset of the units")?;
    boxed(
        *b"stbl",
        &[
            &sample_description(config)?,
            &full(
                *b"stts",
                0,
                &[&one, &count, &samples_per_unit.to_be_bytes()],
            )?,
            &full(*b"stsc", 0, &[&one, &one, &count, &one])?,
            // A sample size of 0 says the sizes follow, one a sample.
            &full(*b"stsz", 0, &[&[0; 4], &count, &sizes])?,
            &full(*b"stco", 0, &[&one, &offset])?,
        ],
    )
}

/// `moov`: one sound track of the units, whose bytes start `chunk_offset` bytes into the file.
fn movie(
    config: [u8; 4],
    units: &[&[u8]],
    samples_per_unit: u32,
    chunk_offset: usize,
) -> io::Result<Vec<u8>> {
    let ticks = u64::try_from(units.len())
        .ok()
        .and_then(|count| count.checked_mul(u64::from(samples_per_unit)))
        .and_then(|ticks| u32::try_from(ticks).ok())
        .ok_or_else(|| too_large("the duration"))?
        .to_be_bytes();
    let scale = TIMESCALE.to_be_bytes();
    let matrix: Vec<u8> = MATRIX.iter().flat_map(|word| word.to_be_bytes()).collect();
    let track = FIRST.to_be_bytes();
    // The creation and the modification time are 0 in every header.
    let movie_header = full(
        *b"mvhd",
        0,
        &[
            &[0; 8],
            &scale,
            &ticks,
            RATE_AND_VOLUME,
            &[0; 10],
            &matrix,
            &[0; 24],
            // The next track id.
            &2_u32.to_be_bytes(),
        ],
    )?;
    let track_header = full(
        *b"tkhd",
        TRACK_FLAGS,
        &[
            &[0; 8],
            &track,
            &[0; 4],
            &ticks,
            // Eight reserved bytes, layer 0, alternate group 0, volume 1.0, two reserved bytes.
            &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0],
            &matrix,
            // Width and height, which a sound track leaves at 0.
            &[0; 8],
        ],
    )?;
    let media_header = full(*b"mdhd", 0, &[&[0; 8], &scale, &ticks, LANGUAGE])?;
    // A reserved word, the handler type, twelve reserved bytes and an empty name.
    let handler = full(*b"hdlr", 0, &[&[0; 4], b"soun", &[0; 13]])?;
    let data = boxed(
        *b"dinf",
        &[&full(
            *b"dref",
            0,
            &[&FIRST.to_be_bytes(), &full(*b"url ", SELF_CONTAINED, &[])?],
        )?],
    )?;
    let media_info = boxed(
        *b"minf",
        &[
            // Balance 0 and a reserved word.
            &full(*b"smhd", 0, &[&[0; 4]])?,
            &data,
            &sample_table(config, units, samples_per_unit, chunk_offset)?,
        ],
    )?;
    let media = boxed(*b"mdia", &[&media_header, &handler, &media_info])?;
    boxed(
        *b"moov",
        &[&movie_header, &boxed(*b"trak", &[&track_header, &media])?],
    )
}

/// Writes `units` as the samples of one AAC track configured by `config`, each lasting `samples_per_unit` ticks.
pub(crate) fn write(
    out: &mut dyn Write,
    config: [u8; 4],
    units: &[&[u8]],
    samples_per_unit: u32,
) -> io::Result<()> {
    if units.is_empty() {
        let none = "an ISO media file needs at least one unit";
        return Err(io::Error::new(io::ErrorKind::InvalidInput, none));
    }
    let file_type = boxed(*b"ftyp", &[FILE_TYPE])?;
    // The offset is four bytes whatever it holds, so the box written with 0 has the length of the real one.
    let movie_len = movie(config, units, samples_per_unit, 0)?.len();
    let chunk_offset = [file_type.len(), movie_len, BOX_HEADER_LEN]
        .iter()
        .try_fold(0_usize, |sum, &len| sum.checked_add(len))
        .ok_or_else(|| too_large("the offset of the units"))?;
    let coded = boxed(*b"mdat", units)?;
    out.write_all(&file_type)?;
    out.write_all(&movie(config, units, samples_per_unit, chunk_offset)?)?;
    out.write_all(&coded)
}

#[cfg(test)]
mod tests;
