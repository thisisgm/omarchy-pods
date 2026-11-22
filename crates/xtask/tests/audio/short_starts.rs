//! `probe analyse`, `probe decode` and `probe audio` in a row on runs whose starts are too short for the slope of the clock.

use std::fs;
use std::path::{Path, PathBuf};

use pods_proto::sdulog::Record;
use tempfile::TempDir;

use super::super::backlog::with_backlog;
use super::super::dictation::SAMPLE_BYTES;
use super::super::fixture::{TONE_UNIT, ffprobe, installed};
use super::super::signal::{SEED, close, samples_in};
use super::super::synth::{SILENT_UNIT, synth};
use super::super::units::with_units;
use super::super::xorshift::next_random;
use super::reference::{SCENE_BEFORE_ZERO_MS, against};
use super::{
    COUNT_ON, MS_PER_SECOND, NS_PER_MS, RATE, SAMPLES_PER_UNIT, UNITS_PER_SDU, analysed_sending,
    float_wav, marks, names_in, number, probe, put_file, refusal, row, rows, table_of,
    uplink_lines,
};

/// The uplink of `trials`, `early` and `firstword` runs this long after each start.
const CYCLE_S: u32 = 5;
/// The starts of `firstword`.
const FIRSTWORD_STARTS_MS: [u32; 3] = [8_000, 23_000, 38_000];
/// The first sample of a cycle is this much older than its start.
const LEAD_MS: u32 = 900;
/// The backlog of a cycle comes one SDU every this long, five times as fast as it was recorded.
const BURST_PACE_MS: u32 = 6;
/// Each arrival is late by a seeded delay under this.
const JITTER_MS: u32 = 2;
/// A recorder runs this long past the last start mark: the search of a head and a body ends 6 s after it.
const RECORDER_TAIL_MS: u32 = 7_000;
/// The cycles of `trials`: sixteen that start 15 s apart, then four after a minute of idle each.
const TRIALS_SHORT_CYCLES: u32 = 16;
const TRIALS_SHORT_PERIOD_MS: u32 = 15_000;
const TRIALS_LONG_STARTS_MS: [u32; 4] = [290_000, 355_000, 420_000, 485_000];
/// The second start of `early`, which follows the start sent before the handshake.
const EARLY_SECOND_START_MS: u32 = 8_000;
/// A start long enough for the slope of the clock, and a cycle that begins after its stop.
const LONG_START_S: u32 = 31;
const AFTER_LONG_START_MS: u32 = 40_000;
/// A second cycle that begins after the stop of a first.
const AFTER_FIRST_CYCLE_MS: u32 = 15_000;
/// The rate the configuration declares, and a pace within five percent of neither rate.
const DECLARED_RATE: u32 = 48_000;
const NEITHER_RATE: u32 = 56_000;
/// A peer whose first sample is no older than its start, so its SDUs come as they end.
const STEADY_PEER: Peer = Peer {
    rate_hz: RATE,
    lead_ms: 0,
    pace_ms: BURST_PACE_MS,
};
/// A backlog of 2.4 s that comes an SDU every 20 ms, twice as fast as 48 kHz records one: it lasts 2.36 s.
const LONG_LEAD_MS: u32 = 2400;
const SLOW_PACE_MS: u32 = 20;
/// A start stopped a second before a cycle ends, which leaves under three seconds after the opening.
const CUT_START_S: u32 = 4;
/// A clock a quarter of 64 kHz, at which an SDU of four units lasts 120 ms.
const SPARSE_RATE: u32 = 16_000;
/// How near a coarse clock of a generated start comes to the rate it was generated at.
const COARSE_WITHIN_HZ: f64 = 640.0;
/// The silent units a start opens with: 120 ms at 64 kHz and 160 ms at 48 kHz, each a whole number of the 20 ms frames the first audible one is sought in.
const QUIET_UNITS: u32 = 16;
/// How near a start is placed, as the tests of the reference place one.
const PLACED_WITHIN_MS: f64 = 5.0;

/// How a generated peer sends one uplink run.
#[derive(Debug, Clone, Copy)]
struct Peer {
    rate_hz: u32,
    /// How much older than the start the first sample is: the backlog the run opens with.
    lead_ms: u32,
    /// The backlog comes one SDU every this long.
    pace_ms: u32,
}

/// One start mark, the stop mark `seconds` after it and what the pods sent between them.
#[derive(Debug, Clone, Copy)]
struct Start {
    at_ms: u32,
    seconds: u32,
    /// `None` for a start the pods never answered.
    peer: Option<Peer>,
}

