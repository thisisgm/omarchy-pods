//! The WAV writer: a header that carries the rate, and every sample after it.

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
