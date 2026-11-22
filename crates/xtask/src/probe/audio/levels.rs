//! Levels of a stretch of samples: its RMS and peak, its active frames, its quietest stretch and how much of it is near zero.

use std::ops::Range;

use super::super::analyse::stats::count;

/// The frame speech activity and audibility are judged in.
const FRAME_MS: u32 = 20;
/// A frame is active within this much of the loudest frame of its window.
const ACTIVE_WITHIN_DB: f64 = 10.0;
/// The step the quietest stretch is searched in.
const HOP_MS: u32 = 10;
/// The stretch a noise floor is read from.
const FLOOR_MS: u32 = 3000;
/// A frame at or over this RMS counts as audible: 60 dB under full scale, below any speech a close microphone records.
const AUDIBLE_DBFS: f64 = -60.0;
/// One step of a 16 bit sample.
const LSB_16: f32 = 1.0 / 32_768.0;
/// The power of a signal one 16 bit step in size, under which no rise and no envelope is read.
pub(crate) const LSB_POWER: f64 = 1.0 / (32_768.0 * 32_768.0);
const MS_PER_SECOND: u64 = 1000;

/// The RMS and the peak of a stretch, in dB under full scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Level {
    pub(crate) rms_dbfs: f64,
    pub(crate) peak_dbfs: f64,
}

/// Why a stretch has no level in dB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum NoLevel {
    #[error("it holds no sample")]
    NoSample,
    #[error("it holds no whole {FRAME_MS} ms frame")]
    NoFrame,
    #[error("it is shorter than the {FLOOR_MS} ms a floor is read from")]
    NoFloor,
    #[error("it is digital silence: every sample is exactly zero")]
    DigitalSilence,
    #[error("its quietest {FLOOR_MS} ms are digital silence")]
    FloorOfZeros,
    #[error("none of its {FRAME_MS} ms frames reaches {AUDIBLE_DBFS} dBFS")]
    NotAudible,
}

/// The samples `ms` milliseconds hold at `rate`.
pub(crate) fn samples_in(ms: u32, rate: u32) -> usize {
    let samples = u64::from(ms).saturating_mul(u64::from(rate)) / MS_PER_SECOND;
    usize::try_from(samples).unwrap_or(usize::MAX)
}

/// How long `samples` samples last at `rate`, in seconds.
pub(crate) fn seconds_of(samples: usize, rate: u32) -> f64 {
    count(samples) / f64::from(rate)
}

/// A power ratio in dB; a ratio of zero, or one that is no number, has none.
pub(crate) fn db(power: f64) -> Option<f64> {
    let db = 10.0 * power.log10();
    db.is_finite().then_some(db)
}

fn energy(samples: &[f32]) -> f64 {
    let squares = samples.iter().map(|&sample| f64::from(sample).powi(2));
    squares.sum()
}

/// The mean power of a stretch that holds a sample.
pub(crate) fn mean_power(samples: &[f32]) -> Option<f64> {
    (!samples.is_empty()).then(|| energy(samples) / count(samples.len()))
}

pub(crate) fn is_digital_silence(samples: &[f32]) -> bool {
    samples.iter().all(|&sample| sample == 0.0)
}

pub(crate) fn level(samples: &[f32]) -> Result<Level, NoLevel> {
    let power = mean_power(samples).ok_or(NoLevel::NoSample)?;
    let peak = samples
        .iter()
        .fold(0.0_f64, |peak, &sample| peak.max(f64::from(sample).abs()));
    match (db(power), db(peak * peak)) {
        (Some(rms_dbfs), Some(peak_dbfs)) => Ok(Level {
            rms_dbfs,
            peak_dbfs,
        }),
        _ => Err(NoLevel::DigitalSilence),
    }
}

/// What a level over the active frames says of itself.
pub(crate) fn active_note(frames: usize) -> String {
    format!("over the {frames} frames of {FRAME_MS} ms within {ACTIVE_WITHIN_DB} dB of the loudest")
}

