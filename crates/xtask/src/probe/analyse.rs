//! Structural analysis of one capture run: the shape of the stream, with no audio decoded.

mod bands;
mod census;
mod input;
mod lead;
mod load;
mod rates;
mod report;
mod shape;
mod stats;
#[cfg(test)]
mod support;
#[cfg(test)]
mod synth;

use std::path::Path;

use super::store::{MAX_READ_BYTES, Run, StoreError, Unnamed};
use input::{
    CutLines, InputError, LOG_FILE, MARKS_FILE, META_FILE, parse_log, parse_marks, parse_meta,
    text_of, whole_lines,
};

/// The table of figures, which is also what the command prints.
pub(crate) const ANALYSIS_FILE: &str = "analysis.tsv";
/// One line for every unit of every audio SDU.
pub(crate) const UNITS_FILE: &str = "units.tsv";
/// The strings of the device information packet; they hold serial numbers and stay in the run.
pub(crate) const DEVICE_INFO_FILE: &str = "device-info.txt";
/// The packet log is read up to 256 MiB: two hours of 354 byte SDUs, 34 a second, written in hex, come to about 170 MiB.
const MAX_LOG_BYTES: u64 = 256 * 1024 * 1024;

/// Why a run was not analysed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AnalyseError {
    #[error(transparent)]
    Input(#[from] InputError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Analyses the run at `run_dir`, writes its tables into the run and returns the analysis table.
pub(crate) fn analyse_run(run_dir: &Path) -> Result<String, AnalyseError> {
    let run = Run::open(run_dir)?;
    // All three are read before anything is parsed, so a run with a file missing is refused as such.
    let log = run.read(LOG_FILE, MAX_LOG_BYTES)?;
    let marks = run.read(MARKS_FILE, MAX_READ_BYTES)?;
    let meta = run.read(META_FILE, MAX_READ_BYTES)?;
    // Cut as bytes, before any is read as text: a line cut inside a character is not text and is left out like any other.
    let (log, log_cut) = whole_lines(&log);
    let (marks, marks_cut) = whole_lines(&marks);
    let records = parse_log(text_of(LOG_FILE, log)?)?;
    let marks = parse_marks(text_of(MARKS_FILE, marks)?)?;
    let meta = parse_meta(text_of(META_FILE, &meta)?)?;
    let cut = CutLines {
        log: log_cut,
        marks: marks_cut,
    };

    let report = report::analyse(&records, &marks, &meta.script, cut);
    let table = report.table();
    let device_info = report.device_strings.as_ref().map(|strings| {
        let lines: Vec<String> = strings.iter().map(|string| format!("{string}\n")).collect();
        lines.concat()
    });
    // The analysis table goes first: a run analysed before refuses here, before any other file is made.
    let mut files = vec![(ANALYSIS_FILE, table.as_str()), (UNITS_FILE, &report.units)];
    if let Some(device_info) = &device_info {
        files.push((DEVICE_INFO_FILE, device_info));
    }
    write_new(&run, &files)?;
    Ok(table)
}

/// Writes every file whole before any gets its name, so a write that fails leaves nothing in the run.
fn write_new(run: &Run, files: &[(&str, &str)]) -> Result<(), StoreError> {
    let mut whole = Vec::new();
    for &(file, contents) in files {
        whole.push(run.write_unnamed(file, contents.as_bytes())?);
    }
    whole.iter().try_for_each(Unnamed::give_name)
}

#[cfg(test)]
mod tests;
