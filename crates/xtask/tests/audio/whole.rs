//! `probe audio --whole`: one decoded file measured as one window with no cue read, and the dictation audio session by session.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use super::super::dictation::{DICTATION, FRAME, near_zero_percent};
use super::super::oracles::opus_oracles_installed;
use super::super::signal::{BIN_HZ, ROOM, SEED, add, band_noise, close, noise, rms_dbfs, span};
use super::super::wav::chunk;
use super::{
    INTEGER, file, float_wav, names_in, number, probe, put_file, refusal, row, run_holding,
    samples_in, table_in,
};

/// The rate the recording of another microphone is converted to, and the rate the dictation audio is decoded at.
pub(super) const RATE: u32 = 48_000;
/// The rate the recording is generated at before FFmpeg converts it.
const SOURCE_RATE: u32 = 44_100;
/// Each sine of the generated voice, which sums to about -24 dBFS under an 8 kHz wall.
const VOICE: f32 = 0.01;
/// The generated recording: a voice over a room, then the room alone, then the voice again.
pub(super) const LENGTH_MS: u32 = 10_000;
const ROOM_FROM_MS: u32 = 5_500;
const ROOM_MS: u32 = 3_500;
/// One gate: a stretch the recorder wrote as exact zeros.
const GATE_MS: u32 = 300;
/// Where the five gates of the gated recording open, 15 percent of it in all.
const GATES_MS: [u32; 5] = [500, 1500, 2500, 3500, 4500];
/// The 20 ms frames of the voice the gates leave.
const VOICE_FRAMES: usize = 250;
/// The level of the room every generated recording lies over, which `ROOM` is the amplitude of, in dBFS.
const ROOM_DBFS: f64 = -70.0;
/// The frames of each of the two generated dictation sessions, four seconds, and those between them that lie in no session.
const SESSION_FRAMES: usize = 200;
const BETWEEN_FRAMES: usize = 10;
const TABLE: &str = "audio-whole.tsv";
const DICTATION_TABLE: &str = "audio-dictation.tsv";
/// What `probe audio` is asked of a run and the table it then writes: the uplink audio, and the dictation audio.
pub(super) const UPLINK: (&[&str], &str) = (&["--whole"], TABLE);
pub(super) const DICTATED: (&[&str], &str) = (&DICTATION, DICTATION_TABLE);

/// A voice walled at `wall_hz` over a room, a stretch of the room alone, and exact zeros in each of `gates_ms`.
fn recording(rate: u32, wall_hz: f32, gates_ms: &[u32]) -> Vec<f32> {
    let mut seed = SEED;
    let mut stream = noise(&mut seed, ROOM, samples_in(LENGTH_MS, rate));
    let again_ms = ROOM_FROM_MS + ROOM_MS;
    for (from_ms, len_ms) in [(0, ROOM_FROM_MS), (again_ms, LENGTH_MS - again_ms)] {
        let voice = band_noise(&mut seed, wall_hz, rate, VOICE, samples_in(len_ms, rate));
        add(&mut stream, samples_in(from_ms, rate), &voice);
    }
    for &gate_ms in gates_ms {
        stream[span(gate_ms, GATE_MS, rate)].fill(0.0);
    }
    stream
}

/// A run that holds nothing but `wav` under the name a decode of the uplink gives it.
pub(super) fn run_of(home: &TempDir, wav: &[u8]) -> PathBuf {
    run_holding(home, "mac", &[("pods.f32.wav", wav)])
}

pub(super) fn whole(home: &TempDir, run: &Path, (asked, table): (&[&str], &str)) -> String {
    let measured = probe(home, "audio", run).args(asked).output().unwrap();
    assert_eq!(String::from_utf8(measured.stderr.clone()).unwrap(), "");
    table_in(&measured, run, table)
}

// Sample output: the header, then `0\t0\t0\t1\t0\t0x80\t200\t1\t0xb8`: one line a frame, its session in the fourth field or nothing there.
fn frames_table(sessions: &[Option<u32>]) -> String {
    let header = "index\tpacket\tarrival_ns\tsession\tcounter\tflag\tlevel\tlen\ttoc\n".to_owned();
    let lines = sessions.iter().enumerate().map(|(index, session)| {
        let session = session.map_or_else(String::new, |session| session.to_string());
        format!("{index}\t{index}\t0\t{session}\t{index}\t0x80\t200\t1\t0xb8\n")
    });
    std::iter::once(header).chain(lines).collect()
}

