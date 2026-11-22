//! `probe audio` on a run it cannot measure: what it refuses, and what it says in a row of a window too short or of zeros.

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use super::super::signal::{SEED, add, band_noise, close, sine};
use super::{
    COUNT, INTEGER, JINGLE, RATE, SDU_MS, SILENT, TAP, analysed, decoded, file, float_wav, marks,
    names_in, number, probe, put_file, refusal, row, samples_in, table_of,
};

/// A run of one second with one cue, analysed, with no decode in it yet.
fn short_run(home: &TempDir) -> PathBuf {
    let lines = [(0, "start", ""), (0, "cue", SILENT), (1100, "stop", "")];
    analysed(home, "voice", &marks(&lines), &[(0, 1)])
}

#[test]
fn pcm_name_with_a_slash_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let run = short_run(&home);
    decoded(&run, RATE, &vec![0.0; samples_in(990, RATE)]);
    let before = names_in(&run);
    for (name, sentence) in [
        ("../page/pods.f32.wav", "named without a directory"),
        ("sub/pods.f32.wav", "named without a directory"),
        ("other.f32.wav", "are pods.f32.wav and dictation.f32.wav"),
    ] {
        let refused = probe(&home, "audio", &run)
            .args(["--pcm", name])
            .output()
            .unwrap();
        let said = refusal(&refused, &run);
        assert!(said.contains(sentence), "{name}: {said}");
    }
    assert_eq!(names_in(&run), before);
    // The dictation stream is cut by a table no command writes yet, so it is refused with or without a file of that name in the run.
    let not_yet =
        "frames.tsv places the frames of dictation.f32.wav, and this command does not read it yet";
    for frames in [None, Some("frames.tsv")] {
        if let Some(name) = frames {
            put_file(&run, name, b"");
        }
        let held = names_in(&run);
        let mut dictation = probe(&home, "audio", &run);
        dictation.args(["--pcm", "dictation.f32.wav"]);
        let said = refusal(&dictation.output().unwrap(), &run);
        assert!(said.contains(not_yet), "{frames:?}: {said}");
        assert_eq!(names_in(&run), held, "{frames:?}");
    }
}

#[test]
fn a_decode_the_tables_do_not_describe_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let run = short_run(&home);
    let missing = refusal(&probe(&home, "audio", &run).output().unwrap(), &run);
    assert!(missing.contains("could not open"), "{missing}");
    assert!(
        missing.contains("pods.f32.wav: No such file or directory"),
        "{missing}"
    );

    for (wav, sentence) in [
        (
            float_wav(RATE, &[0.0; 480]),
            "holds 480 samples and units.tsv lists units for 63360",
        ),
        (
            file(INTEGER, 1, RATE, 8, &[0; 480]),
            "pods.f32.wav was not read: its samples are 8 bit",
        ),
        (float_wav(4000, &[0.0; 480]), "labelled 4000 Hz"),
    ] {
        let home = tempfile::tempdir().unwrap();
        let run = short_run(&home);
        put_file(&run, "pods.f32.wav", &wav);
        let said = refusal(&probe(&home, "audio", &run).output().unwrap(), &run);
        assert!(said.contains(sentence), "{said}");
    }

    let home = tempfile::tempdir().unwrap();
    let lines = [(0, "connect", ""), (1100, "end", "")];
    let run = analysed(&home, "listen", &marks(&lines), &[(0, 1)]);
    decoded(&run, RATE, &vec![0.0; samples_in(990, RATE)]);
    let said = refusal(&probe(&home, "audio", &run).output().unwrap(), &run);
    assert!(said.contains("no cue and no start mark"), "{said}");
}

#[test]
fn a_units_table_that_does_not_place_its_units_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let run = short_run(&home);
    decoded(&run, RATE, &vec![0.0; samples_in(990, RATE)]);
    let units = fs::read_to_string(run.join("units.tsv")).unwrap();
    // The second unit of the first SDU arrives with the first, 30 ms into the run.
    let second = "1\t2\t30000000\t";
    let line_3 =
        "units.tsv line 3 has no arrival in nanoseconds that is at or after the one before";
    for (table, sentence) in [
        (
            units.replacen("arrival_ns", "arrival", 1),
            "units.tsv names no arrival_ns column in its first line",
        ),
        (units.replacen(second, "1\t2\t29999999\t", 1), line_3),
        (units.replacen(second, "1\t2\tsoon\t", 1), line_3),
    ] {
        fs::write(run.join("units.tsv"), table).unwrap();
        let said = refusal(&probe(&home, "audio", &run).output().unwrap(), &run);
        assert!(said.contains(sentence), "{said}");
    }
}

