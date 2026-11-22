//! Acoustic measurements of a run's decoded audio: alone, against a reference recording, or one file whole.

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

use clap::{Args, ValueEnum};

use super::analyse::att::FRAMES_FILE;
use super::analyse::input::{
    InputError, MARKS_FILE, META_FILE, parse_cues, parse_marks, parse_meta, text_of, whole_lines,
};
use super::analyse::load::{UPLINK, windows};
use super::analyse::report::table;
use super::analyse::{UNITS_FILE, write_new};
use super::decode::{SAMPLES_PER_UNIT, WAV_FILE};
use super::decode_opus::{
    FramesError, WAV_FILE as DICTATION_WAV, WAV_FRAME, WAV_RATE_HZ, sessions,
};
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
/// The table of a file measured whole: the uplink audio, and the dictation audio.
const WHOLE_FILE: &str = "audio-whole.tsv";
const DICTATION_FILE: &str = "audio-dictation.tsv";
/// A decode writes at most 293 MiB of samples, and twenty minutes of 24 bit stereo at 48 kHz are 330 MiB.
const MAX_WAV_BYTES: u64 = 512 * 1024 * 1024;
/// The least rate measured: at it the 2 kHz a tap is read above still lies an octave under half the rate.
const MIN_RATE_HZ: u32 = 8000;

/// Which decoded file of the run is measured, by its name in the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Pcm {
    /// The uplink audio `decode` writes.
    #[value(name = WAV_FILE)]
    Pods,
    /// The dictation audio `decode-opus` writes.
    #[value(name = DICTATION_WAV)]
    Dictation,
}

/// What `probe audio` takes.
#[derive(Debug, Args)]
pub(crate) struct AudioArgs {
    /// The run directory, a session directory of the store then the run name.
    run_dir: PathBuf,
    /// The decoded file of the run to measure; the dictation audio has no cue, so it is measured with --whole alone.
    #[arg(long, value_enum, default_value_t = Pcm::Pods, requires_if(DICTATION_WAV, "whole"))]
    pcm: Pcm,
    /// Measures the file as one window with no cue and no mark read, and the dictation audio session by session too; writes audio-whole.tsv or audio-dictation.tsv.
    #[arg(long, conflicts_with = "reference")]
    whole: bool,
    /// A WAV recording of the same sound by another microphone: the caller's own, with no access for group or others.
    #[arg(long, requires = "reference_start_ns")]
    reference: Option<PathBuf>,
    /// The wall clock time of the first sample of the reference, in nanoseconds since 1970.
    #[arg(long, requires = "reference")]
    reference_start_ns: Option<u64>,
}

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
    #[error(transparent)]
    Frames(#[from] FramesError),
    #[error("{DICTATION_WAV} is labelled {rate} Hz, and decode-opus writes {WAV_RATE_HZ} Hz")]
    DictationRate { rate: u32 },
    #[error(
        "{DICTATION_WAV} holds {samples} samples and {FRAMES_FILE} lists {frames} frames of {WAV_FRAME} samples, so the two are not of one decode"
    )]
    NotOfOneDictation { samples: usize, frames: usize },
    #[error(
        "{FRAMES_FILE} does not list each session as one unbroken run of lines, in rising order: session {session} follows session {after}"
    )]
    SessionTwice { session: u32, after: u32 },
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

/// The samples of each session of the dictation audio, by its number: one unbroken run of the lines of the frames table, each after the one before.
fn session_spans<'wav>(
    stream: &'wav Wav,
    sessions: &[Option<u32>],
) -> Result<Vec<(u32, &'wav [f32])>, AudioError> {
    if stream.rate != WAV_RATE_HZ {
        return Err(AudioError::DictationRate { rate: stream.rate });
    }
    let frame = usize::from(WAV_FRAME);
    let not_of_one = || AudioError::NotOfOneDictation {
        samples: stream.samples.len(),
        frames: sessions.len(),
    };
    let mut rest = stream.samples.as_slice();
    let mut spans: Vec<(u32, &[f32])> = Vec::new();
    for run in sessions.chunk_by(|one, next| one == next) {
        let held = run.len().saturating_mul(frame);
        let (samples, after) = rest.split_at_checked(held).ok_or_else(not_of_one)?;
        rest = after;
        let Some(&Some(session)) = run.first() else {
            continue;
        };
        match spans.last() {
            Some(&(after, _)) if after >= session => {
                return Err(AudioError::SessionTwice { session, after });
            }
            _ => spans.push((session, samples)),
        }
    }
    if !rest.is_empty() {
        return Err(not_of_one());
    }
    Ok(spans)
}

/// Measures one decoded file of the run as a whole, the dictation audio session by session too, and writes the table into the run.
fn whole_run(run: &Run, pcm: Pcm) -> Result<String, AudioError> {
    let (rows, file) = match pcm {
        Pcm::Pods => {
            let stream = read_wav(WAV_FILE, &run.read(WAV_FILE, MAX_WAV_BYTES)?)?;
            (rows::whole_rows(&stream, &[]), WHOLE_FILE)
        }
        Pcm::Dictation => {
            let listed = run.read(FRAMES_FILE, MAX_READ_BYTES)?;
            let stream = read_wav(DICTATION_WAV, &run.read(DICTATION_WAV, MAX_WAV_BYTES)?)?;
            let spans = session_spans(&stream, &sessions(text_of(FRAMES_FILE, &listed)?)?)?;
            (rows::whole_rows(&stream, &spans), DICTATION_FILE)
        }
    };
    let table = table(&rows);
    write_new(run, &[(file, &table)])?;
    Ok(table)
}

/// Measures the decoded audio of the run, writes the table into the run and returns it.
pub(crate) fn audio_run(args: &AudioArgs) -> Result<String, AudioError> {
    let run = Run::open(&args.run_dir)?;
    if args.whole {
        return whole_run(&run, args.pcm);
    }
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