impl Start {
    /// One cycle of a script from a peer clocked at `rate_hz`, which opens with its backlog in a burst.
    const fn cycle(at_ms: u32, rate_hz: u32) -> Self {
        let peer = Peer {
            rate_hz,
            lead_ms: LEAD_MS,
            pace_ms: BURST_PACE_MS,
        };
        Self::of(at_ms, CYCLE_S, peer)
    }

    const fn of(at_ms: u32, seconds: u32, peer: Peer) -> Self {
        Self {
            at_ms,
            seconds,
            peer: Some(peer),
        }
    }

    const fn unanswered(at_ms: u32) -> Self {
        Self {
            at_ms,
            seconds: CYCLE_S,
            peer: None,
        }
    }

    /// The SDUs of the start: its backlog first, an SDU every `pace_ms`, then each as its last unit ends, and none after the stop.
    fn sent(&self) -> Vec<Record> {
        let Some(peer) = self.peer else {
            return Vec::new();
        };
        let ns = |ms: u32| u64::from(ms) * NS_PER_MS;
        let recorded_s = self
            .seconds
            .saturating_add(peer.lead_ms.div_ceil(MS_PER_SECOND));
        let units = (SAMPLES_PER_UNIT, UNITS_PER_SDU);
        let steady = synth(peer.rate_hz, units.0, units.1, recorded_s, JITTER_MS, SEED);
        let stop_ns = ns(self.seconds.saturating_mul(MS_PER_SECOND));
        let mut sent = with_backlog(steady, ns(peer.lead_ms), ns(peer.pace_ms), stop_ns);
        for record in &mut sent {
            record.at_ns = record.at_ns.saturating_add(ns(self.at_ms));
        }
        sent
    }
}

/// A run of `starts` under `script`, analysed; with it, the SDUs each start delivered. A start opens with `QUIET_UNITS` of silence and one unit of tone, is tone or silence by a seeded draw after, and ends with a silent SDU so that none of its tone laps into the next start.
fn analysed_starts(
    home: &TempDir,
    script: &str,
    cue: Option<&'static str>,
    starts: &[Start],
) -> (PathBuf, Vec<usize>) {
    let cycles: Vec<(u32, u32)> = starts
        .iter()
        .map(|start| (start.at_ms, start.seconds))
        .collect();
    let cue: Vec<_> = cue.map(|text| (0, "cue", text)).into_iter().collect();
    let lines = [cue, uplink_lines(&cycles)].concat();
    let mut random = SEED;
    let mut sent_by = |start: &Start| {
        let silent = start.sent();
        let sdus_before_the_last = u32::try_from(silent.len().saturating_sub(1)).unwrap();
        let last_sdu_from = sdus_before_the_last.saturating_mul(UNITS_PER_SDU);
        let mut unit = 0_u32;
        let drawn = || {
            let drawn_tone = next_random(&mut random).is_multiple_of(2);
            let tone = unit == QUIET_UNITS || (unit > QUIET_UNITS && drawn_tone);
            let tone = tone && unit < last_sdu_from;
            unit = unit.saturating_add(1);
            if tone {
                &TONE_UNIT[..]
            } else {
                &SILENT_UNIT[..]
            }
        };
        with_units(&silent, drawn)
    };
    let sent: Vec<Vec<Record>> = starts.iter().map(&mut sent_by).collect();
    let run = analysed_sending(home, script, &marks(&lines), sent.concat());
    (run, sent.iter().map(Vec::len).collect())
}

/// The table `probe decode` prints for `run`, which it also leaves in the run.
fn decoded_table(home: &TempDir, run: &Path) -> String {
    let decoded = probe(home, "decode", run).output().unwrap();
    assert!(decoded.status.success(), "{decoded:?}");
    let table = String::from_utf8(decoded.stdout).unwrap();
    assert_eq!(fs::read_to_string(run.join("decode.tsv")).unwrap(), table);
    table
}

/// The rate `pods.f32.wav` of `run` is labelled with, as ffprobe reads it.
fn labelled(run: &Path) -> String {
    ffprobe(&run.join("pods.f32.wav"), "stream=sample_rate")
}

/// When a recorder that began `SCENE_BEFORE_ZERO_MS` before the zero of the run heard the first sample of a cycle.
fn first_sample_ms(start: &Start) -> u32 {
    start
        .at_ms
        .saturating_add(SCENE_BEFORE_ZERO_MS)
        .saturating_sub(LEAD_MS)
}