#[test]
fn a_window_too_short_to_measure_says_so_in_its_rows() {
    let home = tempfile::tempdir().unwrap();
    // One SDU arrives in the tap window, the keys have no silence beside them, and the last cue comes after the stream.
    let lines = [
        (0, "start", ""),
        (0, "cue", TAP),
        (1040, "cue", JINGLE),
        (3050, "cue", "Put it back"),
        (3100, "stop", ""),
    ];
    let run = analysed(&home, "voice", &marks(&lines), &[(0, 3)]);
    decoded(&run, RATE, &vec![0.0; samples_in(3000, RATE)]);
    let table = table_of(&probe(&home, "audio", &run).output().unwrap(), &run);

    assert_eq!(
        row(&table, "cue_1_samples").0,
        samples_in(SDU_MS, RATE).to_string()
    );
    assert_eq!(row(&table, "cue_1_near_zero").0, "100.00");
    let (taps, why) = row(&table, "cue_1_taps");
    assert_eq!(taps, "none");
    assert!(
        why.contains("shorter than the 52 ms a rise is read over"),
        "{why}"
    );
    let reach = row(&table, "cue_2_reach");
    assert_eq!(
        reach,
        ("none", "the run has no silent window to compare with")
    );
    assert_eq!(row(&table, "cue_3_task").0, "other");
    assert_eq!(row(&table, "cue_3_samples").0, "0");
    assert_eq!(
        row(&table, "cue_3_near_zero"),
        ("none", "the window holds no sample")
    );
    let (audible, why) = row(&table, "start_1_first_audible");
    assert_eq!(audible, "none");
    assert_eq!(why, "none of its 20 ms frames reaches -60 dBFS");
}

#[test]
fn a_meta_file_with_a_bad_wall_clock_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let run = short_run(&home);
    decoded(&run, RATE, &vec![0.0; samples_in(990, RATE)]);
    let rule = "realtime_at_zero_ns must be one decimal number of nanoseconds, given once";
    for (meta, line) in [
        ("script=voice\nrealtime_at_zero_ns=01\n", 2),
        (
            "script=voice\nrealtime_at_zero_ns=1\nrealtime_at_zero_ns=2\n",
            3,
        ),
    ] {
        fs::write(run.join("meta.txt"), meta).unwrap();
        let said = refusal(&probe(&home, "audio", &run).output().unwrap(), &run);
        assert!(
            said.contains(&format!("meta.txt line {line}: {rule}")),
            "{said}"
        );
    }
}

#[test]
fn a_marks_file_cut_short_is_said_in_the_table() {
    let home = tempfile::tempdir().unwrap();
    // The capture was killed while it wrote the stop mark, so the line has no newline and is left out.
    let lines = [(0, "start", ""), (0, "cue", SILENT), (1100, "stop", "")];
    let cut = marks(&lines);
    let run = analysed(&home, "voice", cut.trim_end_matches('\n'), &[(0, 1)]);
    decoded(&run, RATE, &vec![0.0; samples_in(990, RATE)]);
    let table = table_of(&probe(&home, "audio", &run).output().unwrap(), &run);
    let (cut, note) = row(&table, "marks_cut_lines");
    assert_eq!(cut, "1");
    assert!(
        note.starts_with("last line of marks.tsv without its newline"),
        "{note}"
    );
}

#[test]
fn a_silent_window_of_zeros_gives_no_reach_and_no_floor() {
    let home = tempfile::tempdir().unwrap();
    let lines = [
        (0, "start", ""),
        (0, "cue", COUNT),
        (8000, "cue", JINGLE),
        (12_000, "cue", SILENT),
        (17_100, "stop", ""),
    ];
    let run = analysed(&home, "voice", &marks(&lines), &[(0, 17)]);
    // The stream is digital silence but for three seconds of speech, late in their window, and the keys.
    let mut seed = SEED;
    let mut stream = vec![0.0_f32; samples_in(16_980, RATE)];
    let voice = sine(300.0, RATE, 0.25, 0.0, samples_in(3000, RATE));
    add(&mut stream, samples_in(4500, RATE), &voice);
    let keys = band_noise(&mut seed, 10_000.0, RATE, 0.01, samples_in(2500, RATE));
    add(&mut stream, samples_in(9200, RATE), &keys);
    decoded(&run, RATE, &stream);
    let table = table_of(&probe(&home, "audio", &run).output().unwrap(), &run);

    // A sine of a quarter of full scale has an RMS of -15.05 dBFS, and the frame at each end of it is half silence.
    assert!(
        close(number(&table, "cue_1_speech_rms"), -15.05, 0.05),
        "{table}"
    );
    let no_floor = "the stretch has no such level: its quietest 3000 ms are digital silence";
    assert_eq!(row(&table, "cue_1_floor"), ("none", no_floor));
    assert_eq!(row(&table, "cue_3_noise_rms"), ("none", no_floor));
    assert_eq!(row(&table, "cue_3_exact_zeros").0, "yes");
    let nothing_under =
        "the silent window holds nothing at 250 Hz to lie over, as digital silence does";
    assert_eq!(row(&table, "cue_2_reach"), ("none", nothing_under));
}
