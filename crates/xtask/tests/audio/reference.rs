//! `probe audio` with a reference recording: the taps and the speech of both, and the head of each start against its body.

use std::fs::{self, Permissions};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use super::super::signal::{
    ROOM, SEED, add, bursts, close, noise, rms_dbfs, samples_in, sine, span,
};
use super::super::wav::{FLOAT, INTEGER, chunk, extensible, file, pcm16, riff};
use super::super::xorshift::next_random;
use super::{
    COUNT, COUNT_ON, NS_PER_MS, RATE, REALTIME_AT_ZERO_NS, TAP, analysed, decoded, marks, number,
    probe, put_file, refusal, row, table_of, uplink_lines,
};

/// The rate of the reference recorder.
pub(super) const RECORDER_RATE: u32 = 48_000;
/// The recorder of the tap run starts this long before the zero of the run.
pub(super) const BEFORE_ZERO_MS: u32 = 300;
/// A mode with a group bit, which the bounded read refuses.
const GROUP_READABLE: u32 = 0o640;
/// The clicks of the tap run, in milliseconds of the stream.
pub(super) const CLICKS_MS: [u32; 4] = [2000, 2300, 9500, 9700];
/// Where the sentence of the tap run starts in the stream, and how long it lasts.
const SENTENCE_FROM_MS: u32 = 5200;
const SENTENCE_MS: u32 = 2600;
/// The bytes of one frame of the tap run's recorder: two channels of 16 bits.
const FRAME_BYTES: usize = 4;
/// Where the cue that ends the sentence window of the tap run lies in its recorder: 8 s into the run, and the 300 ms before its zero.
const SENTENCE_WINDOW_END_MS: u32 = 8300;
/// The slots of a burst train of 18 seconds, and of the 19 seconds the search of a start 12.5 s into a run ends at.
const SCENE_SLOTS: usize = 900;
const SEARCHED_SLOTS: usize = 950;
/// The recorder of a scene starts this long before the zero of the run.
pub(super) const SCENE_BEFORE_ZERO_MS: u32 = 500;
/// What a start of five seconds holds of the stream, and the first second inside that.
pub(super) const START_MS: u32 = 4980;
pub(super) const HEAD_MS: u32 = 1000;
/// What a start of two seconds holds of the stream, and the head and the body no start under four seconds has.
const SHORT_START_MS: u32 = 1980;
const HEAD_AND_BODY_MS: u32 = 4000;
/// Why a start has no row when the reference ends or begins inside what is searched.
const SEARCH_NOT_HELD: &str = "the reference does not hold the 2000 ms to either side of where the start mark puts the head and the body";

/// The command on `run` with the reference at `path`, whose first sample has the wall clock time `first_sample_ns`.
fn against_at(home: &TempDir, run: &Path, path: &Path, first_sample_ns: u64) -> Command {
    let mut command = probe(home, "audio", run);
    command.arg("--reference").arg(path);
    command.args(["--reference-start-ns", &first_sample_ns.to_string()]);
    command
}

/// That command for a reference whose first sample was `before_zero_ms` before the zero of the run.
pub(super) fn against(home: &TempDir, run: &Path, path: &Path, before_zero_ms: u32) -> Command {
    let before_ns = u64::from(before_zero_ms).saturating_mul(NS_PER_MS);
    against_at(
        home,
        run,
        path,
        REALTIME_AT_ZERO_NS.saturating_sub(before_ns),
    )
}

/// A start of the scene from `from_ms` on, whose first second is the scene from `head_from_ms` on.
pub(super) fn start_with_head_from(scene: &[f32], from_ms: u32, head_from_ms: u32) -> Vec<f32> {
    let head = &scene[span(head_from_ms, HEAD_MS, RATE)];
    let rest_ms = START_MS.saturating_sub(HEAD_MS);
    let rest = &scene[span(from_ms.saturating_add(HEAD_MS), rest_ms, RATE)];
    [head, rest].concat()
}

/// A `firstword` run whose starts, five seconds each, hold `segments`.
pub(super) fn firstword_run(home: &TempDir, starts_ms: &[u32], segments: &[Vec<f32>]) -> PathBuf {
    let starts: Vec<(u32, u32)> = starts_ms.iter().map(|&start_ms| (start_ms, 5)).collect();
    let lines = [vec![(0, "cue", COUNT_ON)], uplink_lines(&starts)].concat();
    let run = analysed(home, "firstword", &marks(&lines), &starts);
    decoded(&run, RATE, &segments.concat());
    run
}

