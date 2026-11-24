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
    }

    #[test]
    fn a_file_cut_inside_a_frame_ends_at_the_frame_before() {
        let cut: Vec<&[u8]> = frames(&[0x02, 0xb8, 0x00, 0x03, 0xb8]).collect();
        assert_eq!(cut, [&[0xb8, 0x00]]);
    }
}
