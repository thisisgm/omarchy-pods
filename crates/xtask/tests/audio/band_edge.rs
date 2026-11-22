//! `probe audio --whole` on a voice: where its band ends, whatever its tilt, its floor and its length, and what is said where there is no edge to give.

use std::ops::Range;

use tempfile::TempDir;

use super::super::dictation::FRAME;
use super::super::signal::{BIN_HZ, ROOM, SEED, add, band_noise, close, noise, sine, tilted_noise};
use super::whole::{DICTATED, LENGTH_MS, RATE, UPLINK, dictation_run, run_of, whole};
use super::{float_wav, number, row, samples_in};

/// The wall of a wideband stream, and that of the wider stream a call records.
const WIDEBAND_HZ: f32 = 8_000.0;
const CALL_HZ: f32 = 11_500.0;
/// The full amplitude of each sine of the generated voice, which the tilt lowers from 500 Hz up, and its tilt in dB an octave where a test tries no other.
const VOICE: f32 = 0.02;
const TILT_DB: f32 = 9.0;
/// The generated voice speaks, pauses for longer than the three seconds a floor is read from, and speaks again.
const SPOKEN_MS: u32 = 4_000;
const PAUSE_MS: u32 = 3_500;
/// The amplitude of each sine of the hiss a decoded stream holds in its pauses, under the wall of the stream as the voice is.
const HISS: f32 = 2e-5;
/// White noise 60 dB under the voice that tilts 9 dB an octave, and white noise at -84 dBFS.
const FLOOR_60_DB_DOWN: f32 = 6.5e-5;
const FLOOR_84_DBFS: f32 = 1.1e-4;
/// The frames of the short dictation session, and of one a block short of an edge: six frames hold seven whole 16 ms blocks.
const SHORT_FRAMES: usize = 400;
const THIN_FRAMES: usize = 6;
/// A vowel with a gap in it: a fundamental, formants over it, then sibilants from the far side of the gap to the wall, a sine every 100 Hz.
const FUNDAMENTAL_HZ: f32 = 400.0;
const FORMANTS_HZ: f32 = 3_000.0;
const SIBILANTS_HZ: Range<u16> = 5_000..8_000;
const SIBILANT_STEP_HZ: usize = 100;
/// How far in dB each sine of the fundamental lies over one of the voice, and each sibilant under one.
const FUNDAMENTAL_OVER_DB: f32 = 20.0;
const SIBILANT_UNDER_DB: f32 = 20.0;
/// The wall a resampler leaves in the floor of a recording, and how far in dB that floor lies under the voice.
const RESAMPLED_HZ: f32 = 16_000.0;
const RESAMPLED_FLOOR_DB: f32 = 26.0;
/// White noise loud enough that each frame of it is active, how long it lasts, and how far under it a pause lies in dB: near it, and far.
const LOUD: f32 = 0.1;
const LOUD_MS: u32 = 6_500;
const NEAR_DB: f32 = 4.0;
const FAR_DB: f32 = 9.0;

/// What lies under the voice from end to end.
#[derive(Clone, Copy)]
enum Under {
    /// Nothing: the pauses are exact zeros, and no sine lies over the wall.
    DigitalSilence,
    /// The hiss of a decoded stream: noise under the wall alone.
    Hiss,
    /// White noise of this amplitude over the whole band.
    Floor(f32),
}

/// The samples of the stream most tests measure.
fn length() -> usize {
    samples_in(LENGTH_MS, RATE)
}

/// A voice that tilts `tilt_db` an octave under a wall at `wall_hz`, a pause and the voice again, cut at `len` samples: a stream no longer than the first stretch of voice is voice from end to end.
fn speech(tilt_db: f32, wall_hz: f32, under: Under, len: usize) -> Vec<f32> {
    let mut seed = SEED;
    let mut stream = match under {
        Under::DigitalSilence => vec![0.0; len],
        Under::Hiss => band_noise(&mut seed, wall_hz, RATE, HISS, len),
        Under::Floor(amplitude) => noise(&mut seed, amplitude, len),
    };
    let again = samples_in(SPOKEN_MS + PAUSE_MS, RATE);
    let first = samples_in(SPOKEN_MS, RATE).min(len);
    for (from, spoken) in [(0, first), (again, len.saturating_sub(again))] {
        let voice = tilted_noise(&mut seed, wall_hz, RATE, VOICE, tilt_db, spoken);
        add(&mut stream, from, &voice);
    }
    stream
}

