//! The WAV writer: a header that carries the rate, and every sample after it.

use proptest::prelude::*;

use super::fixture::{FLOAT, INTEGER, chunk, extensible, file, format, pcm16, riff};
use super::*;

#[test]
fn wav_carries_the_rate_and_every_sample() {
    let samples = [0.0_f32, 1.0, -0.5];
    let mut file = Vec::new();
    write_f32(&mut file, 64_000, &samples).unwrap();

    let header = [
        &b"RIFF"[..],
        &48_u32.to_le_bytes(),
        b"WAVEfmt ",
        &16_u32.to_le_bytes(),
        // IEEE float, one channel.
        &[3, 0, 1, 0],
        &64_000_u32.to_le_bytes(),
        &256_000_u32.to_le_bytes(),
        // Four bytes a frame, 32 bits a sample.
        &[4, 0, 32, 0],
        b"data",
        &12_u32.to_le_bytes(),
    ]
    .concat();
    let (start, coded) = file.split_at(header.len());
    assert_eq!(start, header);
    assert_eq!(
        coded,
        [
            0.0_f32.to_le_bytes(),
            1.0_f32.to_le_bytes(),
            (-0.5_f32).to_le_bytes()
        ]
        .concat()
    );
}

#[test]
fn a_wav_of_no_samples_is_a_header_alone() {
    let mut file = Vec::new();
    write_f32(&mut file, 48_000, &[]).unwrap();
    assert_eq!(file.len(), 44);
    assert_eq!(file[24..28], 48_000_u32.to_le_bytes());
    assert_eq!(file[40..], [0, 0, 0, 0]);
}

#[test]
fn wav_round_trips_and_carries_the_rate() {
    let samples = [0.0_f32, 1.0, -0.5, 0.25];
    for rate in [64_000, 48_000] {
        let mut file = Vec::new();
        write_f32(&mut file, rate, &samples).unwrap();
        let wav = read(&file).unwrap();
        assert_eq!((wav.rate, wav.channels), (rate, 1));
        assert_eq!(wav.samples, samples);
    }
}

#[test]
fn a_wav_longer_than_its_header_says_is_refused() {
    let mut file = Vec::new();
    write_f32(&mut file, 64_000, &[0.5; 8]).unwrap();
    let whole = file.clone();
    file.extend([0; 4]);
    assert_eq!(
        read(&file),
        Err(WavError::Length {
            stated: 68,
            held: 72
        })
    );
    assert_eq!(
        read(&whole[..72]),
        Err(WavError::Length {
            stated: 68,
            held: 64
        })
    );
    assert!(read(&whole).is_ok());
}

#[test]
fn reads_integer_samples_and_takes_the_first_channel() {
    let stereo = pcm16(&[16_384, -1, -32_768, 5]);
    let wav = read(&file(INTEGER, 2, 44_100, 16, &stereo)).unwrap();
    assert_eq!((wav.rate, wav.channels), (44_100, 2));
    assert_eq!(wav.samples, [0.5, -1.0]);

    // 0x400000, the lowest value and one step, three bytes each.
    let deep = [0x00, 0x00, 0x40, 0x00, 0x00, 0x80, 0x01, 0x00, 0x00];
    let wav = read(&file(INTEGER, 1, 48_000, 24, &deep)).unwrap();
    assert_eq!(wav.samples, [0.5, -1.0, 1.0 / 8_388_608.0]);
}

#[test]
fn reads_past_other_chunks_and_through_an_extensible_header() {
    let data = 0.25_f32.to_le_bytes();
    let chunks = [
        chunk(*b"LIST", b"odd"),
        chunk(*b"fmt ", &extensible(FLOAT, 1, 48_000, 32)),
        chunk(*b"fact", &[1, 0, 0, 0]),
        chunk(*b"data", &data),
    ];
    let wav = read(&riff(&chunks.concat())).unwrap();
    assert_eq!((wav.rate, wav.samples), (48_000, vec![0.25]));
}

#[test]
fn a_format_it_does_not_read_is_refused_by_name() {
    for (tag, bits) in [(INTEGER, 8), (INTEGER, 32), (FLOAT, 64), (2, 16)] {
        let refused = read(&file(tag, 1, 48_000, bits, &[0; 16])).unwrap_err();
        assert_eq!(refused, WavError::Unsupported { tag, bits });
        let shown = refused.to_string();
        assert!(
            shown.contains(&format!("{bits} bit")) && shown.contains(&format!("tag {tag}")),
            "{shown}"
        );
    }
    let inner = riff(
        &[
            chunk(*b"fmt ", &extensible(2, 1, 48_000, 16)),
            chunk(*b"data", &[0; 4]),
        ]
        .concat(),
    );
    assert_eq!(
        read(&inner),
        Err(WavError::Unsupported { tag: 2, bits: 16 })
    );
}

#[test]
fn a_file_that_is_not_what_its_header_says_is_refused() {
    let silent = pcm16(&[0, 0]);
    assert_eq!(read(b"RIFF"), Err(WavError::NotWave));
    assert_eq!(read(&riff(b"")[..11]), Err(WavError::NotWave));
    assert_eq!(
        read(&file(INTEGER, 0, 48_000, 16, &silent)),
        Err(WavError::Layout)
    );
    assert_eq!(
        read(&file(INTEGER, 1, 0, 16, &silent)),
        Err(WavError::Layout)
    );
    // The frame of three channels is six bytes, and the header of this one says four.
    let mut lying = format(INTEGER, 3, 48_000, 16);
    lying[12] = 4;
    let lying = riff(&[chunk(*b"fmt ", &lying), chunk(*b"data", &silent)].concat());
    assert_eq!(read(&lying), Err(WavError::Layout));
    assert_eq!(
        read(&file(INTEGER, 2, 48_000, 16, &[0; 6])),
        Err(WavError::PartFrame { len: 6, frame: 4 })
    );

    assert_eq!(
        read(&riff(&chunk(*b"data", &silent))),
        Err(WavError::NoFormat)
    );
    assert_eq!(
        read(&riff(&chunk(*b"fmt ", &format(INTEGER, 1, 48_000, 16)))),
        Err(WavError::NoData)
    );
    assert_eq!(
        read(&riff(&chunk(*b"fmt ", &[1, 0, 1]))),
        Err(WavError::ShortFormat { len: 3 })
    );
    let mut cut = file(INTEGER, 1, 48_000, 16, &silent);
    // The data chunk says it holds one byte more than the file does.
    cut[40] = 5;
    assert_eq!(read(&cut), Err(WavError::ChunkPastEnd { at: 36 }));

    for not_a_number in [f32::NAN, f32::INFINITY] {
        let samples = [0.0_f32.to_le_bytes(), not_a_number.to_le_bytes()].concat();
        assert_eq!(
            read(&file(FLOAT, 1, 64_000, 32, &samples)),
            Err(WavError::NotFinite { sample: 1 })
        );
    }
}

proptest! {
    #[test]
    fn read_never_panics(
        format in proptest::collection::vec(any::<u8>(), 0..48),
        data in proptest::collection::vec(any::<u8>(), 0..64),
        loose in proptest::collection::vec(any::<u8>(), 0..128),
    ) {
        let _refused = read(&loose);
        let _refused = read(&riff(&loose));
        let _read = read(&riff(&[chunk(*b"fmt ", &format), chunk(*b"data", &data)].concat()));
    }
}
