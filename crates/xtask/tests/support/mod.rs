//! Runs in a store under a temporary `HOME`, and the probe commands of the built binary on them.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use pods_proto::sdulog::{Record, format_line};
use tempfile::TempDir;

use super::synth::synth;

/// A stamp of the shape `date -u +%Y%m%dT%H%M%SZ` prints.
const STAMP: &str = "20261120T120000Z";
/// The mode the store gives its directories.
const OWNER_DIR: u32 = 0o700;
/// The mode the store gives its files.
pub(crate) const OWNER_FILE: u32 = 0o600;
/// The permission bits of a mode, without the file type.
pub(crate) const PERMISSION_BITS: u32 = 0o777;
/// The clock of the generated peer where a test needs only one.
pub(crate) const RATE_HZ: u32 = 64_000;
/// The samples one unit is assumed to hold, which is also the step of the generated stamps.
pub(crate) const SAMPLES_PER_UNIT: u32 = 480;
/// The units in every generated SDU.
pub(crate) const UNITS_PER_SDU: u32 = 4;
/// One second over the 30 s a start needs for its rates, so the short run still gets a whole table.
pub(crate) const SHORT_RUN_S: u32 = 31;
/// Each arrival is late by a seeded delay under this, far below the 30 ms between two SDUs.
const JITTER_MS: u32 = 2;
/// The seed of every generated log.
const SEED: u64 = 0x5eed_0006;
/// A device information packet with two made up strings.
pub(crate) const DEVICE_INFO: &[u8] =
    b"\x04\x00\x04\x00\x1d\x00\x02\xed\x00\x04\x00Probe Pods\x00SERIAL0XYZ\x00";
/// A word of each made up string, which no output but the device file may show.
pub(crate) const PRIVATE: [&str; 2] = ["Probe", "SERIAL0XYZ"];
/// A run that starts at zero and stops after 601 seconds.
const MARKS: &str =
    "0\tconnect\t\n0\tstart\t\n601000000000\tstop\t\n602000000000\tend\tscript done\n";
/// The meta file of a soak.
const META: &str = "script=soak\ntool=xtask 0.0.0\nrealtime_at_zero_ns=1763640000000000000\n";

/// Silent audio SDUs of `seconds` from a peer clocked at `rate_hz`, four units of 480 samples in each.
pub(crate) fn soak(rate_hz: u32, seconds: u32) -> Vec<Record> {
    synth(
        rate_hz,
        SAMPLES_PER_UNIT,
        UNITS_PER_SDU,
        seconds,
        JITTER_MS,
        SEED,
    )
}

pub(crate) fn log_text(records: &[Record]) -> String {
    records
        .iter()
        .map(|record| format_line(record.at_ns, record.direction, &record.sdu).unwrap() + "\n")
        .collect()
}

/// Where the store under `home` keeps the run `name`.
pub(crate) fn run_path(home: &TempDir, name: &str) -> PathBuf {
    home.path().join("omapods-captures").join(STAMP).join(name)
}

/// A run directory under `home` shaped and moded as the store makes one, holding the three input files.
pub(crate) fn make_run(home: &TempDir, name: &str, log: &str) -> PathBuf {
    let run = run_path(home, name);
    DirBuilder::new()
        .recursive(true)
        .mode(OWNER_DIR)
        .create(&run)
        .unwrap();
    for (file, contents) in [("sdu.log", log), ("marks.tsv", MARKS), ("meta.txt", META)] {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(OWNER_FILE)
            .open(run.join(file))
            .unwrap()
            .write_all(contents.as_bytes())
            .unwrap();
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

/// The value and the note of the one row with this key.
// Sample input: `max_lsb\t0.000\tLSB\tlargest |candidate - oracle| x 32768 at that offset`, a row's key, value, unit and note.
pub(crate) fn row<'a>(table: &'a str, key: &str) -> (&'a str, &'a str) {
    let mut found = table.lines().filter_map(|line| {
        let fields: Vec<&str> = line.split('\t').collect();
        (fields[0] == key).then(|| (fields[1], fields[3]))
    });
    let row = found.next().unwrap();
    assert_eq!(found.next(), None, "{key} has more than one row");
    row
}

pub(crate) fn names_in(run: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(run)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}
