//! Decodes the dictation frames of one analysed run with the Opus decoder candidate and with FFmpeg's two decoders, each by name, compares them, and runs the fixed experiments.

mod candidate;
mod experiments;
mod frames;

use std::io;
use std::path::Path;

use pods_proto::dictation::TOC_CELT_WIDEBAND_20MS_MONO;

use super::analyse::MAX_LOG_BYTES;
use super::analyse::att::FRAMES_FILE;
use super::analyse::input::{ATT_LOG_FILE, InputError, parse_lines, text_of, whole_lines};
use super::analyse::report::{NOTE_SEPARATOR, Row, table};
use super::decode::candidate::Tally;
use super::decode::compare::{AGREEMENT_OF_PEAK, Comparison, TOLERANCE_LSB, compare};
use super::decode::oracle::{Heard, Job, OracleError, hear, pcm_limit};
use super::decode::{
    FRAMES, comparison_rows, figure, output_rows, refusal_row, tally_rows, timing_rows, why_not,
};
use super::oggfile;
use super::store::{MAX_READ_BYTES, Run, StoreError, Unnamed};
use super::wav;
use candidate::{Candidate, OutputRate};
pub(crate) use frames::{FramesError, sessions};

/// The table of figures, which is also what the command prints.
const TABLE_FILE: &str = "decode-opus.tsv";
/// What the candidate decoded at 48 kHz, as a WAV file of floats.
pub(crate) const WAV_FILE: &str = "dictation.f32.wav";
/// The frames as one Ogg Opus stream, which is what both oracles read.
const MEDIA_FILE: &str = "dictation.opus";
/// The rate of the comparison: FFmpeg decodes Opus at 48 kHz, so no resampler stands between the decoders.
const COMPARED: OutputRate = OutputRate::Hz48000;
/// The rate `WAV_FILE` is labelled with and the samples each frame of the run takes in it.
pub(crate) const WAV_RATE_HZ: u32 = COMPARED.hz();
pub(crate) const WAV_FRAME: u16 = COMPARED.frame();
/// Twenty minutes of frames at 50 a second: each decode of that many at 48 kHz is 220 MiB of samples, and four of those and one at 16 kHz are held at once.
const MAX_FRAMES: u32 = 60_000;
/// A packet of the table of contents byte alone, which a decoder conceals as a lost frame: it stands in the Ogg stream for a frame nobody decodes.
const LOST_PACKET: [u8; 1] = [TOC_CELT_WIDEBAND_20MS_MONO];
/// What every row of the oracle that decides nothing says first.
const NOT_A_GATE: &str = "not a gate";
/// The unit of a difference that is a share of a frame's peak.
const OF_PEAK: &str = "of peak";

/// The two Opus decoders of FFmpeg; each run names its own, so neither falls to the one FFmpeg prefers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OracleDecoder {
    /// The wrapper of the reference library: the oracle the candidate is held to.
    Libopus,
    /// FFmpeg's own decoder.
    Native,
}

impl OracleDecoder {
    /// The options before the media file: the name FFmpeg knows the decoder by, and floats from the wrapper, which left alone rounds to 16 bits and clips at full scale.
    const fn input_options(self) -> &'static [&'static str] {
        match self {
            Self::Libopus => &["-c:a", "libopus", "-request_sample_fmt", "flt"],
            Self::Native => &["-c:a", "opus"],
        }
    }

    /// What the keys of its rows start with.
    const fn key(self) -> &'static str {
        match self {
            Self::Libopus => "libopus",
            Self::Native => "ffopus",
        }
    }

    const fn pcm_file(self) -> &'static str {
        match self {
            Self::Libopus => "libopus.f32",
            Self::Native => "ffopus.f32",
        }
    }

    const fn stderr_file(self) -> &'static str {
        match self {
            Self::Libopus => "libopus.stderr",
            Self::Native => "ffopus.stderr",
        }
    }
}

