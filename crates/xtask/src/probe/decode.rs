//! Decodes the units of one analysed run with the decoder candidate and with FFmpeg, compares the two, and runs the fixed experiments.

mod candidate;
mod compare;
mod experiments;
mod label;
mod oracle;
#[cfg(test)]
mod refused;

use std::fmt::Display;
use std::io;
use std::path::Path;

use pods_proto::sdulog::{Direction, Record};
use pods_proto::uplink::UplinkPacket;

use super::analyse::input::{InputError, LOG_FILE, parse_log, text_of, whole_lines};
use super::analyse::report::{Row, UNITS_KEY, table, values};
use super::analyse::{ANALYSIS_FILE, MAX_LOG_BYTES};
use super::store::{MAX_READ_BYTES, Run, StoreError, Unnamed};
use super::wav;
use candidate::{Candidate, Tally};
use compare::{
    CompareError, Comparison, LSB_PER_FULL_SCALE, SEARCH_UNITS, TOLERANCE_LSB, compare, lsb_apart,
};
use label::{Label, LabelError};
use oracle::{OracleError, OracleRun, STDERR_FILE, oracle};

/// The configuration the host supplies for the stream: AAC-ELD, mono, 480 sample frames, declared at 48 kHz.
pub(crate) const EXPECTED_CONFIG: [u8; 4] = [0xf8, 0xe6, 0x30, 0x00];
/// The samples one unit decodes to under that configuration.
pub(crate) const SAMPLES_PER_UNIT: u16 = 480;
/// The table of figures, which is also what the command prints.
const DECODE_FILE: &str = "decode.tsv";
/// What the candidate decoded, as a WAV file of floats.
pub(crate) const WAV_FILE: &str = "pods.f32.wav";
/// Twenty minutes of units at 133.33 a second: each decode of that many is 300 MiB of samples, and three are held at once.
const MAX_UNITS: u32 = 160_000;
const UNITS: &str = "units";
const SAMPLES: &str = "samples";
const FRAMES: &str = "frames";
const LSB: &str = "LSB";
const VERDICT: &str = "verdict";
/// What a row reads when its figure could not be taken; the note says why.
const REFUSED: &str = "refused";

/// Why a run was not decoded.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DecodeError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Input(#[from] InputError),
    #[error(transparent)]
    Oracle(#[from] OracleError),
    #[error(transparent)]
    Label(#[from] LabelError),
    #[error(
        "{ANALYSIS_FILE} does not count the {found} units that {LOG_FILE} holds, so the two are not of one run"
    )]
    UnitCount { found: usize },
    #[error("{LOG_FILE} holds no unit, so there is nothing to decode")]
    NoUnits,
    #[error("{LOG_FILE} holds {found} units, and one decode takes at most {MAX_UNITS}")]
    TooManyUnits { found: usize },
    #[error("could not write {WAV_FILE}: {0}")]
    Wav(io::Error),
}

/// What both decoders made of one stream of units.
struct Both<'run> {
    /// `Err` holds the candidate's reason for refusing the configuration.
    candidate: Result<Tally, String>,
    oracle: OracleRun<'run>,
    units: usize,
}

impl<'run> Both<'run> {
    fn of(
        run: &'run Run,
        config: [u8; 4],
        units: &[&[u8]],
        samples_per_unit: u16,
    ) -> Result<Self, OracleError> {
        let frame = usize::from(samples_per_unit);
        let decoded = |mut candidate| Tally::of(&mut candidate, units, frame);
        Ok(Self {
            oracle: oracle(run, config, units, samples_per_unit)?,
            candidate: Candidate::new(config).map(decoded),
            units: units.len(),
        })
    }

    /// The candidate's samples when every unit gave one frame, so sample k of unit n is at n x the frame length + k.
    fn whole_candidate(&self) -> Option<&[f32]> {
        let tally = self.candidate.as_ref().ok()?;
        (tally.decoded == self.units).then_some(tally.samples.as_slice())
    }

    fn whole_oracle(&self) -> Option<&[f32]> {
        self.oracle.decoded.as_deref().ok()
    }

