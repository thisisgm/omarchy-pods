//! FFmpeg as the oracle: one run of the installed binary over the units, judged by its own exit status and by what it said.

use std::fmt;
use std::io;
use std::process::{Command, ExitStatus, Stdio};

use super::super::eldfile;
use super::super::store::{Run, StoreError, Unnamed};
use super::compare::SEARCH_UNITS;

/// The oracle, found on the path.
pub(crate) const FFMPEG: &str = "ffmpeg";
/// Before the media file: never read the terminal, print errors only, and stop at the first one.
const BEFORE_INPUT: [&str; 6] = ["-nostdin", "-hide_banner", "-v", "error", "-xerror", "-i"];
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
/// The samples as the oracle wrote them.
pub(crate) const PCM_FILE: &str = "ffmpeg.f32";
/// Everything the oracle said.
pub(crate) const STDERR_FILE: &str = "ffmpeg.stderr";
/// What the oracle says is read up to 1 MiB; a run that stops at its first error says about ten lines.
pub(crate) const MAX_STDERR_BYTES: u64 = 1024 * 1024;
const SAMPLE_BYTES: usize = size_of::<f32>();

/// How the oracle refused a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) status: ExitStatus,
    pub(crate) stderr_lines: usize,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            status,
            stderr_lines,
        } = self;
        write!(f, "{status}; {stderr_lines} lines in {STDERR_FILE}")
    }
}

/// One run of the oracle: its verdict, and the three files of the run it went through, none of them named yet.
#[derive(Debug)]
pub(crate) struct OracleRun<'run> {
    /// The samples, or how the oracle refused the stream.
    pub(crate) decoded: Result<Vec<f32>, Refusal>,
    pub(crate) media: Unnamed<'run>,
    pub(crate) pcm: Unnamed<'run>,
    pub(crate) stderr: Unnamed<'run>,
}

/// Why the oracle gave no verdict at all.
#[derive(Debug, thiserror::Error)]
pub(crate) enum OracleError {
    #[error("could not write the units as an ISO media file: {0}")]
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
    let stderr_lines = String::from_utf8_lossy(said).lines().count();
    Err(Refusal {
        status,
        stderr_lines,
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

/// The most bytes a decode of `units` may be: a decoder may add up to the units an offset can explain.
fn pcm_limit(units: usize, samples_per_unit: u16) -> u64 {
    let frames = u64::try_from(units.saturating_add(SEARCH_UNITS)).unwrap_or(u64::MAX);
    frames
        .saturating_mul(u64::from(samples_per_unit))
        .saturating_mul(u64::try_from(SAMPLE_BYTES).unwrap_or(u64::MAX))
}

/// Runs the oracle over `units` through files of `run` that have no name yet, and reads its exit status from the child itself.
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
    let pcm = run.write_unnamed(PCM_FILE, &[])?;
    let stderr = run.write_unnamed(STDERR_FILE, &[])?;
    let status = Command::new(FFMPEG)
        .args(BEFORE_INPUT)
        .arg(media.path_for_child())
        .args(AFTER_INPUT)
        .stdin(Stdio::null())
        .stdout(pcm.handle()?)
        .stderr(stderr.handle()?)
        .status()
        .map_err(OracleError::Start)?;
    let said = stderr.contents(MAX_STDERR_BYTES)?;
    let decoded = match judge(status, &said) {
        Ok(()) => Ok(samples_of(
            &pcm.contents(pcm_limit(units.len(), samples_per_unit))?,
        )?),
        Err(refusal) => Err(refusal),
    };
    Ok(OracleRun {
        decoded,
        media,
        pcm,
        stderr,
    })
}

#[cfg(test)]
mod tests;