/// What a recorder beside the pods held of a run clocked at `RATE`: the oracle's decode of each start, laid where its first sample was heard, from `SCENE_BEFORE_ZERO_MS` before the zero of the run.
fn recorder(run: &Path, starts: &[Start], sdus: &[usize]) -> Vec<f32> {
    let decoded = fs::read(run.join("ffmpeg.f32")).unwrap();
    let (samples, cut) = decoded.as_chunks::<SAMPLE_BYTES>();
    assert_eq!(cut, b"");
    let mut decoded = samples.iter().map(|&sample| f32::from_le_bytes(sample));
    let last = starts.last().unwrap();
    let ends_ms = first_sample_ms(last).saturating_add(LEAD_MS + RECORDER_TAIL_MS);
    let mut heard = vec![0.0; samples_in(ends_ms, RATE)];
    let samples_per_sdu = usize::try_from(UNITS_PER_SDU * SAMPLES_PER_UNIT).unwrap();
    for (start, sdus) in starts.iter().zip(sdus) {
        let from = samples_in(first_sample_ms(start), RATE);
        for slot in heard
            .iter_mut()
            .skip(from)
            .take(sdus.saturating_mul(samples_per_sdu))
        {
            *slot = decoded.next().unwrap();
        }
    }
    assert_eq!(decoded.next(), None);
    heard
}

/// The values of the rows `key` has in the analysis table of `run`, one for each start.
fn analysed_values(run: &Path, key: &str) -> Vec<String> {
    let analysis = fs::read_to_string(run.join("analysis.tsv")).unwrap();
    let found = rows(&analysis, key).into_iter();
    found.map(|(value, _)| value.to_owned()).collect()
}