/// A run that holds a frames table of `sessions` and `wav` as the dictation audio decoded from it.
pub(super) fn dictation_run(home: &TempDir, sessions: &[Option<u32>], wav: &[u8]) -> PathBuf {
    let table = frames_table(sessions);
    let files = [("frames.tsv", table.as_bytes()), ("dictation.f32.wav", wav)];
    run_holding(home, "dictation", &files)
}

#[test]
fn whole_reports_the_planted_band_edge_and_the_planted_gate() {
    let home = tempfile::tempdir().unwrap();
    let stream = recording(RATE, 8_000.0, &GATES_MS);
    let run = run_of(&home, &float_wav(RATE, &stream));
    let table = whole(&home, &run, UPLINK);
    assert_eq!(names_in(&run), [TABLE, "pods.f32.wav"]);

    assert_eq!(row(&table, "stream_rate").0, "48000");
    assert_eq!(row(&table, "stream_samples").0, "480000");
    let planted = near_zero_percent(&stream);
    assert!(planted > 16.0, "{planted}");
    assert!(
        close(number(&table, "whole_near_zero"), planted, 0.005),
        "{planted}\n{table}"
    );
    let voice = rms_dbfs(&stream[span(0, GATES_MS[0], RATE)]);
    assert!(
        close(number(&table, "whole_speech_rms"), voice, 0.3),
        "{voice}\n{table}"
    );
    let over = format!("the {VOICE_FRAMES} frames");
    assert!(row(&table, "whole_speech_rms").1.contains(&over), "{table}");
    assert!(
        close(number(&table, "whole_floor"), ROOM_DBFS, 0.3),
        "{table}"
    );
    for key in ["whole_band_edge", "whole_reach"] {
        assert!(
            close(number(&table, key), 8_000.0, f64::from(BIN_HZ)),
            "{table}"
        );
    }

    let again = probe(&home, "audio", &run).arg("--whole").output().unwrap();
    assert_eq!(again.status.code(), Some(1), "{again:?}");
    let said = String::from_utf8(again.stderr).unwrap();
    assert!(said.contains("audio-whole.tsv already exists"), "{said}");
    assert_eq!(std::fs::read_to_string(run.join(TABLE)).unwrap(), table);

    // The control: the same recording with another wall and no gate moves both figures.
    let home = tempfile::tempdir().unwrap();
    let stream = recording(RATE, 6_000.0, &[]);
    let table = whole(&home, &run_of(&home, &float_wav(RATE, &stream)), UPLINK);
    assert!(number(&table, "whole_near_zero") < 2.5, "{table}");
    assert!(
        close(
            number(&table, "whole_band_edge"),
            6_000.0,
            f64::from(BIN_HZ)
        ),
        "{table}"
    );
}

#[test]
fn whole_measures_a_recording_converted_as_another_microphone_is() {
    if !opus_oracles_installed() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let stream = recording(SOURCE_RATE, 8_000.0, &GATES_MS);
    let source = home.path().join("recorded.wav");
    put_file(
        home.path(),
        "recorded.wav",
        &float_wav(SOURCE_RATE, &stream),
    );
    let target = home.path().join("converted.wav");
    let rate = RATE.to_string();
    let converted = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(&source)
        .args(["-c:a", "pcm_f32le", "-ar", &rate, "-ac", "1"])
        .arg(&target)
        .output()
        .unwrap();
    assert!(converted.status.success(), "{converted:?}");

    let run = run_of(&home, &std::fs::read(&target).unwrap());
    let table = whole(&home, &run, UPLINK);
    assert_eq!(row(&table, "stream_rate").0, rate);
    assert!(
        close(number(&table, "stream_samples"), 480_000.0, 480.0),
        "{table}"
    );
    // The conversion filters the room as well as the voice, which moves the share near zero by a fraction of a point; the gates plant 15.
    assert!(
        close(
            number(&table, "whole_near_zero"),
            near_zero_percent(&stream),
            0.3
        ),
        "{table}"
    );
    assert!(
        close(number(&table, "whole_floor"), ROOM_DBFS, 0.3),
        "{table}"
    );
    for key in ["whole_band_edge", "whole_reach"] {
        assert!(
            close(number(&table, key), 8_000.0, f64::from(BIN_HZ)),
            "{table}"
        );
    }
}

