//! Structural analysis of one capture run: the shape of the stream, with no audio decoded.

pub(crate) mod att;
#[cfg(test)]
mod backlog;
mod bands;
mod census;
pub(crate) mod coarse;
pub(crate) mod input;
mod lead;
pub(crate) mod load;
mod rates;
pub(crate) mod report;
mod shape;
pub(crate) mod stats;
#[cfg(test)]
pub(crate) mod support;
#[cfg(test)]
mod synth;

use std::io;
use std::path::Path;

use pods_proto::sdulog::Record;

use super::store::{MAX_READ_BYTES, Run, StoreError, Unnamed};
// The generator of logs names it as its neighbour, here and in the test of the built command.
#[cfg(test)]
use super::xorshift;
use input::{
    ATT_LOG_FILE, CutLines, InputError, LOG_FILE, MARKS_FILE, META_FILE, parse_lines, parse_marks,
    parse_meta, text_of, whole_lines,
};

/// The table of figures, which the command prints first.
pub(crate) const ANALYSIS_FILE: &str = "analysis.tsv";
/// One line for every unit of every audio SDU.
pub(crate) const UNITS_FILE: &str = "units.tsv";
/// The strings of the device information packet; they hold serial numbers and stay in the run.
pub(crate) const DEVICE_INFO_FILE: &str = "device-info.txt";
/// The packet log is read up to 256 MiB: two hours of 354 byte SDUs, 34 a second, written in hex, come to about 170 MiB.
pub(crate) const MAX_LOG_BYTES: u64 = 256 * 1024 * 1024;

/// Why a run was not analysed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AnalyseError {
    #[error(transparent)]
    Input(#[from] InputError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Analyses the run at `run_dir`, writes its tables into the run and returns what the command prints: the analysis table, then the one of the attribute channel when the run holds its log.
pub(crate) fn analyse_run(run_dir: &Path) -> Result<String, AnalyseError> {
    let run = Run::open(run_dir)?;
    // All are read before anything is parsed, so a run with a file missing is refused as such.
    let log = run.read(LOG_FILE, MAX_LOG_BYTES)?;
    let att_log = read_if_held(&run, ATT_LOG_FILE)?;
    let marks = run.read(MARKS_FILE, MAX_READ_BYTES)?;
    let meta = run.read(META_FILE, MAX_READ_BYTES)?;
    // Cut as bytes, before any is read as text: a line cut inside a character is not text and is left out like any other.
    let (records, log_cut) = whole_records(LOG_FILE, &log)?;
    let att = att_log
        .as_deref()
        .map(|log| whole_records(ATT_LOG_FILE, log));
    let att = att.transpose()?;
    let (marks, marks_cut) = whole_lines(&marks);
    let marks = parse_marks(text_of(MARKS_FILE, marks)?)?;
    let meta = parse_meta(text_of(META_FILE, &meta)?)?;
    let cut = CutLines {
        log: log_cut,
        marks: marks_cut,
    };
    match &att {
        None if records.is_empty() => return Err(InputError::EmptyLog.into()),
        Some((att_records, _)) if records.is_empty() && att_records.is_empty() => {
            return Err(InputError::EmptyLogs.into());
        }
        None | Some(_) => {}
    }

    let report = report::analyse(&records, &marks, &meta.script, cut);
    let table = report::table(&report.rows);
    let device_info = report.device_strings.as_ref().map(|strings| {
        let lines: Vec<String> = strings.iter().map(|string| format!("{string}\n")).collect();
        lines.concat()
    });
    let att = att.map(|(att_records, att_cut)| att::analyse(&att_records, &marks, att_cut));
    let att_table = att.as_ref().map(|att| report::table(&att.rows));
    // The analysis table goes first: a run analysed before refuses here, before any other file is made.
    let mut files = vec![(ANALYSIS_FILE, table.as_str()), (UNITS_FILE, &report.units)];
    if let Some(device_info) = &device_info {
        files.push((DEVICE_INFO_FILE, device_info));
    }
    if let (Some(att), Some(att_table)) = (&att, &att_table) {
        files.push((att::ANALYSIS_FILE, att_table));
        files.push((att::FRAMES_FILE, &att.frames));
        files.push((att::HANDLES_FILE, &att.handles));
    }
    write_new(&run, &files)?;
    Ok(table + att_table.as_deref().unwrap_or_default())
}

/// A log of the run that only some runs hold: `None` when the run has no file of that name.
fn read_if_held(run: &Run, file: &str) -> Result<Option<Vec<u8>>, StoreError> {
    match run.read(file, MAX_LOG_BYTES) {
        Err(StoreError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        read => read.map(Some),
    }
}

/// The packets of the whole lines of a log, and whether its last line came without a newline and was left out.
fn whole_records(file: &'static str, log: &[u8]) -> Result<(Vec<Record>, bool), InputError> {
    let (whole, cut) = whole_lines(log);
    Ok((parse_lines(file, text_of(file, whole)?)?, cut))
}

/// Writes every file whole before any gets its name and gives all the names or none, so a failure leaves the run as it was.
pub(crate) fn write_new(run: &Run, files: &[(&str, &str)]) -> Result<(), StoreError> {
    let mut whole = Vec::new();
    for &(file, contents) in files {
        whole.push(run.write_unnamed(file, contents.as_bytes())?);
    }
    Unnamed::name_all(&whole)
}

#[cfg(test)]
mod tests;