#[test]
fn a_firstword_run_is_decoded_from_its_short_starts_and_its_heads_are_measured() {
    if !(installed("ffmpeg") && installed("ffprobe")) {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let starts = FIRSTWORD_STARTS_MS.map(|at_ms| Start::cycle(at_ms, RATE));
    let (run, sdus) = analysed_starts(&home, "firstword", Some(COUNT_ON), &starts);

    let decoded = decoded_table(&home, &run);
    assert_eq!(analysed_values(&run, "clock_hz"), ["refused"; 3]);
    let analysis = fs::read_to_string(run.join("analysis.tsv")).unwrap();
    let coarse = rows(&analysis, "clock_hz_coarse");
    assert_eq!(coarse.len(), 3, "{analysis}");
    for (hz, note) in coarse {
        assert!(
            close(hz.parse().unwrap(), f64::from(RATE), COARSE_WITHIN_HZ),
            "{hz}"
        );
        assert!(note.contains("the pace of the arrivals"), "{note}");
    }
    assert_eq!(row(&decoded, "rate_label").0, RATE.to_string());
    assert_eq!(row(&decoded, "rate_label_from_clock_hz").0, "0");
    assert_eq!(row(&decoded, "rate_label_from_clock_hz_coarse").0, "3");
    assert_eq!(row(&decoded, "rate_label_starts_without_audio").0, "0");
    assert_eq!(labelled(&run), format!("sample_rate={RATE}\n"));

    put_file(
        home.path(),
        "recorder.wav",
        &float_wav(RATE, &recorder(&run, &starts, &sdus)),
    );
    let reference = home.path().join("recorder.wav");
    let mut measured = against(&home, &run, &reference, SCENE_BEFORE_ZERO_MS);
    let table = table_of(&measured.output().unwrap(), &run);
    assert_eq!(row(&table, "stream_rate").0, RATE.to_string());
    for (ordinal, start) in (1..).zip(starts) {
        let first_ms = f64::from(first_sample_ms(&start));
        for part in ["head", "body"] {
            let placed = number(&table, &format!("start_{ordinal}_{part}_offset_ms"));
            assert!(close(placed, first_ms, PLACED_WITHIN_MS), "{table}");
        }
        assert_eq!(
            row(&table, &format!("start_{ordinal}_head_body_agree")).0,
            "yes"
        );
        assert!(number(&table, &format!("start_{ordinal}_head_minus_body_db")).is_finite());
        let lead = number(&table, &format!("start_{ordinal}_acoustic_lead_ms"));
        assert!(close(lead, f64::from(LEAD_MS), PLACED_WITHIN_MS), "{table}");
    }
}

/// The label rows of a decode: the rate, then the starts labelled by their slope, by their coarse clock, and those without audio.
fn label_rows(decoded: &str) -> [String; 4] {
    [
        "rate_label",
        "rate_label_from_clock_hz",
        "rate_label_from_clock_hz_coarse",
        "rate_label_starts_without_audio",
    ]
    .map(|key| row(decoded, key).0.to_owned())
}

#[test]
fn the_scripts_with_short_starts_are_labelled_and_measured() {
    if !(installed("ffmpeg") && installed("ffprobe")) {
        return;
    }
    let trials_starts_ms = (0..TRIALS_SHORT_CYCLES)
        .map(|cycle| cycle * TRIALS_SHORT_PERIOD_MS)
        .chain(TRIALS_LONG_STARTS_MS);
    let trials: Vec<Start> = trials_starts_ms
        .map(|at_ms| Start::cycle(at_ms, RATE))
        .collect();
    let slow_firstword = FIRSTWORD_STARTS_MS.map(|at_ms| Start::cycle(at_ms, DECLARED_RATE));
    let early_unanswered = [
        Start::unanswered(0),
        Start::cycle(EARLY_SECOND_START_MS, RATE),
    ];
    let early_answered = [
        Start::cycle(0, RATE),
        Start::cycle(EARLY_SECOND_START_MS, RATE),
    ];
    let long_then_short = [
        Start::of(0, LONG_START_S, STEADY_PEER),
        Start::cycle(AFTER_LONG_START_MS, RATE),
    ];
    for (script, starts, labelled_as) in [
        ("trials", &trials[..], ["64000", "0", "20", "0"]),
        ("firstword", &slow_firstword, ["48000", "0", "3", "0"]),
        ("early", &early_unanswered, ["64000", "0", "1", "1"]),
        ("early", &early_answered, ["64000", "0", "2", "0"]),
        ("soak", &long_then_short, ["64000", "1", "1", "0"]),
    ] {
        let home = tempfile::tempdir().unwrap();
        let (run, _) = analysed_starts(&home, script, None, starts);
        let decoded = decoded_table(&home, &run);
        assert_eq!(label_rows(&decoded), labelled_as, "{script}");
        let rate = labelled_as[0];
        assert_eq!(labelled(&run), format!("sample_rate={rate}\n"), "{script}");

        let table = table_of(&probe(&home, "audio", &run).output().unwrap(), &run);
        assert_eq!(row(&table, "stream_rate").0, rate, "{script}");
        // The quiet opening lasts as long as the rate of the label says, so a wrong label moves the figure.
        let quiet_samples = f64::from(QUIET_UNITS * SAMPLES_PER_UNIT);
        let quiet_ms = quiet_samples * f64::from(MS_PER_SECOND) / rate.parse::<f64>().unwrap();
        for (ordinal, start) in (1..).zip(starts) {
            let heard = row(&table, &format!("start_{ordinal}_first_audible")).0;
            let after_the_quiet = start.peer.map(|_| format!("{quiet_ms:.1}"));
            let expected = after_the_quiet.as_deref().unwrap_or("none");
            assert_eq!(heard, expected, "{script}, start {ordinal}");
        }
    }
}

/// What `probe decode` says when it refuses `run`, having added no file to it.
fn decode_refusal(home: &TempDir, run: &Path) -> String {
    let before = names_in(run);
    let refused = probe(home, "decode", run).output().unwrap();
    let said = refusal(&refused, run);
    assert_eq!(names_in(run), before);
    said
}

#[test]
fn a_start_whose_pace_is_neither_rate_keeps_its_figure_and_is_not_labelled() {
    let home = tempfile::tempdir().unwrap();
    let starts = [Start::cycle(0, NEITHER_RATE)];
    let (run, _) = analysed_starts(&home, "trials", None, &starts);
    let coarse = &analysed_values(&run, "clock_hz_coarse")[0];
    assert!(
        close(
            coarse.parse().unwrap(),
            f64::from(NEITHER_RATE),
            COARSE_WITHIN_HZ
        ),
        "{coarse}"
    );
    let said = decode_refusal(&home, &run);
    let neither = format!(
        "xtask probe: clock_hz_coarse {coarse} is within 5 percent of neither {RATE} Hz nor {DECLARED_RATE} Hz"
    );
    assert!(said.starts_with(&neither), "{said}");
}

#[test]
fn a_start_with_no_steady_span_is_refused_with_its_reason_and_refuses_the_run() {
    let draining = Peer {
        rate_hz: DECLARED_RATE,
        lead_ms: LONG_LEAD_MS,
        pace_ms: SLOW_PACE_MS,
    };
    let few_sdus = Peer {
        rate_hz: SPARSE_RATE,
        ..STEADY_PEER
    };
    for (name, start, why) in [
        (
            "a backlog still draining a third of the way into the span, which as a whole reads 64.5 kHz",
            Start::of(0, CYCLE_S, draining),
            "percent apart, of 10 allowed: the delay of the arrivals changes inside it",
        ),
        (
            "a start stopped after four seconds, which leaves under three after the opening",
            Start::of(0, CUT_START_S, STEADY_PEER),
            " audio SDUs over 2.9",
        ),
        (
            "a stream of a quarter of the SDUs, which leaves 32 after the opening",
            Start::of(0, CYCLE_S, few_sdus),
            "; 32 audio SDUs over 3.720 s; needs 75 over 3 s",
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        // A whole cycle after it shows that one start with no figure refuses a run another start could label.
        let starts = [start, Start::cycle(AFTER_FIRST_CYCLE_MS, RATE)];
        let (run, _) = analysed_starts(&home, "trials", None, &starts);
        let analysis = fs::read_to_string(run.join("analysis.tsv")).unwrap();
        let [refused, labelled] = rows(&analysis, "clock_hz_coarse")[..] else {
            panic!("{analysis}")
        };
        assert_eq!(refused.0, "refused", "{name}");
        assert!(refused.1.contains(why), "{name}: {}", refused.1);
        assert!(refused.1.starts_with("start 1; "), "{name}: {}", refused.1);
        assert!(
            close(
                labelled.0.parse().unwrap(),
                f64::from(RATE),
                COARSE_WITHIN_HZ
            ),
            "{name}"
        );
        let said = decode_refusal(&home, &run);
        let not_measured = "xtask probe: start 1 of analysis.tsv holds audio and gives neither clock_hz nor clock_hz_coarse as a number";
        assert!(said.starts_with(not_measured), "{name}: {said}");
    }
}

#[test]
fn a_run_without_one_rate_for_all_its_audio_is_refused() {
    for (starts, why) in [
        (
            [
                Start::cycle(0, RATE),
                Start::cycle(AFTER_FIRST_CYCLE_MS, DECLARED_RATE),
            ],
            "xtask probe: the clock rows of analysis.tsv are not all near one rate",
        ),
        (
            [
                Start::unanswered(0),
                Start::unanswered(AFTER_FIRST_CYCLE_MS),
            ],
            "xtask probe: analysis.tsv gives the clock of no start",
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        let (run, _) = analysed_starts(&home, "trials", None, &starts);
        let said = decode_refusal(&home, &run);
        assert!(said.starts_with(why), "{said}");
    }
}

#[test]
fn a_coarse_clock_at_the_very_edge_of_a_band_is_labelled() {
    if !(installed("ffmpeg") && installed("ffprobe")) {
        return;
    }
    // Five percent under and over each rate, as a hand would write them into the table.
    for (edge, rate) in [
        ("60800", "64000"),
        ("67200", "64000"),
        ("45600", "48000"),
        ("50400", "48000"),
    ] {
        let home = tempfile::tempdir().unwrap();
        let (run, _) = analysed_starts(&home, "trials", None, &[Start::cycle(0, RATE)]);
        let analysis = fs::read_to_string(run.join("analysis.tsv")).unwrap();
        let measured = &analysed_values(&run, "clock_hz_coarse")[0];
        let row_of = |hz: &str| format!("clock_hz_coarse\t{hz}\t");
        assert!(analysis.contains(&row_of(measured)), "{analysis}");
        let at_the_edge = analysis.replace(&row_of(measured), &row_of(edge));
        fs::write(run.join("analysis.tsv"), at_the_edge).unwrap();
        let decoded = decoded_table(&home, &run);
        assert_eq!(label_rows(&decoded), [rate, "0", "1", "0"], "{edge}");
        assert_eq!(labelled(&run), format!("sample_rate={rate}\n"), "{edge}");
    }
}

#[test]
fn an_analysis_whose_clock_rows_cannot_be_paired_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let starts = [
        Start::cycle(0, RATE),
        Start::cycle(AFTER_FIRST_CYCLE_MS, RATE),
    ];
    let (run, _) = analysed_starts(&home, "trials", None, &starts);
    let analysis = fs::read_to_string(run.join("analysis.tsv")).unwrap();
    // The coarse row of the first start is lost, so the one left would be read as the first start's.
    let mut lines = analysis.split_inclusive('\n');
    let lost = lines
        .position(|line| line.starts_with("clock_hz_coarse\t"))
        .unwrap();
    let kept = analysis.split_inclusive('\n').enumerate();
    let unpaired: String = kept
        .filter(|&(index, _)| index != lost)
        .map(|(_, line)| line)
        .collect();
    fs::write(run.join("analysis.tsv"), unpaired).unwrap();
    let said = decode_refusal(&home, &run);
    let cannot_pair = "xtask probe: analysis.tsv has 2 clock_hz rows and 1 clock_hz_coarse rows, so the two rows of a start cannot be paired";
    assert!(said.starts_with(cannot_pair), "{said}");
}
