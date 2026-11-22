//! The rows of a decoded file measured with no cue read: the whole of it as one window, then each session of the dictation audio.

use super::super::super::analyse::report::Row;
use super::super::super::wav::Wav;
use super::super::levels::{NoLevel, active, floor_stretch};
use super::super::spectrum::{cutoff, cutoff_note, grid, reach, reach_note};
use super::{HZ, SAMPLES, counted, level_rows, near_zero_row, said, stream_rows};

/// What the keys of the rows of the whole file start with.
const WHOLE: &str = "whole";

/// How much of a stretch is near zero, the level of its active frames, its floor, and how far those frames reach by themselves and over its quietest three seconds.
fn stretch_rows(key: &str, samples: &[f32], rate: u32) -> Vec<Row> {
    let (speech, _floor, levels) = level_rows(key, samples, rate);
    let ranges = active(samples, rate).into_iter();
    let spoken: Vec<&[f32]> = ranges.filter_map(|range| samples.get(range)).collect();
    // Where the level of the active frames is refused, as for a room, their spectrum is refused for the same reason.
    let loud = speech.map(|_| grid(&spoken, rate));
    let fallen = loud.clone().and_then(|loud| {
        cutoff(&loud).map_err(|why| format!("the active frames have no cutoff: {why}"))
    });
    let quiet = said(floor_stretch(samples, rate).ok_or(NoLevel::NoFloor));
    let reached = loud.and_then(|loud| {
        let quiet = grid(&[quiet?], rate);
        reach(&loud, &quiet).map_err(|why| why.to_string())
    });
    let mut rows = vec![near_zero_row(key, samples)];
    rows.extend(levels);
    rows.extend([
        counted(
            &format!("{key}_cutoff"),
            fallen,
            HZ,
            &format!("{}; of the active frames alone", cutoff_note()),
        ),
        counted(
            &format!("{key}_reach"),
            reached,
            HZ,
            &reach_note(
                "what the active frames hold",
                "the quietest stretch, which the floor is read from",
            ),
        ),
    ]);
    rows
}

/// The table of a file measured whole: what it says of itself, the whole of it, then each of `sessions` by its number and its samples.
pub(crate) fn whole_rows(stream: &Wav, sessions: &[(u32, &[f32])]) -> Vec<Row> {
    let mut rows = Vec::from(stream_rows(stream));
    rows.extend(stretch_rows(WHOLE, &stream.samples, stream.rate));
    for &(session, samples) in sessions {
        let key = format!("session_{session}");
        let held = "every frame the frames table puts in the session";
        rows.push(Row::new(
            &format!("{key}_samples"),
            &samples.len(),
            SAMPLES,
            held,
        ));
        rows.extend(stretch_rows(&key, samples, stream.rate));
    }
    rows
}
