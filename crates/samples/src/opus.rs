//! Two synthetic tones as frames of the dictation stream: Opus, CELT only, wideband, 20 ms, mono, coded from generated sines and never from a recording.

use std::iter;

/// The sine the tone was coded from, the sine of the mark, and the peak of both against full scale.
pub const TONE_HZ: u32 = 1000;
pub const MARK_HZ: u32 = 2500;
pub const TONE_DBFS: f32 = -30.0;
/// The frames of a second, and the band a wideband frame carries.
const FRAMES_A_SECOND: u32 = 50;
const WIDEBAND_HZ: u32 = 8000;

/// Each sine turns a whole number of times in a frame, so its second loops with no step; both are in the band and under full scale.
const _: () = assert!(
    TONE_HZ.is_multiple_of(FRAMES_A_SECOND)
        && MARK_HZ.is_multiple_of(FRAMES_A_SECOND)
        && TONE_HZ < MARK_HZ
        && MARK_HZ < WIDEBAND_HZ
        && TONE_DBFS < 0.0
);

/// One second of each, a length byte in front of every frame.
const TONE: &[u8] = include_bytes!("../data/opus-tone.bin");
const MARK: &[u8] = include_bytes!("../data/opus-mark.bin");

// Sample input: `02 b8 00 01 b8`, a frame of two bytes and a frame of one.
fn frames(file: &'static [u8]) -> impl Iterator<Item = &'static [u8]> + Clone {
    let mut rest = file;
    iter::from_fn(move || {
        let (&length, behind) = rest.split_first()?;
        let (frame, behind) = behind.split_at_checked(usize::from(length))?;
        rest = behind;
        Some(frame)
    })
}

/// The 50 frames of the tone in order; a caller that wants more cycles them.
pub fn tone() -> impl Iterator<Item = &'static [u8]> + Clone {
    frames(TONE)
}

/// The 50 frames of the mark, a second tone a test tells from the first.
pub fn mark() -> impl Iterator<Item = &'static [u8]> + Clone {
    frames(MARK)
}

/// One 20 ms CELT wideband frame encoded from zero samples.
#[must_use]
pub const fn silence() -> &'static [u8] {
    &[0xb8, 0xff, 0xfe]
}

/// One 20 ms frame encoded by libopus from a 1 kHz sine at -50 dBFS RMS.
#[must_use]
pub const fn quiet() -> &'static [u8] {
    &[
        0xb8, 0x9f, 0xf8, 0x8f, 0x22, 0xc2, 0x3f, 0x30, 0xc1, 0x47, 0x06, 0x77, 0xa2, 0xfd, 0xd5,
        0xd0, 0x53, 0xf8, 0xb0, 0xa5, 0xa6, 0x49, 0x85, 0x7a, 0x75, 0xd3, 0xe2, 0x3e, 0x7b, 0x38,
        0x9d, 0x7f, 0x82, 0xd8, 0x81, 0x61, 0x9f, 0x55, 0x8e, 0xd0, 0xdf, 0xb9, 0xe6, 0x80, 0x07,
        0xbf, 0xea, 0x07, 0x2e, 0x4e, 0xd1,
    ]
}

/// One independently coded frame of the 1000 Hz fixture, with Opus prediction disabled.
#[must_use]
pub const fn tone_onset() -> &'static [u8] {
    &[
        0xb8, 0x6f, 0x03, 0xd1, 0x27, 0x26, 0xb0, 0xba, 0xb3, 0x11, 0x7c, 0x2c, 0x6d, 0x54, 0x78,
        0x83, 0xd3, 0x63, 0x8c, 0xa2, 0x54, 0x7c, 0x90, 0x5c, 0xdc, 0x7a, 0xbd, 0xa2, 0x39, 0x35,
        0x50, 0xd6, 0x44, 0xba, 0xb8, 0xb8, 0xdf, 0x5c, 0x1e, 0x1d, 0xc1, 0x40, 0x28, 0x50, 0xe7,
        0xbd, 0xd1, 0x76, 0x0d, 0x14, 0x97, 0xc6, 0x72, 0xde, 0x0f, 0xba, 0x6f, 0x4a, 0x3a, 0x85,
        0xde, 0x8b, 0x3c, 0xc8, 0xb1, 0xc7, 0x2e, 0xe6, 0x80, 0xc0, 0xe1, 0x4b, 0x41, 0x2f, 0x41,
        0x01, 0xf4, 0xd5, 0x77, 0x3c,
    ]
}

/// One independently coded frame of the 2500 Hz fixture, with Opus prediction disabled.
#[must_use]
pub const fn mark_onset() -> &'static [u8] {
    &[
        0xb8, 0x6f, 0x8e, 0xe6, 0x3c, 0xe0, 0x47, 0x3c, 0xd5, 0x32, 0xdc, 0x66, 0x11, 0x7d, 0x20,
        0x66, 0xe7, 0xd8, 0x47, 0x28, 0xed, 0x2c, 0xe4, 0x3e, 0x57, 0xf8, 0xe6, 0xf7, 0x93, 0x70,
        0xb5, 0x1b, 0x42, 0x5f, 0x03, 0x6e, 0xa1, 0x80, 0x7b, 0x0a, 0x16, 0xc7, 0x10, 0x47, 0x0e,
        0xbd, 0xc1, 0x5b, 0xa6, 0xeb, 0x1c, 0x6b, 0x97, 0xfc, 0x82, 0x7b, 0x05, 0xf6, 0xe4, 0x00,
        0x13, 0xb6, 0x0e, 0xdf, 0x25, 0x7f, 0xe6, 0x3a, 0xd4, 0xc5, 0x7c, 0x07, 0xb8, 0xf4, 0xef,
        0x38, 0xcc, 0x8d, 0x47, 0x72,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_fixture_is_50_frames_that_use_the_whole_file() {
        for (frames, file) in [(tone().collect::<Vec<_>>(), TONE), (mark().collect(), MARK)] {
            assert_eq!(frames.len(), 50);
            let held: usize = frames.iter().map(|frame| frame.len() + 1).sum();
            assert_eq!(held, file.len());
        }
        assert_ne!(TONE, MARK);
        let reference = tone().next().unwrap();
        for frame in [silence(), quiet(), tone_onset(), mark_onset()] {
            assert_eq!(frame.first(), reference.first());
            assert!(frame.len() <= reference.len());
        }
    }

    #[test]
    fn a_file_cut_inside_a_frame_ends_at_the_frame_before() {
        let cut: Vec<&[u8]> = frames(&[0x02, 0xb8, 0x00, 0x03, 0xb8]).collect();
        assert_eq!(cut, [&[0xb8, 0x00]]);
    }
}
