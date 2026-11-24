//! The codec bench with its real pinned tools, in a cache it makes and removes: it does its work only when Cargo is asked for a bench, which of the gates only `cargo xtask ci bench` does.

#[path = "codec/check.rs"]
mod check;

use std::env;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use check::{check, when_asked};

/// Each excerpt goes through each codec twice, which is what shows a score that moves between runs.
const RUNS: &str = "2";

fn bench(command: &str, cache: &Path) -> Command {
    let mut bench = Command::new(env!("CARGO_BIN_EXE_xtask"));
    bench
        .args(["probe", "bench", command, "--cache"])
        .arg(cache);
    bench.stdin(Stdio::null()).stderr(Stdio::inherit());
    bench
}

fn run() -> Result<(), String> {
    let said = |error: io::Error| error.to_string();
    let work = tempfile::tempdir().map_err(said)?;
    let cache = work.path().join("cache");
    let made = bench("setup", &cache).status().map_err(said)?;
    if !made.success() {
        return Err(format!("setup ended with {made}"));
    }
    let mut codec = bench("codec", &cache);
    codec.args(["--runs", RUNS, "--out"]);
    let scored = codec
        .arg(work.path().join("codec.tsv"))
        .output()
        .map_err(said)?;
    if !scored.status.success() {
        return Err(format!("codec ended with {}", scored.status));
    }
    io::stdout().write_all(&scored.stdout).map_err(said)?;
    check(&String::from_utf8_lossy(&scored.stdout))
}

fn main() -> ExitCode {
    let (said, code) = match when_asked(env::args_os(), run) {
        Ok(true) => return ExitCode::SUCCESS,
        Ok(false) => (
            "skipped, Cargo was not asked for a bench".to_owned(),
            ExitCode::SUCCESS,
        ),
        Err(error) => (error, ExitCode::FAILURE),
    };
    // On standard error, since a test runner reads its list of tests from standard output; one that cannot be written leaves nowhere to say so.
    let _unreported = writeln!(io::stderr(), "codec bench: {said}");
    code
}