    /// What each decoder did with the stream, under keys that start with `prefix`.
    fn rows(&self, prefix: &str) -> Vec<Row> {
        let key = |name: &str| format!("{prefix}{name}");
        let mut rows = Vec::new();
        match &self.candidate {
            Err(reason) => rows.push(Row::new(
                &key("candidate_config"),
                &REFUSED,
                VERDICT,
                &one_line(reason),
            )),
            Ok(tally) => {
                let first_error = tally.first_error.as_deref();
                let first =
                    first_error.map_or_default(|error| format!("first: {}", one_line(error)));
                rows.extend([
                    Row::new(&key("candidate_config"), &"accepted", VERDICT, ""),
                    Row::new(
                        &key("candidate_decoded"),
                        &tally.decoded,
                        UNITS,
                        "units that gave one frame of samples",
                    ),
                    Row::new(&key("candidate_errors"), &tally.errors, UNITS, &first),
                    Row::new(
                        &key("candidate_panics"),
                        &tally.panics,
                        UNITS,
                        "calls that panicked and were caught",
                    ),
                ]);
            }
        }
        let (refused, how) = match &self.oracle.decoded {
            Ok(_) => (
                0_u8,
                format!("exited with success and wrote nothing to {STDERR_FILE}"),
            ),
            Err(refusal) => (1, refusal.to_string()),
        };
        rows.push(Row::new(&key("oracle_refused"), &refused, "runs", &how));
        rows
    }
}

/// A decoder's own words as one line of a table: every control character becomes a space.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|letter| if letter.is_control() { ' ' } else { letter })
        .collect()
}

fn lsb(value: f64) -> String {
    format!("{value:.3}")
}

/// The largest sample of a decode, in steps of a 16 bit sample; none of a decode that holds no sample.
fn peak_lsb(samples: &[f32]) -> Option<f64> {
    let levels = samples.iter().map(|&sample| lsb_apart(sample, 0.0));
    levels.reduce(f64::max)
}

/// A row of a figure, or `refused` with the reason there is none.
fn figure(
    key: &str,
    unit: &'static str,
    found: Option<impl Display>,
    note: &str,
    why_not: &str,
) -> Row {
    match found {
        Some(value) => Row::new(key, &value, unit, note),
        None => Row::new(key, &REFUSED, unit, why_not),
    }
}

/// The coded bytes of every whole unit of every audio SDU the pods sent, in log order: the units the analysis counts.
fn coded_units(records: &[Record]) -> Vec<&[u8]> {
    let mut units = Vec::new();
    for record in records {
        if record.direction != Direction::FromPods {
            continue;
        }
        if let Ok(Some(UplinkPacket::Audio(audio))) = UplinkPacket::parse(&record.sdu) {
            units.extend(audio.units().map(|unit| unit.data));
        }
    }
    units
}

/// The samples one decoder gave and how loud they are, so agreement on silence is seen for what it is; none is a decoder that gave no decode.
fn output_rows(decoder: &str, samples: Option<&[f32]>) -> [Row; 2] {
    let peak = samples.and_then(peak_lsb).map(lsb);
    let peak_note = format!("largest sample x {LSB_PER_FULL_SCALE}");
    [
        figure(
            &format!("{decoder}_samples"),
            SAMPLES,
            samples.map(<[f32]>::len),
            "",
            "it gave no decode",
        ),
        figure(
            &format!("{decoder}_peak_lsb"),
            LSB,
            peak,
            &peak_note,
            "it gave no sample",
        ),
    ]
}

/// The comparison of the two whole decodes, or why there is none.
fn compared(full: &Both<'_>) -> Result<Comparison, String> {
    let candidate = full
        .whole_candidate()
        .ok_or("the candidate did not decode every unit")?;
    let oracle = full.whole_oracle().ok_or("the oracle refused the stream")?;
    compare(candidate, oracle, usize::from(SAMPLES_PER_UNIT))
        .map_err(|error: CompareError| error.to_string())
}