/// Why a run was not decoded.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DecodeOpusError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Input(#[from] InputError),
    #[error(transparent)]
    Frames(#[from] FramesError),
    #[error(transparent)]
    Oracle(#[from] OracleError),
    #[error("{ATT_LOG_FILE} holds no frame, so there is nothing to decode")]
    NoFrames,
    #[error("{ATT_LOG_FILE} holds {found} frames, and one decode takes at most {MAX_FRAMES}")]
    TooManyFrames { found: usize },
    #[error("could not write {WAV_FILE}: {0}")]
    Wav(io::Error),
}

/// Feeds a new candidate every frame in order, each exactly once; a frame nobody decodes is concealed, so the decode keeps its length.
fn decode(rate: OutputRate, coded: &[Option<&[u8]>]) -> Tally {
    let mut candidate = Candidate::new(rate);
    let mut tally = Tally::default();
    let frame = Some(usize::from(rate.frame()));
    for coded in coded {
        match coded {
            Some(bytes) => tally.call(frame, || candidate.frame(bytes)),
            None => tally.call(frame, || candidate.conceal()),
        }
    }
    tally
}

/// The samples of a decode in which each of `frames` gave one frame, so frame n starts at n x the frame length.
fn whole(tally: &Tally, frames: usize) -> Option<&[f32]> {
    (tally.decoded == frames).then_some(tally.samples.as_slice())
}

/// One oracle over the Ogg stream of `frames` frames.
fn oracle<'run>(
    run: &'run Run,
    media: &Unnamed<'run>,
    frames: usize,
    decoder: OracleDecoder,
) -> Result<Heard<'run>, OracleError> {
    let job = Job {
        media,
        decoder: decoder.input_options(),
        pcm_file: decoder.pcm_file(),
        stderr_file: decoder.stderr_file(),
        pcm_limit: pcm_limit(frames, COMPARED.frame()),
    };
    hear(run, job)
}

/// How far the candidate is from one oracle: the rows every comparison has, the largest share of a peak and the frames over the gate.
fn against_rows(decoder: OracleDecoder, compared: &Result<Comparison, String>) -> Vec<Row> {
    let key = decoder.key();
    let found = compared.as_ref().ok();
    let mut against = comparison_rows(&format!("{key}_"), compared).to_vec();
    against.extend([
        figure(
            &format!("{key}_frames_over_one_lsb"),
            FRAMES,
            found.map(|found| found.frames_over_one_lsb),
            &format!(
                "frames with a sample more than {TOLERANCE_LSB} LSB apart, whatever their peak"
            ),
            why_not(compared),
        ),
        figure(
            &format!("{key}_max_of_peak"),
            OF_PEAK,
            found.map(|found| format!("{:.3e}", found.max_of_peak)),
            &format!(
                "largest of each frame's |candidate - oracle| over the peak of that frame in either decode; a frame within {TOLERANCE_LSB} LSB agrees whatever its share, so a quiet frame can put this above {AGREEMENT_OF_PEAK} with no frame over the gate"
            ),
            why_not(compared),
        ),
        figure(
            &format!("{key}_frames_over_gate"),
            FRAMES,
            found.map(|found| found.frames_over_gate),
            &format!(
                "frames further apart than the larger of {TOLERANCE_LSB} LSB and {AGREEMENT_OF_PEAK} of their peak"
            ),
            why_not(compared),
        ),
    ]);
    if decoder == OracleDecoder::Native {
        for row in &mut against {
            let said: Vec<&str> = [NOT_A_GATE, &row.note]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect();
            row.note = said.join(NOTE_SEPARATOR);
        }
    }
    against
}