/// Ten seconds with a sentence between two tap windows as the pods heard it, taps at `clicks_ms`, and a recorder beside them that heard the taps at `heard_ms`.
pub(super) fn tap_run(home: &TempDir, clicks_ms: &[u32], heard_ms: &[u32]) -> (PathBuf, Vec<u8>) {
    let lines = [
        (0, "start", ""),
        (500, "cue", TAP),
        (4000, "cue", COUNT),
        (8000, "cue", TAP),
        (10_600, "stop", ""),
    ];
    let run = analysed(home, "voice", &marks(&lines), &[(0, 10)]);
    let mut seed = SEED;
    let mut stream = noise(&mut seed, ROOM, samples_in(9990, RATE));
    for &click_ms in clicks_ms {
        let click = noise(&mut seed, 0.5, samples_in(1, RATE));
        add(&mut stream, samples_in(click_ms, RATE), &click);
    }
    let sentence = sine(300.0, RATE, 0.25, 0.0, samples_in(SENTENCE_MS, RATE));
    add(&mut stream, samples_in(SENTENCE_FROM_MS, RATE), &sentence);
    decoded(&run, RATE, &stream);

    // The recorder hears a square wave of the sentence's size on its first channel; its second holds one value that is never read.
    let mut heard = vec![0_i16; samples_in(11_500, RECORDER_RATE)];
    for &click_ms in heard_ms {
        let click = span(click_ms.saturating_add(BEFORE_ZERO_MS), 1, RECORDER_RATE);
        for sample in &mut heard[click] {
            let [low, high, ..] = next_random(&mut seed).to_le_bytes();
            *sample = i16::from_le_bytes([low, high]) / 4;
        }
    }
    let said_from_ms = SENTENCE_FROM_MS.saturating_add(BEFORE_ZERO_MS);
    let said = span(said_from_ms, SENTENCE_MS, RECORDER_RATE);
    for (index, sample) in heard[said].iter_mut().enumerate() {
        *sample = if index % 160 < 80 { 8192 } else { -8192 };
    }
    let frames: Vec<i16> = heard.iter().flat_map(|&first| [first, 30_000]).collect();
    (run, pcm16(&frames))
}

#[test]
fn a_reference_gives_the_levels_and_the_taps_of_both_recordings() {
    let home = tempfile::tempdir().unwrap();
    let (run, frames) = tap_run(&home, &CLICKS_MS, &CLICKS_MS);
    let reference = home.path().join("recorder.wav");
    put_file(
        home.path(),
        "recorder.wav",
        &file(INTEGER, 2, RECORDER_RATE, 16, &frames),
    );
    let measured = against(&home, &run, &reference, BEFORE_ZERO_MS)
        .output()
        .unwrap();
    let table = table_of(&measured, &run);

    assert_eq!(row(&table, "reference_rate").0, "48000");
    let (samples, channels) = row(&table, "reference_samples");
    assert_eq!(
        (samples, channels),
        ("552000", "of 2 channels, the first is read")
    );
    // A square wave a quarter of full scale is 3.01 dB over a sine of that amplitude.
    let (level, note) = row(&table, "cue_2_reference_rms");
    assert!(close(level.parse().unwrap(), -12.04, 0.05), "{level}");
    assert!(note.ends_with("recorder clock"), "{note}");
    assert!(
        close(number(&table, "cue_2_pods_minus_reference"), -3.01, 0.05),
        "{table}"
    );

    let (ratio, note) = row(&table, "tap_spacing_ratio");
    assert!(ratio.parse::<f64>().unwrap().abs() < 20.0, "{ratio}");
    assert!(note.ends_with("4 taps"), "{note}");
    // A tap's SDU arrives 30 ms after the SDU's first sample, which lies up to 20 ms before the tap.
    for (name, behind_ms) in [("min", 10.0), ("median", 10.0), ("max", 20.0)] {
        let (behind, note) = row(&table, &format!("tap_arrival_minus_reference_{name}"));
        assert!(
            close(behind.parse().unwrap(), behind_ms, 1.5),
            "{name}: {behind}"
        );
        assert!(
            note.contains("4 taps") && note.ends_with("recorder clock"),
            "{note}"
        );
    }
}

