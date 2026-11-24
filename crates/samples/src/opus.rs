//! A synthetic tone as frames of the dictation stream: Opus, CELT only, wideband, 20 ms, mono, coded from a generated sine of 1 kHz that peaks at -30 dBFS and never from a recording.

use std::iter;

/// One second of the tone, a length byte in front of every frame.
const TONE: &[u8] = include_bytes!("../data/opus-tone.bin");

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tone_is_50_frames_that_use_the_whole_file() {
        let frames: Vec<&[u8]> = tone().collect();
        assert_eq!(frames.len(), 50);
        let held: usize = frames.iter().map(|frame| frame.len() + 1).sum();
        assert_eq!(held, TONE.len());
    }

    #[test]
    fn a_file_cut_inside_a_frame_ends_at_the_frame_before() {
        let cut: Vec<&[u8]> = frames(&[0x02, 0xb8, 0x00, 0x03, 0xb8]).collect();
        assert_eq!(cut, [&[0xb8, 0x00]]);
    }
}
