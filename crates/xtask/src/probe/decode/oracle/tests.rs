//! FFmpeg as the oracle: what it decodes, how a refusal is told from a decode, and that it agrees with the candidate.

use std::os::unix::process::ExitStatusExt;

use tempfile::TempDir;

use std::fs;
use std::path::PathBuf;

use super::super::super::analyse::support::{SILENT_UNIT, run_with};
use super::super::super::fixture::{TONE_UNIT, installed};
use super::super::candidate::{Candidate, Tally};
use super::super::compare::{TOLERANCE_LSB, compare};
use super::super::refused::{REFUSED_MESSAGE, REFUSED_UNIT};
use super::super::{EXPECTED_CONFIG, SAMPLES_PER_UNIT};
use super::*;

/// The units of each stream decoded here.
const UNITS: usize = 200;
/// A wait status of a child that exited with code 3: the code is the second byte.
const EXITED_WITH_3: i32 = 3 << 8;

/// An empty run in a store under `home`, and where it is.
fn scratch(home: &TempDir) -> (Run, PathBuf) {
    let path = run_with(home, "scratch", &[]);
    (Run::open(&path).unwrap(), path)
}

/// The samples of one unit as a length.
fn frame() -> usize {
    usize::from(SAMPLES_PER_UNIT)
}

fn candidate_samples(unit: &[u8]) -> Vec<f32> {
    let mut candidate = Candidate::new(EXPECTED_CONFIG).unwrap();
    let tally = Tally::of(&mut candidate, &[unit; UNITS], frame());
    assert_eq!((tally.decoded, tally.errors, tally.panics), (UNITS, 0, 0));
    tally.samples
}

#[test]
fn oracle_decodes_silent_units_to_one_frame_of_zero_samples_each() {
    if !installed(FFMPEG) {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let (run, path) = scratch(&home);
    let ran = oracle(
        &run,
        EXPECTED_CONFIG,
        &[&SILENT_UNIT[..]; UNITS],
        SAMPLES_PER_UNIT,
    )
    .unwrap();

    let samples = ran.decoded.unwrap();
    assert_eq!(samples.len(), UNITS * frame());
    assert!(samples.iter().all(|&sample| sample == 0.0));
    assert_eq!(ran.stderr.contents(MAX_STDERR_BYTES).unwrap(), b"");
    let written = ran.pcm.contents(u64::MAX).unwrap().len();
    assert_eq!(written, UNITS * frame() * SAMPLE_BYTES);
    // Nothing has a name until the caller gives one.
    assert_eq!(fs::read_dir(path).unwrap().count(), 0);
}

#[test]
fn oracle_exit_code_is_read_directly() {
    if !installed(FFMPEG) {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let (run, _path) = scratch(&home);
    let ran = oracle(
        &run,
        EXPECTED_CONFIG,
        &[&REFUSED_UNIT[..]; UNITS],
        SAMPLES_PER_UNIT,
    )
    .unwrap();

    let refusal = ran.decoded.unwrap_err();
    assert!(!refusal.status.success(), "{refusal}");
    assert!(refusal.stderr_lines > 0, "{refusal}");
    let messages = String::from_utf8(ran.stderr.contents(MAX_STDERR_BYTES).unwrap()).unwrap();
    assert!(messages.contains(REFUSED_MESSAGE), "{messages}");
    assert_eq!(messages.lines().count(), refusal.stderr_lines);
}

#[test]
fn a_failed_exit_is_a_refusal_with_no_message_at_all() {
    let failed = ExitStatus::from_raw(EXITED_WITH_3);
    let refusal = judge(failed, b"").unwrap_err();
    assert_eq!((refusal.status.code(), refusal.stderr_lines), (Some(3), 0));
}

#[test]
fn a_message_is_a_refusal_with_a_clean_exit() {
    let clean = ExitStatus::from_raw(0);
    let refusal = judge(clean, b"File 'out' already exists. Exiting.\n").unwrap_err();
    assert_eq!((refusal.status.code(), refusal.stderr_lines), (Some(0), 1));
    assert_eq!(judge(clean, b""), Ok(()));
}

#[test]
fn bytes_that_are_not_whole_samples_are_refused() {
    assert_eq!(
        samples_of(&[0, 0, 0x80, 0x3f, 0, 0, 0, 0xbf]).unwrap(),
        [1.0, -0.5]
    );
    let ragged = samples_of(&[0, 0, 0x80, 0x3f, 0]).unwrap_err();
    assert!(
        matches!(ragged, OracleError::NotSamples { bytes: 5 }),
        "{ragged}"
    );
}

#[test]
fn candidate_and_oracle_agree_on_silent_units() {
    if !installed(FFMPEG) {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let (run, _path) = scratch(&home);
    let ran = oracle(
        &run,
        EXPECTED_CONFIG,
        &[&SILENT_UNIT[..]; UNITS],
        SAMPLES_PER_UNIT,
    )
    .unwrap();
    let found = compare(
        &candidate_samples(&SILENT_UNIT),
        &ran.decoded.unwrap(),
        frame(),
    )
    .unwrap();
    assert_eq!((found.offset_units, found.frames), (Some(0), UNITS));
    assert_eq!((found.max_lsb, found.frames_over_one_lsb), (0.0, 0));
}

#[test]
fn candidate_and_oracle_agree_on_a_unit_that_is_not_silence() {
    if !installed(FFMPEG) {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let (run, _path) = scratch(&home);
    let ran = oracle(
        &run,
        EXPECTED_CONFIG,
        &[&TONE_UNIT[..]; UNITS],
        SAMPLES_PER_UNIT,
    )
    .unwrap();
    let from_oracle = ran.decoded.unwrap();
    let loud = from_oracle.iter().filter(|sample| sample.abs() > 0.01);
    assert!(
        loud.count() > UNITS,
        "the oracle's decode is close to silence"
    );

    let found = compare(&candidate_samples(&TONE_UNIT), &from_oracle, frame()).unwrap();
    assert_eq!((found.offset_units, found.frames), (Some(0), UNITS));
    assert!(found.max_lsb <= TOLERANCE_LSB, "{found:?}");
    assert_eq!(found.frames_over_one_lsb, 0);
}