#[test]
fn taps_are_shown_the_same_from_three_on() {
    let too_few = "2 taps, too few to show that they are the same taps in both";
    for (taps, said) in [(2, too_few), (3, "3 taps")] {
        let home = tempfile::tempdir().unwrap();
        // Both recordings hold the first taps of the run, and with two of them none in the last window.
        let (run, frames) = tap_run(&home, &CLICKS_MS[..taps], &CLICKS_MS[..taps]);
        let reference = home.path().join("recorder.wav");
        put_file(
            home.path(),
            "recorder.wav",
            &file(INTEGER, 2, RECORDER_RATE, 16, &frames),
        );
        let mut measured = against(&home, &run, &reference, BEFORE_ZERO_MS);
        let table = table_of(&measured.output().unwrap(), &run);

        // The taps lie on whole milliseconds of both recordings, so both give the same time from the first to the last.
        let (ratio, note) = row(&table, "tap_spacing_ratio");
        assert_eq!(ratio, "0.0", "{taps}");
        assert!(note.ends_with(&format!("; {said}")), "{note}");
        let (behind, note) = row(&table, "tap_arrival_minus_reference_median");
        assert!(close(behind.parse().unwrap(), 10.0, 1.5), "{behind}");
        assert!(
            note.ends_with(&format!("; {said}; recorder clock")),
            "{note}"
        );
    }
}

#[test]
fn a_reference_labelled_with_another_rate_moves_the_tap_ratio() {
    let home = tempfile::tempdir().unwrap();
    let (run, frames) = tap_run(&home, &CLICKS_MS, &CLICKS_MS);
    let reference = home.path().join("recorder.wav");
    put_file(
        home.path(),
        "recorder.wav",
        &file(INTEGER, 2, 44_100, 16, &frames),
    );
    let measured = against(&home, &run, &reference, BEFORE_ZERO_MS)
        .output()
        .unwrap();
    let table = table_of(&measured, &run);
    // The same samples at 44.1 kHz last 48 / 44.1 as long, so the stream takes 8.1 percent less than they do.
    assert!(
        close(number(&table, "tap_spacing_ratio"), -81_250.0, 30.0),
        "{table}"
    );
}

#[test]
fn a_reference_that_holds_no_window_gives_no_figure() {
    let home = tempfile::tempdir().unwrap();
    let (run, frames) = tap_run(&home, &CLICKS_MS, &CLICKS_MS);
    let reference = home.path().join("recorder.wav");
    put_file(
        home.path(),
        "recorder.wav",
        &file(INTEGER, 2, RECORDER_RATE, 16, &frames),
    );
    // The recorder ran for 11.5 s from a minute before the run began.
    let measured = against(&home, &run, &reference, 60_000).output().unwrap();
    let table = table_of(&measured, &run);

    let not_held = "the reference does not hold the whole window";
    assert_eq!(row(&table, "cue_2_reference_rms"), ("none", not_held));
    assert_eq!(
        row(&table, "cue_2_pods_minus_reference"),
        ("none", not_held)
    );
    for key in ["tap_spacing_ratio", "tap_arrival_minus_reference_median"] {
        assert_eq!(
            row(&table, key),
            (
                "none",
                "cue_1: the reference does not hold the whole window"
            )
        );
    }
    assert!(
        close(number(&table, "cue_2_speech_rms"), -15.05, 0.05),
        "{table}"
    );
}