#[test]
fn the_dictation_audio_is_measured_session_by_session() {
    let home = tempfile::tempdir().unwrap();
    let mut seed = SEED;
    let session = SESSION_FRAMES * FRAME;
    // The first session is a voice under an 8 kHz wall for 800 ms and then the room alone, which holds its quietest three seconds.
    let mut first = noise(&mut seed, ROOM, session);
    let voice = band_noise(&mut seed, 8_000.0, RATE, VOICE, samples_in(800, RATE));
    add(&mut first, 0, &voice);
    // The second is a voice under a 6 kHz wall from end to end, with one gate.
    let mut second = band_noise(&mut seed, 6_000.0, RATE, VOICE, session);
    second[span(1000, GATE_MS, RATE)].fill(0.0);
    let between = vec![0.0; BETWEEN_FRAMES * FRAME];
    let stream = [&first[..], &between, &second].concat();
    let sessions = [
        vec![Some(1); SESSION_FRAMES],
        vec![None; BETWEEN_FRAMES],
        vec![Some(2); SESSION_FRAMES],
    ]
    .concat();
    let run = dictation_run(&home, &sessions, &float_wav(RATE, &stream));

    let table = whole(&home, &run, DICTATED);
    assert_eq!(
        names_in(&run),
        [DICTATION_TABLE, "dictation.f32.wav", "frames.tsv"]
    );
    assert_eq!(row(&table, "stream_rate").0, "48000");
    assert_eq!(row(&table, "stream_samples").0, stream.len().to_string());
    assert!(
        close(
            number(&table, "whole_near_zero"),
            near_zero_percent(&stream),
            0.005
        ),
        "{table}"
    );
    for (key, samples, wall_hz) in [
        ("session_1", &first, 8_000.0),
        ("session_2", &second, 6_000.0),
    ] {
        assert_eq!(
            row(&table, &format!("{key}_samples")).0,
            session.to_string()
        );
        assert!(
            close(
                number(&table, &format!("{key}_near_zero")),
                near_zero_percent(samples),
                0.005
            ),
            "{table}"
        );
        assert!(
            close(
                number(&table, &format!("{key}_band_edge")),
                wall_hz,
                f64::from(BIN_HZ)
            ),
            "{table}"
        );
    }
    assert!(
        close(number(&table, "session_1_floor"), ROOM_DBFS, 0.3),
        "{table}"
    );
    assert!(
        close(
            number(&table, "session_1_reach"),
            8_000.0,
            f64::from(BIN_HZ)
        ),
        "{table}"
    );
    // The second session has no quiet stretch to lie over: its quietest three seconds are the voice itself.
    let (reach, why) = row(&table, "session_2_reach");
    assert_eq!(reach, "none");
    assert!(why.contains("at no grid point"), "{why}");
}

#[test]
fn a_file_the_whole_measure_cannot_read_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let run = run_holding(&home, "mac", &[]);
    let missing = refusal(
        &probe(&home, "audio", &run).arg("--whole").output().unwrap(),
        &run,
    );
    assert!(
        missing.contains("pods.f32.wav: No such file or directory"),
        "{missing}"
    );

    for (wav, sentence) in [
        (
            float_wav(4000, &[0.0; 480]),
            "xtask probe: pods.f32.wav is labelled 4000 Hz, and under 8000 Hz nothing is measured\n",
        ),
        (
            file(INTEGER, 1, RATE, 8, &[0; 480]),
            "xtask probe: pods.f32.wav was not read: its samples are 8 bit with format tag 1; what is read is 16 and 24 bit integer PCM (tag 1) and 32 bit float (tag 3)\n",
        ),
        (
            chunk(*b"data", &[0; 480]),
            "xtask probe: pods.f32.wav was not read: it does not open with the RIFF header of a WAVE file\n",
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        let run = run_of(&home, &wav);
        let refused = probe(&home, "audio", &run).arg("--whole").output().unwrap();
        assert_eq!(refusal(&refused, &run), sentence);
        assert_eq!(names_in(&run), ["pods.f32.wav"]);
    }
}