fn measured(stream: &[f32]) -> String {
    let home = tempfile::tempdir().unwrap();
    whole(&home, &run_of(&home, &float_wav(RATE, stream)), UPLINK)
}

/// Whether the row gives the wall: the last point of the grid the generated voice fills lies one step under it.
fn at_wall(table: &str, key: &str, wall_hz: f32) -> bool {
    close(number(table, key), f64::from(wall_hz), f64::from(BIN_HZ))
}

#[test]
fn the_band_edge_of_a_voice_is_its_wall_whatever_its_tilt() {
    for tilt_db in [6.0, 12.0] {
        let table = measured(&speech(
            tilt_db,
            WIDEBAND_HZ,
            Under::DigitalSilence,
            length(),
        ));
        assert!(at_wall(&table, "whole_band_edge", WIDEBAND_HZ), "{table}");
        let note = row(&table, "whole_band_edge").1;
        for choice in ["16 ms block", "250 Hz grid", "from 1000 Hz up", "12 dB"] {
            assert!(note.contains(choice), "{note}");
        }
        // The pauses are exact zeros, so there is no floor and no quiet stretch to reach over, and each row says so.
        let (floor, why) = row(&table, "whole_floor");
        assert_eq!(floor, "none");
        assert!(why.contains("3000 ms are digital silence"), "{why}");
        let (reach, why) = row(&table, "whole_reach");
        assert_eq!(reach, "none");
        assert!(why.contains("as digital silence does"), "{why}");
    }
}

#[test]
fn a_loud_fundamental_and_a_gap_between_two_formants_are_no_edge() {
    let mut seed = SEED;
    let len = samples_in(SPOKEN_MS, RATE);
    // A fundamental over all else, formants, nothing across the gap, then a sibilant band to the wall.
    let fundamental = quieter(VOICE, -FUNDAMENTAL_OVER_DB);
    let mut stream = band_noise(&mut seed, FUNDAMENTAL_HZ, RATE, fundamental, len);
    add(
        &mut stream,
        0,
        &tilted_noise(&mut seed, FORMANTS_HZ, RATE, VOICE, TILT_DB, len),
    );
    let sibilant = quieter(VOICE, SIBILANT_UNDER_DB);
    for hz in SIBILANTS_HZ.step_by(SIBILANT_STEP_HZ) {
        add(
            &mut stream,
            0,
            &sine(f32::from(hz), RATE, sibilant, 0.0, len),
        );
    }
    let table = measured(&stream);
    assert!(at_wall(&table, "whole_band_edge", WIDEBAND_HZ), "{table}");
}

#[test]
fn the_reach_of_a_decoded_stream_stops_at_its_band_edge() {
    let table = measured(&speech(TILT_DB, WIDEBAND_HZ, Under::Hiss, length()));
    assert!(at_wall(&table, "whole_band_edge", WIDEBAND_HZ), "{table}");
    // Over the wall the voice and the hiss both leave only what leaks from under it, as far apart as they are under it.
    assert!(at_wall(&table, "whole_reach", WIDEBAND_HZ), "{table}");
    let note = row(&table, "whole_reach").1;
    assert!(
        note.contains("no point over the band edge is read"),
        "{note}"
    );
}

/// An amplitude `db` decibels under `amplitude`, or over it where `db` is negative.
fn quieter(amplitude: f32, db: f32) -> f32 {
    amplitude / 10.0_f32.powf(db / 20.0)
}

#[test]
fn a_voice_reaches_only_where_it_lies_6_db_over_its_pause() {
    // White noise, then less of it for a pause: every frame is active, and their mean lies 3 dB over the near pause and 7 dB over the far one.
    let reach_over = |pause_db: f32| {
        let mut seed = SEED;
        let mut stream = noise(&mut seed, LOUD, samples_in(LOUD_MS, RATE));
        let pause = quieter(LOUD, pause_db);
        stream.extend(noise(&mut seed, pause, samples_in(PAUSE_MS, RATE)));
        let table = measured(&stream);
        // Noise has no band edge, so both reaches are read on the whole grid.
        assert_eq!(row(&table, "whole_band_edge").0, "none", "{table}");
        let (reach, why) = row(&table, "whole_reach");
        (reach.to_owned(), why.to_owned())
    };
    let (reach, why) = reach_over(NEAR_DB);
    assert_eq!(reach, "none");
    assert!(why.contains("at no grid point"), "{why}");
    let (reach, _) = reach_over(FAR_DB);
    assert_eq!(reach, (RATE / 2).to_string());
}