#[test]
fn a_window_has_reference_figures_only_when_the_reference_holds_all_of_it() {
    // With the frames up to the cue that ends the sentence window the recorder holds that window, and with one fewer it does not.
    let whole = samples_in(SENTENCE_WINDOW_END_MS, RECORDER_RATE);
    for (kept, held) in [(whole, true), (whole.saturating_sub(1), false)] {
        let home = tempfile::tempdir().unwrap();
        let (run, frames) = tap_run(&home, &CLICKS_MS, &CLICKS_MS);
        let reference = home.path().join("recorder.wav");
        let kept_bytes = kept.saturating_mul(FRAME_BYTES);
        put_file(
            home.path(),
            "recorder.wav",
            &file(INTEGER, 2, RECORDER_RATE, 16, &frames[..kept_bytes]),
        );
        let measured = against(&home, &run, &reference, BEFORE_ZERO_MS)
            .output()
            .unwrap();
        let table = table_of(&measured, &run);

        assert_eq!(row(&table, "reference_samples").0, kept.to_string());
        let (level, less) = (
            row(&table, "cue_2_reference_rms"),
            row(&table, "cue_2_pods_minus_reference"),
        );
        if held {
            assert!(close(level.0.parse().unwrap(), -12.04, 0.05), "{level:?}");
            assert!(close(less.0.parse().unwrap(), -3.01, 0.05), "{less:?}");
        } else {
            let not_held = "the reference does not hold the whole window";
            assert_eq!((level, less), (("none", not_held), ("none", not_held)));
        }
        // The window of the last taps ends after the recorder did.
        for key in ["tap_spacing_ratio", "tap_arrival_minus_reference_max"] {
            assert_eq!(
                row(&table, key),
                (
                    "none",
                    "cue_3: the reference does not hold the whole window"
                )
            );
        }
    }
}

#[test]
fn latency_rows_need_both_reference_arguments() {
    let home = tempfile::tempdir().unwrap();
    let (run, frames) = tap_run(&home, &CLICKS_MS, &CLICKS_MS);
    let reference = home.path().join("recorder.wav");
    put_file(
        home.path(),
        "recorder.wav",
        &file(INTEGER, 2, RECORDER_RATE, 16, &frames),
    );

    let mut alone = probe(&home, "audio", &run);
    alone.arg("--reference").arg(&reference);
    let said = refusal(&alone.output().unwrap(), &run);
    assert!(
        said.contains("--reference-start-ns <REFERENCE_START_NS>"),
        "{said}"
    );
    let mut alone = probe(&home, "audio", &run);
    alone.args(["--reference-start-ns", "1763640000000000000"]);
    let said = refusal(&alone.output().unwrap(), &run);
    assert!(said.contains("--reference <REFERENCE>"), "{said}");

    // The reference is read by the rules of a capture: the caller's own file, closed to others, never through a link.
    fs::set_permissions(&reference, Permissions::from_mode(GROUP_READABLE)).unwrap();
    let said = refusal(
        &against(&home, &run, &reference, BEFORE_ZERO_MS)
            .output()
            .unwrap(),
        &run,
    );
    assert!(said.contains("group and others have no access"), "{said}");
    let link = home.path().join("link.wav");
    symlink(&reference, &link).unwrap();
    let said = refusal(
        &against(&home, &run, &link, BEFORE_ZERO_MS)
            .output()
            .unwrap(),
        &run,
    );
    assert!(said.contains("is a symbolic link"), "{said}");
    put_file(home.path(), "short.wav", b"RIFF");
    let short = home.path().join("short.wav");
    let said = refusal(
        &against(&home, &run, &short, BEFORE_ZERO_MS)
            .output()
            .unwrap(),
        &run,
    );
    assert!(
        said.contains("short.wav was not read: it does not open with the RIFF header"),
        "{said}"
    );
}

/// What a recorder `heard` of the scene, in a float file with the extensible header.
fn scene_recording(home: &TempDir, heard: &[f32]) -> PathBuf {
    let recorded: Vec<u8> = heard
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect();
    let format = chunk(*b"fmt ", &extensible(FLOAT, 1, RECORDER_RATE, 32));
    put_file(
        home.path(),
        "recorder.wav",
        &riff(&[format, chunk(*b"data", &recorded)].concat()),
    );
    home.path().join("recorder.wav")
}

