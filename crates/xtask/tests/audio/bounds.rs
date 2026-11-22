//! `probe audio` at its bounds: a rise, a level, a match, an agreement and a tap that fall just under and just over each.

use tempfile::TempDir;

use super::super::signal::{
    OTHER_SEED, ROOM, SEED, add, bursts, close, noise, samples_in, sine, span,
};
use super::super::wav::{INTEGER, file};
use super::reference::{
    BEFORE_ZERO_MS, CLICKS_MS, HEAD_MS, RECORDER_RATE, SCENE_BEFORE_ZERO_MS, START_MS, against,
    firstword_run, start_with_head_from, tap_run,
};
use super::{
    COUNT, QUIETLY, RATE, TAP, analysed, decoded, float_wav, marks, number, probe, put_file, row,
    table_of,
};

/// A tone far above the 2 kHz a tap is read over, eight samples a turn at the rate of the stream.
const TONE_HZ: f32 = 8000.0;
/// The steady size of the tone, 60 dB under full scale.
const BED: f32 = 0.001;
/// What the tone grows by: 19 dB and 21 dB, a decibel to either side of the rise that makes a tap.
const UNDER_A_TAP: f32 = 8.913;
const OVER_A_TAP: f32 = 11.220;
/// The sizes of a sine a tenth of a decibel to either side of an RMS of -60 dBFS, at which a frame is audible.
const UNDER_AUDIBLE: f32 = 0.001_40;
const OVER_AUDIBLE: f32 = 0.001_43;
/// The slots of a burst train of 30 seconds.
const SCENE_SLOTS: usize = 1500;
/// The three seconds after the first inside a start.
const BODY_MS: u32 = 3000;
/// The thousandths of a first second and of the three after it that end in another scene, a hundredth of each part to either side of a match of 0.5.
const JUST_MATCHING: [u32; 2] = [475, 455];
const JUST_NOT_MATCHING: [u32; 2] = [485, 465];

#[test]
fn a_rise_is_a_tap_only_over_the_bound() {
    let home = tempfile::tempdir().unwrap();
    let lines = [(0, "start", ""), (0, "cue", TAP), (3100, "stop", "")];
    let run = analysed(&home, "voice", &marks(&lines), &[(0, 3)]);
    let mut stream = sine(TONE_HZ, RATE, BED, 0.0, samples_in(3000, RATE));
    for (from_ms, grows_by) in [(1500, UNDER_A_TAP), (2200, OVER_A_TAP)] {
        let more = sine(
            TONE_HZ,
            RATE,
            BED * (grows_by - 1.0),
            0.0,
            samples_in(20, RATE),
        );
        add(&mut stream, samples_in(from_ms, RATE), &more);
    }
    decoded(&run, RATE, &stream);
    let table = table_of(&probe(&home, "audio", &run).output().unwrap(), &run);

    assert_eq!(row(&table, "cue_1_taps").0, "1");
    let (_, note) = row(&table, "cue_1_tap_1_peak");
    assert!(note.contains("from 2.200 s of the stream"), "{note}");
}

#[test]
fn speech_is_read_only_where_a_frame_is_audible() {
    let home = tempfile::tempdir().unwrap();
    let lines = [
        (0, "start", ""),
        (0, "cue", COUNT),
        (4000, "cue", QUIETLY),
        (8100, "stop", ""),
    ];
    let run = analysed(&home, "voice", &marks(&lines), &[(0, 8)]);
    // Four seconds of a tone no frame of which is audible, then four of one every frame of which is.
    let tone = |size| sine(300.0, RATE, size, 0.0, samples_in(4000, RATE));
    let stream = [tone(UNDER_AUDIBLE), tone(OVER_AUDIBLE)].concat();
    decoded(&run, RATE, &stream[..samples_in(7980, RATE)]);
    let table = table_of(&probe(&home, "audio", &run).output().unwrap(), &run);

    let unheard = "the stretch has no such level: none of its 20 ms frames reaches -60 dBFS";
    assert_eq!(row(&table, "cue_1_speech_rms"), ("none", unheard));
    assert_eq!(row(&table, "cue_1_speech_peak"), ("none", unheard));
    assert!(
        close(number(&table, "cue_2_speech_rms"), -59.90, 0.02),
        "{table}"
    );
    assert_eq!(row(&table, "start_1_first_audible").0, "4000.0");
}

#[test]
fn a_tap_is_its_own_only_nearer_than_half_the_gap_of_two() {
    let [first, second, third, last] = CLICKS_MS;
    // The recorder heard the second tap 49 ms and then 51 ms from where the others put it.
    for (off_ms, taps) in [(49, Some("4 taps")), (51, None)] {
        let home = tempfile::tempdir().unwrap();
        let heard = [first, second.saturating_add(off_ms), third, last];
        let (run, frames) = tap_run(&home, &CLICKS_MS, &heard);
        let reference = home.path().join("recorder.wav");
        put_file(
            home.path(),
            "recorder.wav",
            &file(INTEGER, 2, RECORDER_RATE, 16, &frames),
        );
        let mut measured = against(&home, &run, &reference, BEFORE_ZERO_MS);
        let table = table_of(&measured.output().unwrap(), &run);
        for key in ["tap_spacing_ratio", "tap_arrival_minus_reference_median"] {
            let (value, note) = row(&table, key);
            match taps {
                Some(taps) => assert!(value != "none" && note.contains(taps), "{off_ms}: {note}"),
                None => assert_eq!(
                    (value, note),
                    (
                        "none",
                        "the two do not hold the same taps: with the first and the last tap of each laid on each other, tap 2 lies 50 ms or more from its own"
                    )
                ),
            }
        }
    }
}