#[test]
fn a_floor_over_the_whole_band_leaves_the_edge_at_the_wall() {
    let table = measured(&speech(
        TILT_DB,
        WIDEBAND_HZ,
        Under::Floor(FLOOR_60_DB_DOWN),
        length(),
    ));
    let over_floor = number(&table, "whole_speech_rms") - number(&table, "whole_floor");
    assert!(close(over_floor, 60.0, 0.5), "{table}");
    assert!(at_wall(&table, "whole_band_edge", WIDEBAND_HZ), "{table}");
    assert!(at_wall(&table, "whole_reach", WIDEBAND_HZ), "{table}");

    let table = measured(&speech(
        TILT_DB,
        CALL_HZ,
        Under::Floor(FLOOR_84_DBFS),
        length(),
    ));
    assert!(close(number(&table, "whole_floor"), -84.0, 0.5), "{table}");
    assert!(at_wall(&table, "whole_band_edge", CALL_HZ), "{table}");
    assert!(at_wall(&table, "whole_reach", CALL_HZ), "{table}");
}

#[test]
fn of_two_walls_the_band_edge_is_the_lower() {
    let mut seed = SEED;
    let len = samples_in(SPOKEN_MS, RATE);
    // A floor under the voice runs on to a wall of its own, as the floor of a resampled recording does.
    let floor = quieter(VOICE, RESAMPLED_FLOOR_DB);
    let mut stream = band_noise(&mut seed, RESAMPLED_HZ, RATE, floor, len);
    add(
        &mut stream,
        0,
        &band_noise(&mut seed, WIDEBAND_HZ, RATE, VOICE, len),
    );
    let table = measured(&stream);
    assert!(at_wall(&table, "whole_band_edge", WIDEBAND_HZ), "{table}");
}

#[test]
fn a_stream_with_no_edge_gets_none_and_the_reason() {
    let mut seed = SEED;
    for (stream, reason) in [
        (
            vec![0.0; length()],
            "it is digital silence: every sample is exactly zero",
        ),
        (
            noise(&mut seed, ROOM, length()),
            "none of its 20 ms frames reaches -60 dBFS",
        ),
        (
            noise(&mut seed, LOUD, length()),
            "from 1000 Hz up no grid point lies 12 dB over all that is found over it",
        ),
    ] {
        let table = measured(&stream);
        let (edge, why) = row(&table, "whole_band_edge");
        assert_eq!(edge, "none", "{table}");
        assert!(why.contains(reason), "{why}");
    }
}

/// The dictation table of a run whose sessions hold `first` and then `second`.
fn dictation_table(home: &TempDir, first: &[f32], second: &[f32]) -> String {
    let sessions = [
        vec![Some(1); first.len() / FRAME],
        vec![Some(2); second.len() / FRAME],
    ]
    .concat();
    let stream = [first, second].concat();
    let run = dictation_run(home, &sessions, &float_wav(RATE, &stream));
    whole(home, &run, DICTATED)
}

#[test]
fn a_short_session_has_its_edge_and_one_of_a_few_frames_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let short = speech(TILT_DB, WIDEBAND_HZ, Under::Hiss, SHORT_FRAMES * FRAME);
    let thin = speech(TILT_DB, WIDEBAND_HZ, Under::Hiss, THIN_FRAMES * FRAME);
    let table = dictation_table(&home, &short, &thin);
    let held = (SHORT_FRAMES * FRAME).to_string();
    assert_eq!(row(&table, "session_1_samples").0, held);
    for key in ["whole_band_edge", "session_1_band_edge"] {
        assert!(at_wall(&table, key, WIDEBAND_HZ), "{table}");
    }
    assert!(at_wall(&table, "session_1_reach", WIDEBAND_HZ), "{table}");
    let (edge, why) = row(&table, "session_2_band_edge");
    assert_eq!(edge, "none", "{table}");
    let thin = "it holds 7 whole 16 ms blocks, and an edge is read from 8 or more";
    assert!(why.contains(thin), "{why}");
}
