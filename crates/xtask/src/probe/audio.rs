//! Acoustic measurements of the decoded audio of one run: levels, noise floor, bandwidth, taps and first audible frame, and with a reference recording the same sound against it.

mod alignment;
mod levels;
mod onsets;
mod reference;
mod rows;
#[cfg(test)]
pub(crate) mod signal;
mod spectrum;
mod timeline;

use std::path::PathBuf;

use clap::Args;

use super::analyse::input::{
    InputError, MARKS_FILE, META_FILE, parse_cues, parse_marks, parse_meta, text_of, whole_lines,
};
use super::analyse::load::{UPLINK, windows};
use super::analyse::report::table;
use super::analyse::{UNITS_FILE, write_new};
use super::decode::{SAMPLES_PER_UNIT, WAV_FILE};
use super::script::ScriptName;
use super::store::{self, MAX_READ_BYTES, Run, StoreError};
use super::wav::{self, Wav, WavError};
// The generator of signals names it as its neighbour, here and in the test of the built command.
#[cfg(test)]
use super::xorshift;
use reference::Reference;
use rows::Recording;
use timeline::{Timeline, TimelineError, cue_windows};

/// The table of figures, which is also what the command prints.
const AUDIO_FILE: &str = "audio.tsv";
/// A decode writes at most 293 MiB of samples, and twenty minutes of 24 bit stereo at 48 kHz are 330 MiB.
const MAX_WAV_BYTES: u64 = 512 * 1024 * 1024;
/// The least rate measured: at it the 2 kHz a tap is read above still lies an octave under half the rate.
const MIN_RATE_HZ: u32 = 8000;

/// What `probe audio` takes.
#[derive(Debug, Args)]
pub(crate) struct AudioArgs {
    /// The run directory, a session directory of the store then the run name.
    run_dir: PathBuf,
    /// A WAV recording of the same sound by another microphone: the caller's own, with no access for group or others.
    #[arg(long, requires = "reference_start_ns")]
    reference: Option<PathBuf>,
    /// The wall clock time of the first sample of the reference, in nanoseconds since 1970.
    #[arg(long, requires = "reference")]
    reference_start_ns: Option<u64>,
}

/// Why a run was not measured.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AudioError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Input(#[from] InputError),
    #[error(transparent)]
    Timeline(#[from] TimelineError),
    #[error("{file} was not read: {source}")]
    Wav { file: String, source: WavError },
    #[error("{file} is labelled {rate} Hz, and under {MIN_RATE_HZ} Hz nothing is measured")]
    Rate { file: String, rate: u32 },
    #[error(
        "{WAV_FILE} holds {samples} samples and {UNITS_FILE} lists units for {listed}, so the two are not of one decode"
    )]
    NotOfOneDecode { samples: usize, listed: usize },
    #[error("{MARKS_FILE} holds no cue and no start mark, so there is no window to measure")]
    NothingToMeasure,
    #[error(
        "{META_FILE} has no realtime_at_zero_ns line, so nothing places the reference on the clock of the run"
    )]
    NoRealtime,
}

fn read_wav(file: &str, bytes: &[u8]) -> Result<Wav, AudioError> {
    let file = file.to_owned();
    let wav = wav::read(bytes).map_err(|source| AudioError::Wav {
        file: file.clone(),
        source,
    })?;
    if wav.rate < MIN_RATE_HZ {
        return Err(AudioError::Rate {
            file,
            rate: wav.rate,
        });
    }
    Ok(wav)
}

/// Measures the decoded audio of the run, writes the table into the run and returns it.
pub(crate) fn audio_run(args: &AudioArgs) -> Result<String, AudioError> {
    let run = Run::open(&args.run_dir)?;
    let marks = run.read(MARKS_FILE, MAX_READ_BYTES)?;
    let meta = run.read(META_FILE, MAX_READ_BYTES)?;
    let units = run.read(UNITS_FILE, MAX_READ_BYTES)?;
    let stream = read_wav(WAV_FILE, &run.read(WAV_FILE, MAX_WAV_BYTES)?)?;
    let reference = match (&args.reference, args.reference_start_ns) {
        (Some(path), Some(start_ns)) => {
            let bytes = store::read_trace(path, MAX_WAV_BYTES)?;
            Some((read_wav(&path.display().to_string(), &bytes)?, start_ns))
        }
        _ => None,
    };

    let (marks, marks_cut) = whole_lines(&marks);
    let marks = text_of(MARKS_FILE, marks)?;
    let (cues, marks) = (parse_cues(marks)?, parse_marks(marks)?);
    let meta = parse_meta(text_of(META_FILE, &meta)?)?;
    let samples_per_unit = usize::from(SAMPLES_PER_UNIT);
    let timeline = Timeline::parse(text_of(UNITS_FILE, &units)?, samples_per_unit)?;
    let (samples, listed) = (stream.samples.len(), timeline.samples());
    if samples != listed {
        return Err(AudioError::NotOfOneDecode { samples, listed });
    }
    let starts = windows(&marks, UPLINK);
    if cues.is_empty() && starts.is_empty() {
        return Err(AudioError::NothingToMeasure);
    }
    let reference = match &reference {
        Some((wav, start_ns)) => {
            let realtime_at_zero_ns = meta.realtime_at_zero_ns.ok_or(AudioError::NoRealtime)?;
            Some(Reference::new(wav, *start_ns, realtime_at_zero_ns))
        }
        None => None,
    };
    let recording = Recording {
        stream: &stream,
        timeline: &timeline,
        cues: cue_windows(&cues, &marks),
        starts,
        marks_cut,
        firstword: meta.script == ScriptName::Firstword.to_string(),
        reference,
    };
    let table = table(&rows::rows(&recording));
    write_new(&run, &[(AUDIO_FILE, &table)])?;
    Ok(table)
}

#[cfg(test)]
mod tests;
