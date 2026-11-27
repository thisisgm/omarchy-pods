//! A run in a store under a temporary `HOME`, and the probe commands of the built binary on it.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use pods_proto::sdulog::{Record, format_line};
use tempfile::TempDir;

/// A stamp of the shape `date -u +%Y%m%dT%H%M%SZ` prints.
pub(crate) const STAMP: &str = "20261120T120000Z";
/// The clock of the generated peer where a test needs only one.
pub(crate) const RATE_HZ: u32 = 64_000;
/// The samples one unit is assumed to hold, which is also the step of the generated stamps.
pub(crate) const SAMPLES_PER_UNIT: u32 = 480;
/// The units in every generated SDU.
pub(crate) const UNITS_PER_SDU: u32 = 4;
/// The mode the store gives its directories.
const OWNER_DIR: u32 = 0o700;
/// The mode the store gives its files.
pub(crate) const OWNER_FILE: u32 = 0o600;
pub(crate) const PERMISSION_BITS: u32 = 0o777;
/// A device information packet with two made up strings.
pub(crate) const DEVICE_INFO: &[u8] =
    b"\x04\x00\x04\x00\x1d\x00\x02\xed\x00\x04\x00Probe Pods\x00SERIAL0XYZ\x00";
/// A word of each made up string, which no output but the device file may show.
pub(crate) const PRIVATE: [&str; 2] = ["Probe", "SERIAL0XYZ"];

pub(crate) fn log_text(records: &[Record]) -> String {
    records
        .iter()
        .map(|record| format_line(record.at_ns, record.direction, &record.sdu).unwrap() + "\n")
        .collect()
}

pub(crate) fn run_path(home: &TempDir, name: &str) -> PathBuf {
    home.path().join("pods-captures").join(STAMP).join(name)
}

/// A new file in `dir` with the mode the store gives its files.
pub(crate) fn put_file(dir: &Path, name: &str, contents: &[u8]) {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(OWNER_FILE)
        .open(dir.join(name))
        .unwrap()
        .write_all(contents)
        .unwrap();
}

/// A run directory under `home` shaped and moded as the store makes one, holding `files`.
pub(crate) fn run_holding(home: &TempDir, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
    let run = run_path(home, name);
    DirBuilder::new()
        .recursive(true)
        .mode(OWNER_DIR)
        .create(&run)
        .unwrap();
    for (file, contents) in files {
        put_file(&run, file, contents);
    }
    run
}

/// One probe command of the built binary on a run, with `HOME` set to the temporary directory, never the real one.
pub(crate) fn probe(home: &TempDir, command: &str, run: &Path) -> Command {
    let mut probe = Command::new(env!("CARGO_BIN_EXE_xtask"));
    probe.args(["probe", command]).arg(run);
    probe.env("HOME", home.path());
    probe
}

// Sample input: "key\tvalue\tunit\tnote\nclock_hz\trefused\tHz\tstart 1\n", from which `clock_hz` gives `refused` and its note for every start.
pub(crate) fn rows<'a>(table: &'a str, key: &str) -> Vec<(&'a str, &'a str)> {
    let found = table.lines().filter_map(|line| {
        let fields: Vec<&str> = line.split('\t').collect();
        (fields[0] == key).then(|| (fields[1], fields[3]))
    });
    found.collect()
}

/// The one row `key` has: its value and its note.
pub(crate) fn row<'a>(table: &'a str, key: &str) -> (&'a str, &'a str) {
    let found = rows(table, key);
    assert_eq!(found.len(), 1, "{key} has {} rows", found.len());
    found[0]
}

pub(crate) fn names_in(run: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(run)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}