/// What one oracle made of the stream and how far the candidate's whole decode is from it.
fn oracle_rows(decoder: OracleDecoder, heard: &Heard<'_>, candidate: Option<&[f32]>) -> Vec<Row> {
    let key = decoder.key();
    let from_oracle = heard.decoded.as_deref().ok();
    let compared = || -> Result<Comparison, String> {
        let candidate = candidate.ok_or("the candidate did not decode every frame")?;
        let oracle = from_oracle.ok_or("the oracle refused the stream")?;
        compare(candidate, oracle, usize::from(COMPARED.frame())).map_err(|error| error.to_string())
    };
    let refused = refusal_row(
        &format!("{key}_refused"),
        &heard.decoded,
        decoder.stderr_file(),
    );
    let mut rows = vec![refused];
    rows.extend(output_rows(key, from_oracle));
    rows.extend(against_rows(decoder, &compared()));
    rows
}

/// Decodes the run at `run_dir`, writes the decodes and their table into it, all or none, and returns the table.
pub(crate) fn decode_run(run_dir: &Path) -> Result<String, DecodeOpusError> {
    let run = Run::open(run_dir)?;
    let listed = run.read(FRAMES_FILE, MAX_READ_BYTES)?;
    let log = run.read(ATT_LOG_FILE, MAX_LOG_BYTES)?;
    let (log, _cut) = whole_lines(&log);
    let records = parse_lines(ATT_LOG_FILE, text_of(ATT_LOG_FILE, log)?)?;
    let frames = frames::load(&records, text_of(FRAMES_FILE, &listed)?)?;
    let found = frames.len();
    let calls = match u32::try_from(found) {
        Ok(0) => return Err(DecodeOpusError::NoFrames),
        Ok(calls) if calls <= MAX_FRAMES => calls,
        _ => return Err(DecodeOpusError::TooManyFrames { found }),
    };
    let coded: Vec<Option<&[u8]>> = frames.iter().map(|frame| frame.coded).collect();
    let packets: Vec<&[u8]> = coded
        .iter()
        .map(|coded| coded.unwrap_or(&LOST_PACKET))
        .collect();
    let mut media = Vec::new();
    oggfile::write(&mut media, &packets).map_err(OracleError::Media)?;

    // The first file this makes is the media file, so a run decoded before is refused here, before any work.
    let media = run.write_unnamed(MEDIA_FILE, &media)?;
    let libopus = oracle(&run, &media, found, OracleDecoder::Libopus)?;
    let ffopus = oracle(&run, &media, found, OracleDecoder::Native)?;
    let full = decode(COMPARED, &coded);
    let narrow = decode(OutputRate::Hz16000, &coded);
    let candidate = whole(&full, found);

    let not_decoded = coded.iter().filter(|coded| coded.is_none()).count();
    let mut rows = vec![
        Row::new(
            "frames_in",
            &found,
            FRAMES,
            "whole frames of the audio notifications in the log, in order",
        ),
        Row::new(
            "frames_not_b8",
            &not_decoded,
            FRAMES,
            "frames whose first byte is not b8: nobody decodes one, and each decoder conceals one frame in its place, in its own way",
        ),
    ];
    rows.extend(tally_rows("", &full, FRAMES));
    rows.extend(output_rows("candidate", Some(&full.samples)));
    rows.extend(oracle_rows(OracleDecoder::Libopus, &libopus, candidate));
    rows.extend(oracle_rows(OracleDecoder::Native, &ffopus, candidate));
    rows.extend(timing_rows(&full, calls));
    rows.extend(tally_rows("rate_16k_", &narrow, FRAMES));
    rows.extend(output_rows("rate_16k_candidate", Some(&narrow.samples)));
    rows.extend(experiments::rows(&frames, candidate));
    let table = table(&rows);

    let mut wav = Vec::new();
    wav::write_f32(&mut wav, COMPARED.hz(), &full.samples).map_err(DecodeOpusError::Wav)?;
    let files = [
        run.write_unnamed(TABLE_FILE, table.as_bytes())?,
        media,
        libopus.pcm,
        libopus.stderr,
        ffopus.pcm,
        ffopus.stderr,
        run.write_unnamed(WAV_FILE, &wav)?,
    ];
    Unnamed::name_all(&files)?;
    Ok(table)
}

#[cfg(test)]
mod tests;