fn comparison_rows(full: &Both<'_>) -> [Row; 4] {
    let offsets = format!(
        "units the candidate is behind the oracle where they agree best; -{SEARCH_UNITS} to {SEARCH_UNITS} tried"
    );
    let largest = format!("largest |candidate - oracle| x {LSB_PER_FULL_SCALE} at that offset");
    let over = format!("frames with a sample more than {TOLERANCE_LSB} LSB apart");
    match compared(full) {
        Ok(found) => [
            Row::new("frames_compared", &found.frames, FRAMES, ""),
            Row::new("offset_units", &found.offset_units, UNITS, &offsets),
            Row::new("max_lsb", &lsb(found.max_lsb), LSB, &largest),
            Row::new(
                "frames_over_one_lsb",
                &found.frames_over_one_lsb,
                FRAMES,
                &over,
            ),
        ],
        Err(why) => [
            Row::new("frames_compared", &REFUSED, FRAMES, &why),
            Row::new("offset_units", &REFUSED, UNITS, &why),
            Row::new("max_lsb", &REFUSED, LSB, &why),
            Row::new("frames_over_one_lsb", &REFUSED, FRAMES, &why),
        ],
    }
}

/// The time the candidate took for each of `calls` units, which is above 0: a magnitude from one run on one machine, never a constant.
fn timing_rows(tally: &Tally, calls: u32) -> [Row; 2] {
    const US_PER_SECOND: f64 = 1e6;
    let mean_us = tally.total.as_secs_f64() * US_PER_SECOND / f64::from(calls);
    let max_us = tally.slowest.as_secs_f64() * US_PER_SECOND;
    let note = format!("{calls} calls in one run on this machine; a magnitude");
    [
        Row::new(
            "candidate_decode_us_mean",
            &format!("{mean_us:.1}"),
            "us",
            &note,
        ),
        Row::new(
            "candidate_decode_us_max",
            &format!("{max_us:.1}"),
            "us",
            &note,
        ),
    ]
}

/// Decodes the run at `run_dir`, writes the decodes and their table into it, all or none, and returns the table.
pub(crate) fn decode_run(run_dir: &Path) -> Result<String, DecodeError> {
    let run = Run::open(run_dir)?;
    let analysis = run.read(ANALYSIS_FILE, MAX_READ_BYTES)?;
    let log = run.read(LOG_FILE, MAX_LOG_BYTES)?;
    let analysis = text_of(ANALYSIS_FILE, &analysis)?;
    // Refused before any decoding: a decode with no rate to label it would be work for nothing.
    let label = Label::read(analysis)?;
    let (log, _cut) = whole_lines(&log);
    let records = parse_log(text_of(LOG_FILE, log)?)?;
    let units = coded_units(&records);
    let found = units.len();
    if values(analysis, UNITS_KEY) != [found.to_string()] {
        return Err(DecodeError::UnitCount { found });
    }
    let calls = match u32::try_from(found) {
        Ok(0) => return Err(DecodeError::NoUnits),
        Ok(calls) if calls <= MAX_UNITS => calls,
        _ => return Err(DecodeError::TooManyUnits { found }),
    };

    // The first file this makes is the media file, so a run decoded before is refused here, before any work.
    let full = Both::of(&run, EXPECTED_CONFIG, &units, SAMPLES_PER_UNIT)?;
    let mut rows = vec![Row::new(
        "units_in",
        &found,
        UNITS,
        "whole units of the audio SDUs in the log, in order",
    )];
    rows.extend(label.rows());
    rows.extend(full.rows(""));
    let from_candidate = full.candidate.as_ref().ok();
    rows.extend(output_rows(
        "candidate",
        from_candidate.map(|tally| tally.samples.as_slice()),
    ));
    rows.extend(output_rows("oracle", full.whole_oracle()));
    rows.extend(comparison_rows(&full));
    if let Some(tally) = from_candidate {
        rows.extend(timing_rows(tally, calls));
    }
    rows.extend(experiments::rows(&run, &units, &full)?);
    let table = table(&rows);

    let Both {
        candidate, oracle, ..
    } = full;
    let decoded = candidate.map_or_default(|tally| tally.samples);
    let mut wav = Vec::new();
    wav::write_f32(&mut wav, label.rate_hz, &decoded).map_err(DecodeError::Wav)?;
    let files = [
        run.write_unnamed(DECODE_FILE, table.as_bytes())?,
        oracle.media,
        oracle.pcm,
        oracle.stderr,
        run.write_unnamed(WAV_FILE, &wav)?,
    ];
    Unnamed::name_all(&files)?;
    Ok(table)
}

#[cfg(test)]
mod tests;
