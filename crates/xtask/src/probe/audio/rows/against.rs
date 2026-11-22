//! The rows only a reference recording gives: the taps of both recordings against each other, and the head of each start against its body.

use super::super::super::analyse::load::Window;
use super::super::super::analyse::report::{NONE, Row};
use super::super::super::analyse::stats::{median, to_f64};
use super::super::alignment::{
    AGREE_WITHIN_MS, Alignment, HEAD_AND_BODY_MS, HeadCheck, SEARCH_SPAN_MS, head,
};
use super::super::levels::{samples_in, seconds_of};
use super::super::onsets::{SAME_TAP_MS, SameTaps, ratio_ppm, same_taps, taps};
use super::super::reference::Reference;
use super::{ANSWER, DB, Found, MS, RECORDER_CLOCK, Recording, answer, counted, figure};

/// Why a window has no figure against the reference.
pub(super) const NOT_HELD: &str = "the reference does not hold the whole window";
const NS_PER_MS: f64 = 1e6;
/// What a tap row says where under three taps leave none between the first and the last.
const TOO_FEW_TAPS: &str = "too few to show that they are the same taps in both";
const PPM: &str = "ppm";
/// The rows the taps of both recordings give, each with its unit.
const TAP_ROWS: [(&str, &str); 4] = [
    ("tap_spacing_ratio", PPM),
    ("tap_arrival_minus_reference_min", MS),
    ("tap_arrival_minus_reference_median", MS),
    ("tap_arrival_minus_reference_max", MS),
];

/// The taps found in one tap window of the stream, as samples of the stream.
#[derive(Debug)]
pub(super) struct TapWindow {
    pub(super) key: String,
    pub(super) window: Window,
    pub(super) onsets: Vec<usize>,
}

/// Each tap of the stream with the tap at its place in the reference, as a sample of each, and whether their places show them the same taps; refused when a window holds another count of taps in the two, or a tap lies off its own.
fn tap_pairs(
    recording: &Recording<'_>,
    reference: &Reference<'_>,
    windows: &[TapWindow],
) -> Found<(Vec<(usize, usize)>, bool)> {
    let mut pairs = Vec::new();
    for tap_window in windows {
        let key = &tap_window.key;
        let held = reference.stretch(tap_window.window);
        let (start, samples) = held.ok_or_else(|| format!("{key}: {NOT_HELD}"))?;
        let theirs = taps(samples, reference.wav.rate);
        let (in_stream, in_reference) = (tap_window.onsets.len(), theirs.len());
        if in_stream != in_reference {
            return Err(format!(
                "{key} holds {in_stream} taps in the stream and {in_reference} in the reference"
            ));
        }
        let theirs = theirs.iter().map(|onset| onset.saturating_add(start));
        pairs.extend(tap_window.onsets.iter().copied().zip(theirs));
    }
    let (ours, theirs) = seconds(recording, reference, &pairs);
    match same_taps(&ours, &theirs) {
        SameTaps::Not { tap } => Err(format!(
            "the two do not hold the same taps: with the first and the last tap of each laid on each other, tap {tap} lies {SAME_TAP_MS} ms or more from its own"
        )),
        SameTaps::Shown => Ok((pairs, true)),
        SameTaps::NotShown => Ok((pairs, false)),
    }
}

/// When each tap of the pairs sounds in the stream and in the reference, in seconds of each by its own samples.
fn seconds(
    recording: &Recording<'_>,
    reference: &Reference<'_>,
    pairs: &[(usize, usize)],
) -> (Vec<f64>, Vec<f64>) {
    let ours = pairs
        .iter()
        .map(|&(ours, _)| seconds_of(ours, recording.stream.rate));
    let theirs = pairs
        .iter()
        .map(|&(_, theirs)| seconds_of(theirs, reference.wav.rate));
    (ours.collect(), theirs.collect())
}

/// How much longer the stream takes from its first tap to its last than the reference, in parts per million.
fn spacing_ppm(
    recording: &Recording<'_>,
    reference: &Reference<'_>,
    pairs: &[(usize, usize)],
) -> Found<f64> {
    let (ours, theirs) = seconds(recording, reference, pairs);
    let ppm = ratio_ppm(&ours, &theirs);
    ppm.ok_or_else(|| "the two recordings hold under two taps to span".to_owned())
}

/// The arrival of each tap's SDU less the onset of the tap in the reference: the least, the median and the most, in nanoseconds.
fn behind_ns(
    recording: &Recording<'_>,
    reference: &Reference<'_>,
    pairs: &[(usize, usize)],
) -> Found<[i128; 3]> {
    let mut behind = Vec::new();
    for &(ours, theirs) in pairs {
        let arrival = recording.timeline.arrival_ns(ours);
        let arrival = arrival.ok_or("a tap lies outside the units table")?;
        let onset = reference.stamp_ns(theirs);
        let onset = onset.ok_or("the reference has no rate to place a tap by")?;
        behind.push(i128::from(arrival).saturating_sub(onset));
    }
    behind.sort_unstable();
    match (behind.first(), median(&behind), behind.last()) {
        (Some(&least), Some(middle), Some(&most)) => Ok([least, middle, most]),
        _ => Err("no tap was found in a tap window".to_owned()),
    }
}

/// Nanoseconds as milliseconds, signed.
fn ms_of(ns: i128) -> f64 {
    let size = to_f64(u64::try_from(ns.unsigned_abs()).unwrap_or(u64::MAX)) / NS_PER_MS;
    if ns < 0 { -size } else { size }
}

