//! The ISO media file: its boxes add up, its offset finds the units, and FFmpeg's own reader takes it for what it is.

use std::fs;

use super::super::analyse::support::SILENT_UNIT;
use super::super::decode::{EXPECTED_CONFIG, SAMPLES_PER_UNIT};
use super::super::fixture::{ffprobe, installed};
use super::*;

/// The units of the file every test here reads.
const UNITS: usize = 200;
/// What ffprobe is asked for.
const SHOW: &str = "stream=codec_name,profile,channels,nb_read_packets";

fn file_of(units: &[&[u8]]) -> Vec<u8> {
    let mut file = Vec::new();
    write(
        &mut file,
        EXPECTED_CONFIG,
        units,
        u32::from(SAMPLES_PER_UNIT),
    )
    .unwrap();
    file
}

/// The size and the type of the box that starts `bytes`, then its body and what follows the box.
// Sample input: `00 00 00 0c 66 72 65 65 01 02 03 04 ..`, a box of 12 bytes named `free` with a body of four.
fn split_box(bytes: &[u8]) -> (&[u8], &[u8], &[u8]) {
    let (size, after_size) = bytes.split_first_chunk::<4>().unwrap();
    let (kind, _) = after_size.split_first_chunk::<4>().unwrap();
    let (whole, after) = bytes.split_at(usize::try_from(u32::from_be_bytes(*size)).unwrap());
    (kind, &whole[8..], after)
}

/// The body of the first box named `kind` among the boxes that fill `bytes`.
fn child(mut bytes: &[u8], kind: [u8; 4]) -> &[u8] {
    loop {
        let (found, body, rest) = split_box(bytes);
        if found == kind {
            return body;
        }
        bytes = rest;
    }
}

#[test]
fn the_boxes_fill_the_file_and_the_chunk_offset_finds_the_units() {
    let units: Vec<&[u8]> = vec![&[0xa1, 0xa2, 0xa3], &[0xb1], &[0xc1, 0xc2]];
    let file = file_of(&units);

    let (kind, _, after_type) = split_box(&file);
    assert_eq!(kind, b"ftyp");
    let (kind, movie, after_movie) = split_box(after_type);
    assert_eq!(kind, b"moov");
    let (kind, coded, rest) = split_box(after_movie);
    assert_eq!(kind, b"mdat");
    assert_eq!(coded, units.concat());
    assert_eq!(rest, b"");

    let media = child(child(movie, *b"trak"), *b"mdia");
    let table = child(child(media, *b"minf"), *b"stbl");
    let sizes = child(table, *b"stsz");
    // After the version and flags: a sample size of 0, the count, then one size a unit.
    assert_eq!(
        sizes[4..],
        [0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 3, 0, 0, 0, 1, 0, 0, 0, 2]
    );
    let times = child(table, *b"stts");
    assert_eq!(times[4..], [0, 0, 0, 1, 0, 0, 0, 3, 0, 0, 0x01, 0xe0]);
    let offset = child(table, *b"stco");
    let offset = usize::try_from(u32::from_be_bytes(offset[8..].try_into().unwrap())).unwrap();
    assert_eq!(file[offset..], units.concat());
}

#[test]
fn the_configuration_is_the_decoder_specific_information() {
    let file = file_of(&[&SILENT_UNIT]);
    // Tag 5 and a length of four, then the configuration, then the SL descriptor: tag 6, one byte, predefined 2.
    let specific = [&[0x05, 0x04][..], &EXPECTED_CONFIG, &[0x06, 0x01, 0x02]].concat();
    let found = file
        .windows(specific.len())
        .filter(|window| *window == specific);
    assert_eq!(found.count(), 1);
}

#[test]
fn a_file_with_no_unit_is_refused() {
    let mut file = Vec::new();
    let refused = write(&mut file, EXPECTED_CONFIG, &[], u32::from(SAMPLES_PER_UNIT)).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(file, b"");
}

#[test]
fn mp4_of_silent_units_is_read_by_ffprobe_as_eld_mono() {
    if !installed("ffprobe") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("eld.mp4");
    fs::write(&path, file_of(&[&SILENT_UNIT[..]; UNITS])).unwrap();
    assert_eq!(
        ffprobe(&path, SHOW),
        format!("codec_name=aac\nprofile=ELD\nchannels=1\nnb_read_packets={UNITS}\n")
    );
}