#[test]
fn the_head_of_each_start_is_placed_against_its_body() {
    let home = tempfile::tempdir().unwrap();
    let lines = [
        (0, "cue", COUNT_ON),
        (1500, "start", ""),
        (6500, "stop", ""),
        (8000, "start", ""),
        (13_000, "stop", ""),
        (14_000, "start", ""),
        (16_100, "stop", ""),
    ];
    let run = analysed(
        &home,
        "firstword",
        &marks(&lines),
        &[(1500, 5), (8000, 5), (14_000, 2)],
    );
    // The recorder runs from 500 ms before the zero of the run, so a mark at T lies T + 500 ms into the scene.
    let scene = bursts(SEED, 2, RATE, SCENE_SLOTS);
    // The first start begins 300 ms before its mark; the second begins at its mark, with a first second from 400 ms later.
    let first = start_with_head_from(&scene, 1700, 1700);
    let second = start_with_head_from(&scene, 8500, 8900);
    let third = vec![0.0; samples_in(SHORT_START_MS, RATE)];
    decoded(&run, RATE, &[&first[..], &second, &third].concat());
    let reference = scene_recording(&home, &bursts(SEED, 1, RECORDER_RATE, SCENE_SLOTS));
    let mut measured = against(&home, &run, &reference, SCENE_BEFORE_ZERO_MS);
    let table = table_of(&measured.output().unwrap(), &run);

    for (key, placed_ms) in [
        ("start_1_head_offset_ms", 1700.0),
        ("start_1_body_offset_ms", 1700.0),
        ("start_2_head_offset_ms", 8900.0),
        ("start_2_body_offset_ms", 8500.0),
    ] {
        let (offset, note) = row(&table, key);
        assert!(
            close(offset.parse().unwrap(), placed_ms, 5.0),
            "{key}: {offset}"
        );
        assert!(
            note.contains("score 0.9") || note.contains("score 1.0"),
            "{note}"
        );
    }
    assert_eq!(row(&table, "start_1_head_body_agree").0, "yes");
    assert_eq!(row(&table, "start_2_head_body_agree").0, "no");
    let (second, rest) = (
        samples_in(HEAD_MS, RATE),
        samples_in(HEAD_AND_BODY_MS, RATE),
    );
    let planted = rms_dbfs(&first[..second]) - rms_dbfs(&first[second..rest]);
    assert!(
        close(number(&table, "start_1_head_minus_body_db"), planted, 0.01),
        "{table}"
    );
    for (key, lead_ms) in [
        ("start_1_acoustic_lead_ms", 300.0),
        ("start_2_acoustic_lead_ms", 0.0),
    ] {
        let (lead, note) = row(&table, key);
        assert!(close(lead.parse().unwrap(), lead_ms, 5.0), "{key}: {lead}");
        assert!(note.ends_with("recorder clock"), "{note}");
    }
    for name in START_ROWS {
        let (value, why) = row(&table, &format!("start_3_{name}"));
        assert_eq!(value, "none");
        assert!(
            why.contains("under the 4000 ms of a head and a body"),
            "{why}"
        );
    }
}

/// The rows a reference gives each start.
const START_ROWS: [&str; 5] = [
    "head_offset_ms",
    "body_offset_ms",
    "head_body_agree",
    "head_minus_body_db",
    "acoustic_lead_ms",
];

/// The table of three starts against a recorder that `heard` the scene from `before_zero_ms` before the zero of the run: zeros at 1.5 s, a first second from 400 ms before its place at 7.2 s, and the scene where its mark puts it at 12.5 s.
fn three_starts_against(heard: &[f32], before_zero_ms: u32) -> String {
    let home = tempfile::tempdir().unwrap();
    let scene = bursts(SEED, 2, RATE, SCENE_SLOTS);
    let at_ms = |mark_ms: u32| mark_ms.saturating_add(before_zero_ms);
    let segments = [
        vec![0.0; samples_in(START_MS, RATE)],
        start_with_head_from(&scene, at_ms(7200), at_ms(6800)),
        start_with_head_from(&scene, at_ms(12_500), at_ms(12_500)),
    ];
    let run = firstword_run(&home, &[1500, 7200, 12_500], &segments);
    let reference = scene_recording(&home, heard);
    let mut measured = against(&home, &run, &reference, before_zero_ms);
    table_of(&measured.output().unwrap(), &run)
}

