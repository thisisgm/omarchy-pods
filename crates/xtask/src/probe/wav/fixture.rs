//! WAV files built field by field, for the tests of the reader and of the built command.

/// The format tags of integer samples, of float samples and of a header that names its format further in.
pub(crate) const INTEGER: u16 = 1;
pub(crate) const FLOAT: u16 = 3;
const EXTENSIBLE: u16 = 0xfffe;
/// The bytes of `WAVE`, which the length in the header counts with the chunks.
const FORM_LEN: usize = 4;
/// The bytes an extensible header holds after its first 18: valid bits, speaker mask and format identifier.
const EXTENSION_LEN: u16 = 22;
/// What follows the inner tag in the format identifier of an extensible header.
const IDENTIFIER_TAIL: [u8; 14] = [
    0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];

/// One chunk: its name, its length, its body, and the pad byte an odd length takes.
pub(crate) fn chunk(name: [u8; 4], body: &[u8]) -> Vec<u8> {
    let len = u32::try_from(body.len()).unwrap();
    let pad = [0_u8].repeat(body.len() % 2);
    [&name[..], &len.to_le_bytes(), body, &pad].concat()
}

/// A WAVE file around `chunks`, with the length its header should state.
pub(crate) fn riff(chunks: &[u8]) -> Vec<u8> {
    let len = u32::try_from(chunks.len().saturating_add(FORM_LEN)).unwrap();
    [b"RIFF", &len.to_le_bytes()[..], b"WAVE", chunks].concat()
}

/// The 16 bytes every format chunk starts with, for frames of whole `bits` bit samples.
pub(crate) fn format(tag: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
    let frame = channels.saturating_mul(bits / 8);
    let bytes_a_second = rate.saturating_mul(u32::from(frame));
    [
        &tag.to_le_bytes()[..],
        &channels.to_le_bytes(),
        &rate.to_le_bytes(),
        &bytes_a_second.to_le_bytes(),
        &frame.to_le_bytes(),
        &bits.to_le_bytes(),
    ]
    .concat()
}

/// The format chunk FFmpeg writes for samples over 16 bits: the tag says extensible and the identifier names `inner`.
pub(crate) fn extensible(inner: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
    let mut body = format(EXTENSIBLE, channels, rate, bits);
    body.extend(
        [
            &EXTENSION_LEN.to_le_bytes()[..],
            &bits.to_le_bytes(),
            &[0; 4],
        ]
        .concat(),
    );
    body.extend(inner.to_le_bytes());
    body.extend(IDENTIFIER_TAIL);
    body
}

/// A whole file: a plain format chunk, then `data` as the samples.
pub(crate) fn file(tag: u16, channels: u16, rate: u32, bits: u16, data: &[u8]) -> Vec<u8> {
    let format = chunk(*b"fmt ", &format(tag, channels, rate, bits));
    riff(&[format, chunk(*b"data", data)].concat())
}

/// 16 bit samples as the bytes of a data chunk.
pub(crate) fn pcm16(samples: &[i16]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect()
}