/// The 20 ms frames within 10 dB of the loudest frame, neighbours joined; a window of silence has none.
pub(crate) fn active(samples: &[f32], rate: u32) -> Vec<Range<usize>> {
    let frame = samples_in(FRAME_MS, rate).max(1);
    let energies: Vec<f64> = samples.chunks_exact(frame).map(energy).collect();
    let loudest = energies.iter().copied().fold(0.0, f64::max);
    let faintest = loudest / 10.0_f64.powf(ACTIVE_WITHIN_DB / 10.0);
    let mut found: Vec<Range<usize>> = Vec::new();
    for (index, &energy) in energies.iter().enumerate() {
        if loudest <= 0.0 || energy < faintest {
            continue;
        }
        let start = index.saturating_mul(frame);
        let end = start.saturating_add(frame);
        match found.last_mut() {
            Some(last) if last.end == start => last.end = end,
            _ => found.push(start..end),
        }
    }
    found
}

/// The level over the active frames of a window, and how many frames that is; refused where no frame is audible, so a room is never read as speech.
pub(crate) fn speech(samples: &[f32], rate: u32) -> Result<(Level, usize), NoLevel> {
    let frame = samples_in(FRAME_MS, rate).max(1);
    if samples.len() < frame {
        return Err(NoLevel::NoFrame);
    }
    if is_digital_silence(samples) {
        return Err(NoLevel::DigitalSilence);
    }
    if first_audible(samples, rate).is_none() {
        return Err(NoLevel::NotAudible);
    }
    let ranges = active(samples, rate);
    let spoken: Vec<f32> = ranges
        .iter()
        .filter_map(|range| samples.get(range.clone()))
        .flatten()
        .copied()
        .collect();
    Ok((
        level(&spoken)?,
        spoken.len().checked_div(frame).unwrap_or(0),
    ))
}

/// The `ms` long stretch of least energy, found in steps of 10 ms; a window shorter than that has none.
pub(crate) fn quietest(samples: &[f32], rate: u32, ms: u32) -> Option<Range<usize>> {
    let hop = samples_in(HOP_MS, rate).max(1);
    let hops = usize::try_from(ms / HOP_MS).ok()?.max(1);
    let energies: Vec<f64> = samples.chunks_exact(hop).map(energy).collect();
    let mut best: Option<(usize, f64)> = None;
    for (start, stretch) in energies.windows(hops).enumerate() {
        let energy: f64 = stretch.iter().sum();
        if best.is_none_or(|(_, least)| energy < least) {
            best = Some((start, energy));
        }
    }
    let start = best?.0.saturating_mul(hop);
    Some(start..start.saturating_add(hops.saturating_mul(hop)))
}

/// What a floor says of itself.
pub(crate) fn floor_note() -> String {
    format!("the RMS of the quietest {FLOOR_MS} ms of the window")
}

/// The quietest three seconds of a window, which its floor is read from; a shorter window has none.
pub(crate) fn floor_stretch(samples: &[f32], rate: u32) -> Option<&[f32]> {
    samples.get(quietest(samples, rate, FLOOR_MS)?)
}

/// The RMS of the quietest three seconds of a window, in dB under full scale.
pub(crate) fn floor_dbfs(samples: &[f32], rate: u32) -> Result<f64, NoLevel> {
    let quiet = floor_stretch(samples, rate).ok_or(NoLevel::NoFloor)?;
    match level(quiet) {
        Ok(level) => Ok(level.rms_dbfs),
        Err(NoLevel::DigitalSilence) => Err(NoLevel::FloorOfZeros),
        Err(why) => Err(why),
    }
}

/// The share of samples within one 16 bit step of zero.
pub(crate) fn near_zero_share(samples: &[f32]) -> Option<f64> {
    let near = samples.iter().filter(|sample| sample.abs() <= LSB_16);
    (!samples.is_empty()).then(|| count(near.count()) / count(samples.len()))
}

/// What a first audible frame says of itself, for a start of `samples` samples.
pub(crate) fn audible_note(samples: usize) -> String {
    format!(
        "from the first sample of the start to the first {FRAME_MS} ms frame at {AUDIBLE_DBFS} dBFS or over, in the stream's own samples; of {samples} samples"
    )
}

/// Where the first 20 ms frame at or over `AUDIBLE_DBFS` starts.
pub(crate) fn first_audible(samples: &[f32], rate: u32) -> Option<usize> {
    let frame = samples_in(FRAME_MS, rate).max(1);
    let audible = 10.0_f64.powf(AUDIBLE_DBFS / 10.0);
    let mut frames = samples.chunks_exact(frame);
    let index = frames.position(|frame| mean_power(frame).is_some_and(|power| power >= audible))?;
    Some(index.saturating_mul(frame))
}
