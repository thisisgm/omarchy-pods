//! The rows of a decoded file measured with no cue read: the whole of it as one window, then each session of the dictation audio.

use super::super::super::analyse::report::Row;
use super::super::super::wav::Wav;
use super::super::levels::{NoLevel, active, floor_stretch};
use super::super::spectrum::{band_edge, edge_note, grid, reach, reach_note};
use super::{HZ, SAMPLES, counted, level_rows, near_zero_row, said, stream_rows};

/// What the keys of the rows of the whole file start with.
const WHOLE: &str = "whole";

/// The grid points at or under the band edge, which a grid lists lowest first; all of them where there is no edge.
fn up_to(grid: &[(u32, f64)], edge: Option<u32>) -> &[(u32, f64)] {
    let Some(edge) = edge else {
        return grid;
    };
    grid.get(..grid.partition_point(|&(hz, _)| hz <= edge))
        .unwrap_or(&[])
}

/// How much of a stretch is near zero, the level of its active frames, its floor, where its band ends, and how far under that edge those frames reach over its quietest three seconds.
fn stretch_rows(key: &str, samples: &[f32], rate: u32) -> Vec<Row> {
    let (speech, _floor, levels) = level_rows(key, samples, rate);
    // Where the level of the active frames is refused, as for a room, the two spectra are refused for the same reason.
    let edge = speech.clone().and_then(|_| {
        band_edge(samples, rate).map_err(|why| format!("the stretch has no band edge: {why}"))
    });
    let ranges = active(samples, rate).into_iter();
    let spoken: Vec<&[f32]> = ranges.filter_map(|range| samples.get(range)).collect();
    let quiet = said(floor_stretch(samples, rate).ok_or(NoLevel::NoFloor));
    // Over the edge both spectra hold only what leaks from under it, in the proportion of the two levels, so nothing is read there.
    let top = edge.as_ref().ok().copied();
    let reached = speech.and_then(|_| {
        let (loud, quiet) = (grid(&spoken, rate), grid(&[quiet?], rate));
        reach(up_to(&loud, top), up_to(&quiet, top)).map_err(|why| why.to_string())
    });
    let mut rows = vec![near_zero_row(key, samples)];
    rows.extend(levels);
    rows.extend([
        counted(&format!("{key}_band_edge"), edge, HZ, &edge_note()),
        counted(
            &format!("{key}_reach"),
            reached,
            HZ,
            &format!(
                "{}; no point over the band edge is read, where there is one",
                reach_note(
                    "what the active frames hold",
                    "the quietest stretch, which the floor is read from",
                )
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