/// The table of a `firstword` run whose starts, five seconds each, hold `segments`, against a recorder that heard the whole scene.
fn against_the_scene(home: &TempDir, starts_ms: &[u32], segments: &[Vec<f32>]) -> String {
    let run = firstword_run(home, starts_ms, segments);
    let recorded = bursts(SEED, 1, RECORDER_RATE, SCENE_SLOTS);
    put_file(
        home.path(),
        "recorder.wav",
        &float_wav(RECORDER_RATE, &recorded),
    );
    let reference = home.path().join("recorder.wav");
    let mut measured = against(home, &run, &reference, SCENE_BEFORE_ZERO_MS);
    table_of(&measured.output().unwrap(), &run)
}

#[test]
fn head_and_body_agree_only_within_the_bound() {
    let home = tempfile::tempdir().unwrap();
    let scene = bursts(SEED, 2, RATE, SCENE_SLOTS);
    // The first second lies 20 ms and 21 ms after its place, then as far before it.
    let segments = [
        start_with_head_from(&scene, 2000, 2020),
        start_with_head_from(&scene, 8500, 8521),
        start_with_head_from(&scene, 15_000, 14_980),
        start_with_head_from(&scene, 21_500, 21_479),
    ];
    let table = against_the_scene(&home, &[1500, 8000, 14_500, 21_000], &segments);

    let placed = [
        ("2020", "2000", "yes"),
        ("8521", "8500", "no"),
        ("14980", "15000", "yes"),
        ("21479", "21500", "no"),
    ];
    for (start, (head_ms, body_ms, agree)) in (1..).zip(placed) {
        let found = |name: &str| row(&table, &format!("start_{start}_{name}")).0;
        assert_eq!(found("head_offset_ms"), head_ms, "start {start}");
        assert_eq!(found("body_offset_ms"), body_ms, "start {start}");
        assert_eq!(found("head_body_agree"), agree, "start {start}");
    }
}

/// A start of the scene from `from_ms` on, whose first second and whose three after it end in another scene for these thousandths of their length.
fn start_ending_elsewhere(scene: &[f32], other: &[f32], from_ms: u32, parts: [u32; 2]) -> Vec<f32> {
    let mut start = scene[span(from_ms, START_MS, RATE)].to_vec();
    let ends_ms = [HEAD_MS, HEAD_MS.saturating_add(BODY_MS)];
    for ((part_ms, ends_ms), thousandths) in [HEAD_MS, BODY_MS].into_iter().zip(ends_ms).zip(parts)
    {
        let elsewhere_ms = part_ms.saturating_mul(thousandths) / 1000;
        let elsewhere = span(ends_ms.saturating_sub(elsewhere_ms), elsewhere_ms, RATE);
        start[elsewhere.clone()].copy_from_slice(&other[elsewhere]);
    }
    start
}

#[test]
fn a_start_is_placed_only_by_a_match_over_the_bound() {
    let scene = bursts(SEED, 2, RATE, SCENE_SLOTS);
    let other = bursts(OTHER_SEED, 3, RATE, SCENE_SLOTS);
    let names = [
        "head_offset_ms",
        "body_offset_ms",
        "head_body_agree",
        "acoustic_lead_ms",
    ];
    let nowhere = |table: &str, start: u32| {
        for name in names {
            let (value, why) = row(table, &format!("start_{start}_{name}"));
            assert_eq!(value, "none", "start {start} {name}");
            assert!(
                why.starts_with("the reference holds no match of the ")
                    && why.contains(" within 2000 ms of "),
                "start {start} {name}: {why}"
            );
        }
    };

    // The pods held only the room, then the scene for as much of each part as leaves a match of 0.51.
    let home = tempfile::tempdir().unwrap();
    let mut seed = SEED;
    let segments = [
        noise(&mut seed, ROOM, samples_in(START_MS, RATE)),
        start_ending_elsewhere(&scene, &other, 13_500, JUST_MATCHING),
    ];
    let table = against_the_scene(&home, &[1500, 13_000], &segments);
    nowhere(&table, 1);
    for name in ["head_offset_ms", "body_offset_ms"] {
        let (offset, note) = row(&table, &format!("start_2_{name}"));
        assert_eq!(offset, "13500", "{name}");
        assert!(note.ends_with("score 0.51"), "{name}: {note}");
    }
    assert_eq!(row(&table, "start_2_head_body_agree").0, "yes");

    // A hundredth less of each part leaves a match of 0.49, and nothing is placed.
    let home = tempfile::tempdir().unwrap();
    let segments = [start_ending_elsewhere(
        &scene,
        &other,
        13_500,
        JUST_NOT_MATCHING,
    )];
    nowhere(&against_the_scene(&home, &[13_000], &segments), 1);
}
