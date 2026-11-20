//! The librepods daemon. This first version only answers `--version`.

use std::process::ExitCode;

use clap::Parser;

/// Command line of the librepods daemon.
#[derive(Debug, Parser)]
#[command(version)]
struct Cli {}

fn main() -> ExitCode {
    let _cli = Cli::parse();
    ExitCode::SUCCESS
}
