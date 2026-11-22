//! Generated capture logs in runs of a store under a temporary `HOME`.

mod run;

use std::path::PathBuf;

use pods_proto::sdulog::Record;
use tempfile::TempDir;

pub(crate) use run::{
    DEVICE_INFO, OWNER_FILE, PERMISSION_BITS, PRIVATE, RATE_HZ, SAMPLES_PER_UNIT, UNITS_PER_SDU,
    log_text, names_in, probe, row, run_path,
};

use super::synth::synth;

/// One second over the 30 s a start needs for its rates, so the short run still gets a whole table.
pub(crate) const SHORT_RUN_S: u32 = 31;
/// Each arrival is late by a seeded delay under this, far below the 30 ms between two SDUs.
const JITTER_MS: u32 = 2;
/// The seed of every generated log.
const SEED: u64 = 0x5eed_0006;
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

/// A run directory under `home` shaped and moded as the store makes one, holding the three input files.
pub(crate) fn make_run(home: &TempDir, name: &str, log: &str) -> PathBuf {
    make_run_of(home, name, &[("sdu.log", log), ("marks.tsv", MARKS)])
}

/// A run directory as `make_run` makes one, holding `files` and the meta file of a soak.
pub(crate) fn make_run_of(home: &TempDir, name: &str, files: &[(&str, &str)]) -> PathBuf {
    let files = files.iter().copied().chain([("meta.txt", META)]);
    let files: Vec<(&str, &[u8])> = files
        .map(|(file, contents)| (file, contents.as_bytes()))
        .collect();
    run::run_holding(home, name, &files)
}