#[test]
fn a_choice_of_file_the_command_does_not_take_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let wav = float_wav(RATE, &[0.0; FRAME]);
    let run = dictation_run(&home, &[Some(1)], &wav);
    for (arguments, sentence) in [
        (
            &["--pcm", "sub/dictation.f32.wav", "--whole"][..],
            "error: invalid value 'sub/dictation.f32.wav' for '--pcm <PCM>'\n  [possible values: pods.f32.wav, dictation.f32.wav]",
        ),
        (
            &["--pcm", "/tmp/dictation.f32.wav", "--whole"],
            "error: invalid value '/tmp/dictation.f32.wav' for '--pcm <PCM>'\n  [possible values: pods.f32.wav, dictation.f32.wav]",
        ),
        (
            &["--pcm", "dictation.f32.wav"],
            "error: the following required arguments were not provided:\n  --whole",
        ),
        (
            &[
                "--whole",
                "--reference",
                "other.wav",
                "--reference-start-ns",
                "1",
            ],
            "error: the argument '--whole' cannot be used with '--reference <REFERENCE>'",
        ),
    ] {
        let refused = probe(&home, "audio", &run)
            .args(arguments)
            .output()
            .unwrap();
        let said = refusal(&refused, &run);
        assert!(said.starts_with(sentence), "{said}");
        assert_eq!(names_in(&run), ["dictation.f32.wav", "frames.tsv"]);
    }
}

#[test]
fn dictation_audio_the_frames_table_does_not_describe_is_refused() {
    let one = float_wav(RATE, &[0.0; FRAME]);
    let two = float_wav(RATE, &[0.0; 2 * FRAME]);
    let three = float_wav(RATE, &[0.0; 3 * FRAME]);
    for (sessions, wav, sentence) in [
        (
            &[Some(1)][..],
            float_wav(16_000, &[0.0; FRAME]),
            "xtask probe: dictation.f32.wav is labelled 16000 Hz, and decode-opus writes 48000 Hz\n",
        ),
        (
            &[Some(1), Some(1)],
            one.clone(),
            "xtask probe: dictation.f32.wav holds 960 samples and frames.tsv lists 2 frames of 960 samples, so the two are not of one decode\n",
        ),
        (
            &[],
            one,
            "xtask probe: dictation.f32.wav holds 960 samples and frames.tsv lists 0 frames of 960 samples, so the two are not of one decode\n",
        ),
        (
            &[Some(2), Some(1)],
            two,
            "xtask probe: frames.tsv does not list each session as one unbroken run of lines, in rising order: session 1 follows session 2\n",
        ),
        (
            &[Some(1), None, Some(1)],
            three,
            "xtask probe: frames.tsv does not list each session as one unbroken run of lines, in rising order: session 1 follows session 1\n",
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        let run = dictation_run(&home, sessions, &wav);
        let refused = probe(&home, "audio", &run)
            .args(DICTATION)
            .output()
            .unwrap();
        assert_eq!(refusal(&refused, &run), sentence);
        assert_eq!(names_in(&run), ["dictation.f32.wav", "frames.tsv"]);
    }

    let home = tempfile::tempdir().unwrap();
    let wav = float_wav(RATE, &[0.0; FRAME]);
    let run = run_holding(&home, "dictation", &[("dictation.f32.wav", &wav)]);
    let refused = probe(&home, "audio", &run)
        .args(DICTATION)
        .output()
        .unwrap();
    let said = refusal(&refused, &run);
    assert!(
        said.contains("frames.tsv: No such file or directory"),
        "{said}"
    );

    // A line of four fields, then a whole line whose fourth field is no number: each is refused by itself.
    for line in ["0\t0\t0\t1", "0\t0\t0\tone\t0\t0x80\t200\t1\t0xb8"] {
        let home = tempfile::tempdir().unwrap();
        let table = format!("header\n{line}\n");
        let files = [
            ("frames.tsv", table.as_bytes()),
            ("dictation.f32.wav", &wav),
        ];
        let run = run_holding(&home, "dictation", &files);
        let refused = probe(&home, "audio", &run)
            .args(DICTATION)
            .output()
            .unwrap();
        assert_eq!(
            refusal(&refused, &run),
            "xtask probe: line 2 of frames.tsv is not 9 fields with a session number or nothing in the fourth\n"
        );
    }
}
