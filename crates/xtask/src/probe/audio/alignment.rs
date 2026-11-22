//! Places a stretch of the stream in a reference recording by the shape of its loudness, and asks whether the start of a stretch belongs to what follows it.

use super::super::analyse::stats::count;
use super::levels::{LSB_POWER, db, mean_power, samples_in};

const ENVELOPE_MS: u32 = 5;
const STEP_MS: usize = 1;
/// Under this correlation of the two envelopes nothing was found.
const MIN_SCORE: f64 = 0.5;
/// The head of a start is its first second, and the body the three seconds after it.
const HEAD_MS: u32 = 1000;
const BODY_MS: u32 = 3000;
pub(crate) const HEAD_AND_BODY_MS: u32 = HEAD_MS + BODY_MS;
/// How far from where it is expected each is searched: a second of lead, and as much of clock error between two machines.
pub(crate) const SEARCH_SPAN_MS: u32 = 2000;
const MS_PER_SECOND: u64 = 1000;
/// Head and body agree when they place the start within this of each other.
pub(crate) const AGREE_WITHIN_MS: u64 = 20;

/// Where a stretch lies in the reference, in milliseconds from the reference's first sample, and how well the envelopes match there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Alignment {
    pub(crate) offset_ms: i64,
    pub(crate) score: f64,
}

/// Where the head and the body of a start each place its first sample, and how much louder the head is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HeadCheck {
    pub(crate) head: Option<Alignment>,
    pub(crate) body: Option<Alignment>,
    /// `None` when either is digital silence or missing.
    pub(crate) head_minus_body_db: Option<f64>,
}

impl Alignment {
    pub(crate) fn agrees_with(self, other: Self) -> bool {
        self.offset_ms.abs_diff(other.offset_ms) <= AGREE_WITHIN_MS
    }
}

/// The energy of a stretch of samples up to each of them, so the power of any frame inside it is one subtraction.
#[derive(Debug)]
struct Running {
    /// The first sample of the stretch.
    from: usize,
    /// Entry k is the energy of the first k samples of the stretch.
    sums: Vec<f64>,
    rate: u32,
}

impl Running {
    /// Over `samples[from..until]`, cut to what the samples hold.
    fn over(samples: &[f32], rate: u32, from: usize, until: usize) -> Self {
        let stretch = samples.get(from..until.min(samples.len())).unwrap_or(&[]);
        let mut sum = 0.0_f64;
        let mut sums = vec![0.0];
        sums.extend(stretch.iter().map(|&sample| {
            sum += f64::from(sample).powi(2);
            sum
        }));
        Self { from, sums, rate }
    }

    /// The loudness in dB of `frames` frames of 5 ms from `from_ms` on, never under one 16 bit step; `None` when one leaves the stretch.
    fn envelope(&self, from_ms: u32, frames: u32) -> Option<Vec<f64>> {
        let edge = |frame: u32| {
            let at_ms = from_ms.saturating_add(frame.saturating_mul(ENVELOPE_MS));
            let sample = samples_in(at_ms, self.rate).checked_sub(self.from)?;
            Some((sample, *self.sums.get(sample)?))
        };
        let loudness = |frame: u32| {
            let ((start, before), (end, after)) = (edge(frame)?, edge(frame.saturating_add(1))?);
            let power = (after - before) / count(end.checked_sub(start)?);
            db(power.max(LSB_POWER))
        };
        (0..frames).map(loudness).collect()
    }
}

/// The correlation of two lists of one length; `None` when either does not vary.
fn correlation(a: &[f64], b: &[f64]) -> Option<f64> {
    let mean = |values: &[f64]| values.iter().sum::<f64>() / count(values.len());
    let (mean_a, mean_b) = (mean(a), mean(b));
    let (mut cross, mut spread_a, mut spread_b) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (&a, &b) in a.iter().zip(b) {
        cross += (a - mean_a) * (b - mean_b);
        spread_a += (a - mean_a) * (a - mean_a);
        spread_b += (b - mean_b) * (b - mean_b);
    }
    let score = cross / (spread_a * spread_b).sqrt();
    score.is_finite().then_some(score)
}

/// Finds `stream` in `reference` within `span_ms` of `around_ms` by the correlation of their envelopes in dB; `None` when no offset tried scores `MIN_SCORE`.
pub(crate) fn align(
    stream: &[f32],
    stream_rate: u32,
    reference: &[f32],
    reference_rate: u32,
    around_ms: i64,
    span_ms: u32,
) -> Option<Alignment> {
    let stream_ms = u64::try_from(stream.len())
        .ok()?
        .saturating_mul(MS_PER_SECOND);
    let stream_ms = stream_ms.checked_div(u64::from(stream_rate))?;
    let frames = u32::try_from(stream_ms.checked_div(u64::from(ENVELOPE_MS))?).ok()?;
    let ours = Running::over(stream, stream_rate, 0, stream.len()).envelope(0, frames)?;
    let first_ms = u32::try_from(around_ms.saturating_sub(i64::from(span_ms)).max(0)).ok()?;
    let last_ms = u32::try_from(around_ms.saturating_add(i64::from(span_ms))).ok()?;
    let end_ms = last_ms.saturating_add(frames.saturating_add(1).saturating_mul(ENVELOPE_MS));
    let (from, until) = (
        samples_in(first_ms, reference_rate),
        samples_in(end_ms, reference_rate),
    );
    let theirs = Running::over(reference, reference_rate, from, until);
    let mut best: Option<Alignment> = None;
    for offset_ms in (first_ms..=last_ms).step_by(STEP_MS) {
        let there = theirs.envelope(offset_ms, frames);
        let Some(score) = there.and_then(|there| correlation(&ours, &there)) else {
            continue;
        };
        if best.is_none_or(|found| score > found.score) {
            best = Some(Alignment {
                offset_ms: i64::from(offset_ms),
                score,
            });
        }
    }
    best.filter(|found| found.score >= MIN_SCORE)
}

/// Aligns the first second of a start and the three seconds after it, each on its own, around where the start is expected.
pub(crate) fn head(
    stream: &[f32],
    rate: u32,
    reference: &[f32],
    reference_rate: u32,
    around_ms: i64,
) -> HeadCheck {
    let head_len = samples_in(HEAD_MS, rate);
    let (head, rest) = stream.split_at_checked(head_len).unwrap_or((stream, &[]));
    let body = rest.get(..samples_in(BODY_MS, rate)).unwrap_or(&[]);
    let place = |stretch, around_ms| {
        align(
            stretch,
            rate,
            reference,
            reference_rate,
            around_ms,
            SEARCH_SPAN_MS,
        )
    };
    let body_around_ms = around_ms.saturating_add(i64::from(HEAD_MS));
    let body_place = place(body, body_around_ms).map(|found| Alignment {
        offset_ms: found.offset_ms.saturating_sub(i64::from(HEAD_MS)),
        score: found.score,
    });
    let loudness = |stretch| mean_power(stretch).and_then(db);
    HeadCheck {
        head: place(head, around_ms),
        body: body_place,
        head_minus_body_db: loudness(head)
            .zip(loudness(body))
            .map(|(head, body)| head - body),
    }
}
