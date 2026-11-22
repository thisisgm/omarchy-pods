//! FFmpeg as the oracle: one run of the installed binary over the units, judged by its own exit status and by what it said.

use std::fmt;
use std::io;
use std::process::{Command, ExitStatus, Stdio};

use super::super::eldfile;
use super::super::store::{Run, StoreError, Unnamed};
use super::compare::SEARCH_UNITS;

/// The oracle, found on the path.
pub(crate) const FFMPEG: &str = "ffmpeg";
/// Before the decoder and the media file: never read the terminal, print errors only, and stop at the first one.
const BEFORE_INPUT: [&str; 5] = ["-nostdin", "-hide_banner", "-v", "error", "-xerror"];
const INPUT: &str = "-i";
/// After it: the first audio stream as raw little endian floats on the standard output, which is a file of the run.
const AFTER_INPUT: [&str; 7] = [
    "-map",
    "0:a:0",
    "-c:a",
    "pcm_f32le",
    "-f",
    "f32le",
    "pipe:1",
];
/// The units as an ISO media file, which is what the oracle reads.
pub(crate) const MEDIA_FILE: &str = "eld.mp4";
pub(crate) const PCM_FILE: &str = "ffmpeg.f32";
pub(crate) const STDERR_FILE: &str = "ffmpeg.stderr";
/// What the oracle says is read up to 1 MiB; a run that stops at its first error says about ten lines.
pub(crate) const MAX_STDERR_BYTES: u64 = 1024 * 1024;
const SAMPLE_BYTES: usize = size_of::<f32>();

/// The most of the oracle's first line a refusal carries.
const FIRST_LINE_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) status: ExitStatus,
    pub(crate) stderr_lines: usize,
    /// The first line it said: only the whole decode's messages are kept as a file, so an experiment's reason lives here.
    pub(crate) first_line: String,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            status,
            stderr_lines,
            first_line,
        } = self;
        write!(f, "{status}; {stderr_lines} lines said")?;
        if first_line.is_empty() {
            return Ok(());
        }
        write!(f, ", first: {first_line}")
    }
}

/// One run of the oracle: its verdict, and the three files of the run it went through, none of them named yet.
#[derive(Debug)]
pub(crate) struct OracleRun<'run> {
    pub(crate) decoded: Result<Vec<f32>, Refusal>,
    pub(crate) media: Unnamed<'run>,
    pub(crate) pcm: Unnamed<'run>,
    pub(crate) stderr: Unnamed<'run>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum OracleError {
    #[error("could not write the media file the oracle reads: {0}")]
    Media(io::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("could not start {FFMPEG}, the oracle every decode is compared with: {0}")]
    Start(io::Error),
    #[error("{FFMPEG} wrote {bytes} bytes, which is not a whole number of samples")]
    NotSamples { bytes: usize },
}

/// A decode only when the exit status is success and nothing was said: FFmpeg exits with 0 after some errors it prints.
fn judge(status: ExitStatus, said: &[u8]) -> Result<(), Refusal> {
    if status.success() && said.is_empty() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(said);
    // Control characters go, a tab among them, so the line stays one cell of the table.
    let first_line = said
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|character| !character.is_control())
        .take(FIRST_LINE_CHARS)
        .collect();
    Err(Refusal {
        status,
        stderr_lines: said.lines().count(),
        first_line,
    })
}

// Sample input: `00 00 80 3f 00 00 00 bf`, two little endian floats that read 1.0 and -0.5.
fn samples_of(bytes: &[u8]) -> Result<Vec<f32>, OracleError> {
    let (samples, rest) = bytes.as_chunks::<SAMPLE_BYTES>();
    if !rest.is_empty() {
        return Err(OracleError::NotSamples { bytes: bytes.len() });
    }
    Ok(samples
        .iter()
        .map(|sample| f32::from_le_bytes(*sample))
        .collect())
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Job<'job, 'run> {
    pub(crate) media: &'job Unnamed<'run>,
    /// The options that name the decoder of the media file and say how it decodes; none leaves both to FFmpeg.
    pub(crate) decoder: &'static [&'static str],
    pub(crate) pcm_file: &'run str,
    pub(crate) stderr_file: &'run str,
    /// The most bytes of samples that are read back.
    pub(crate) pcm_limit: u64,
}

/// What the oracle made of one media file, and the two files it wrote, neither named yet.
#[derive(Debug)]
pub(crate) struct Heard<'run> {
    pub(crate) decoded: Result<Vec<f32>, Refusal>,
    pub(crate) pcm: Unnamed<'run>,
    pub(crate) stderr: Unnamed<'run>,
}

/// The most bytes a decode of `units` may be: a decoder may add up to the units an offset can explain.
pub(crate) fn pcm_limit(units: usize, samples_per_unit: u16) -> u64 {
    let frames = u64::try_from(units.saturating_add(SEARCH_UNITS)).unwrap_or(u64::MAX);
    frames
        .saturating_mul(u64::from(samples_per_unit))
        .saturating_mul(u64::try_from(SAMPLE_BYTES).unwrap_or(u64::MAX))
}

/// Runs the oracle over one media file of `run`, and reads its exit status from the child itself.
pub(crate) fn hear<'run>(run: &'run Run, job: Job<'_, 'run>) -> Result<Heard<'run>, OracleError> {
    let pcm = run.write_unnamed(job.pcm_file, &[])?;
    let stderr = run.write_unnamed(job.stderr_file, &[])?;
    let status = Command::new(FFMPEG)
        .args(BEFORE_INPUT)
        .args(job.decoder)
        .arg(INPUT)
        .arg(job.media.path_for_child())
        .args(AFTER_INPUT)
        .stdin(Stdio::null())
        .stdout(pcm.handle()?)
        .stderr(stderr.handle()?)
        .status()
        .map_err(OracleError::Start)?;
    let said = stderr.contents(MAX_STDERR_BYTES)?;
    let decoded = match judge(status, &said) {
        Ok(()) => Ok(samples_of(&pcm.contents(job.pcm_limit)?)?),
        Err(refusal) => Err(refusal),
    };
    Ok(Heard {
        decoded,
        pcm,
        stderr,
    })
}

/// Runs the oracle over `units` through files of `run` that have no name yet.
pub(crate) fn oracle<'run>(
    run: &'run Run,
    config: [u8; 4],
    units: &[&[u8]],
    samples_per_unit: u16,
) -> Result<OracleRun<'run>, OracleError> {
    let mut media = Vec::new();
    eldfile::write(&mut media, config, units, u32::from(samples_per_unit))
        .map_err(OracleError::Media)?;
    let media = run.write_unnamed(MEDIA_FILE, &media)?;
    let job = Job {
        media: &media,
        decoder: &[],
        pcm_file: PCM_FILE,
        stderr_file: STDERR_FILE,
        pcm_limit: pcm_limit(units.len(), samples_per_unit),
    };
    let Heard {
        decoded,
        pcm,
        stderr,
    } = hear(run, job)?;
    Ok(OracleRun {
        decoded,
        media,
        pcm,
        stderr,
    })
}

#[cfg(test)]
mod tests;