#[test]
fn a_start_is_placed_only_where_the_reference_holds_all_that_is_searched() {
    // With the recorder 500 ms ahead, the search of the first start begins at its first sample and that of the third ends at its last, 19 s in.
    let heard = bursts(SEED, 1, RECORDER_RATE, SEARCHED_SLOTS);
    let table = three_starts_against(&heard, SCENE_BEFORE_ZERO_MS);
    // A start of zeros is searched and matches nowhere, so nothing says whether its parts agree or where it began.
    let no_head = "the reference holds no match of the first second within 2000 ms of where the start mark puts it";
    let no_body = "the reference holds no match of the three seconds after the first within 2000 ms of where the start mark puts it";
    let silent = "the first second or the three after it are digital silence";
    let reasons = [no_head, no_body, no_head, silent, no_body];
    for (name, why) in START_ROWS.into_iter().zip(reasons) {
        let found = row(&table, &format!("start_1_{name}"));
        assert_eq!(found, ("none", why), "{name}");
    }
    // A first second from 400 ms before its place disagrees as one from after it does.
    assert_eq!(row(&table, "start_2_head_offset_ms").0, "7300");
    assert_eq!(row(&table, "start_2_body_offset_ms").0, "7700");
    assert_eq!(row(&table, "start_2_head_body_agree").0, "no");
    assert_eq!(row(&table, "start_3_head_offset_ms").0, "13000");
    assert_eq!(row(&table, "start_3_body_offset_ms").0, "13000");
    assert_eq!(row(&table, "start_3_head_body_agree").0, "yes");

    // One sample less of the recording, and the search of the third start runs past its end.
    let (_, shorter) = heard.split_last().unwrap();
    let table = three_starts_against(shorter, SCENE_BEFORE_ZERO_MS);
    assert_eq!(row(&table, "start_2_body_offset_ms").0, "7700");
    for name in START_ROWS {
        let found = row(&table, &format!("start_3_{name}"));
        assert_eq!(found, ("none", SEARCH_NOT_HELD), "{name}");
    }

    // A recorder one millisecond less ahead, and the search of the first start begins before its first sample.
    let table = three_starts_against(&heard, SCENE_BEFORE_ZERO_MS.saturating_sub(1));
    for name in START_ROWS {
        let found = row(&table, &format!("start_1_{name}"));
        assert_eq!(found, ("none", SEARCH_NOT_HELD), "{name}");
    }
    assert_eq!(row(&table, "start_3_body_offset_ms").0, "12999");
}

#[test]
fn a_recorder_started_after_the_run_places_a_start_from_its_first_sample() {
    let home = tempfile::tempdir().unwrap();
    // The recorder starts a second into the run, so a mark at T lies T - 1000 ms into the scene.
    let scene = bursts(SEED, 2, RATE, SCENE_SLOTS);
    let segments = [
        start_with_head_from(&scene, 1000, 1000),
        start_with_head_from(&scene, 7000, 7000),
    ];
    let run = firstword_run(&home, &[2000, 8000], &segments);
    let reference = scene_recording(&home, &bursts(SEED, 1, RECORDER_RATE, SCENE_SLOTS));
    let a_second_late = REALTIME_AT_ZERO_NS.saturating_add(NS_PER_MS.saturating_mul(1000));
    let mut measured = against_at(&home, &run, &reference, a_second_late);
    let table = table_of(&measured.output().unwrap(), &run);

    // The search of the first start would begin a second before the first sample of the recorder.
    for name in START_ROWS {
        let found = row(&table, &format!("start_1_{name}"));
        assert_eq!(found, ("none", SEARCH_NOT_HELD), "{name}");
    }
    assert_eq!(row(&table, "start_2_head_offset_ms").0, "7000");
    assert_eq!(row(&table, "start_2_body_offset_ms").0, "7000");
    assert_eq!(row(&table, "start_2_acoustic_lead_ms").0, "0");
}

#[test]
fn a_run_with_no_wall_clock_takes_no_reference() {
    let home = tempfile::tempdir().unwrap();
    let (run, frames) = tap_run(&home, &CLICKS_MS, &CLICKS_MS);
    // The meta file of a run whose clock was never tied to the wall clock, as a trace export writes it.
    fs::write(run.join("meta.txt"), "script=voice\n").unwrap();
    put_file(
        home.path(),
        "recorder.wav",
        &file(INTEGER, 2, RECORDER_RATE, 16, &frames),
    );
    let reference = home.path().join("recorder.wav");
    let said = refusal(
        &against(&home, &run, &reference, BEFORE_ZERO_MS)
            .output()
            .unwrap(),
        &run,
    );
    assert!(
        said.contains("meta.txt has no realtime_at_zero_ns line"),
        "{said}"
    );
}