/// The clock ratio the taps give, and the arrival of each tap's SDU against its onset in the reference.
pub(super) fn tap_rows(
    recording: &Recording<'_>,
    reference: &Reference<'_>,
    windows: &[TapWindow],
) -> Vec<Row> {
    let (pairs, shown_the_same) = match tap_pairs(recording, reference, windows) {
        Ok(found) => found,
        Err(why) => {
            let refused = TAP_ROWS.iter();
            return refused
                .map(|&(key, unit)| Row::new(key, &NONE, unit, &why))
                .collect();
        }
    };
    let taps = pairs.len();
    let taps = if shown_the_same {
        format!("{taps} taps")
    } else {
        format!("{taps} taps, {TOO_FEW_TAPS}")
    };
    let ratio_note = format!(
        "how much longer the stream takes from its first tap to its last than the reference, each by its own samples; {taps}"
    );
    let behind_note = format!(
        "the arrival of the SDU a tap came in less the onset of the tap in the reference; {taps}; {RECORDER_CLOCK}"
    );
    let [least, middle, most] = match behind_ns(recording, reference, &pairs) {
        Ok(found) => found.map(|ns| Ok(ms_of(ns))),
        Err(why) => [Err(why.clone()), Err(why.clone()), Err(why)],
    };
    let found = [
        (spacing_ppm(recording, reference, &pairs), &ratio_note),
        (least, &behind_note),
        (middle, &behind_note),
        (most, &behind_note),
    ];
    let rows = TAP_ROWS.iter().zip(found);
    rows.map(|(&(key, unit), (found, note))| figure(key, found, 1, unit, note))
        .collect()
}

/// The head check of one start and where its mark lies in the reference; refused where the start is too short, or the reference does not hold all that is searched.
fn searched(
    segment: &[f32],
    rate: u32,
    reference: &Reference<'_>,
    start_ns: u64,
) -> Found<(HeadCheck, i64)> {
    if segment.len() < samples_in(HEAD_AND_BODY_MS, rate) {
        return Err(format!(
            "the start holds under the {HEAD_AND_BODY_MS} ms of a head and a body"
        ));
    }
    let span_ms = i64::from(SEARCH_SPAN_MS);
    let held = |around_ms: &i64| {
        let until_ms = around_ms
            .saturating_add(span_ms)
            .saturating_add(i64::from(HEAD_AND_BODY_MS));
        reference.holds(around_ms.saturating_sub(span_ms), until_ms)
    };
    let around_ms = reference.ms_at(start_ns).filter(held).ok_or_else(|| {
        format!(
            "the reference does not hold the {SEARCH_SPAN_MS} ms to either side of where the start mark puts the head and the body"
        )
    })?;
    let stretch = &reference.wav.samples;
    let check = head(segment, rate, stretch, reference.wav.rate, around_ms);
    Ok((check, around_ms))
}

/// The five rows of one start: where its head and its body place it, whether they agree, and the lead the body gives.
fn start_rows(key: &str, searched: &Found<(HeadCheck, i64)>) -> [Row; 5] {
    let check = searched.clone().map(|(check, _)| check);
    let placed = |part: &str, found: fn(HeadCheck) -> Option<Alignment>| {
        let nowhere = || {
            format!(
                "the reference holds no match of the {part} within {SEARCH_SPAN_MS} ms of where the start mark puts it"
            )
        };
        check
            .clone()
            .and_then(|check| found(check).ok_or_else(nowhere))
    };
    let head_at = placed("first second", |check| check.head);
    let body_at = placed("three seconds after the first", |check| check.body);
    let offset_row = |name: &str, at: &Found<Alignment>| {
        let key = format!("{key}_{name}");
        match at {
            Ok(found) => {
                let score = found.score;
                let note = format!(
                    "where it places the first sample of the start, from the first sample of the reference; score {score:.2}"
                );
                Row::new(&key, &found.offset_ms, MS, &note)
            }
            Err(why) => Row::new(&key, &NONE, MS, why),
        }
    };
    let both = head_at
        .clone()
        .and_then(|head| Ok((head, body_at.clone()?)));
    let agree = both.map(|(head, body)| answer(head.agrees_with(body)));
    let silent = || "the first second or the three after it are digital silence".to_owned();
    let louder = check.and_then(|check| check.head_minus_body_db.ok_or_else(silent));
    let lead_ms = searched
        .clone()
        .and_then(|(_, around_ms)| Ok(around_ms.saturating_sub(body_at.clone()?.offset_ms)));
    let agree_note = format!(
        "whether the two lie within {AGREE_WITHIN_MS} ms of each other; no clock is read for it"
    );
    let lead_note = format!(
        "the start mark less the first sample of the start as the three seconds place it; {RECORDER_CLOCK}"
    );
    [
        offset_row("head_offset_ms", &head_at),
        offset_row("body_offset_ms", &body_at),
        counted(
            &format!("{key}_head_body_agree"),
            agree,
            ANSWER,
            &agree_note,
        ),
        figure(
            &format!("{key}_head_minus_body_db"),
            louder,
            2,
            DB,
            "the level of the first second less that of the three after it",
        ),
        counted(&format!("{key}_acoustic_lead_ms"), lead_ms, MS, &lead_note),
    ]
}

/// For each start of a run whose starts fall inside speech: its head against its body.
pub(super) fn head_rows(recording: &Recording<'_>, reference: &Reference<'_>) -> Vec<Row> {
    let rate = recording.stream.rate;
    let mut rows = Vec::new();
    for (index, &start) in recording.starts.iter().enumerate() {
        let segment = recording.stretch(start).1;
        let key = format!("start_{}", index.saturating_add(1));
        let searched = searched(segment, rate, reference, start.start_ns);
        rows.extend(start_rows(&key, &searched));
    }
    rows
}
