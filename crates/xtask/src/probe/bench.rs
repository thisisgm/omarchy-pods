//! The objective audio bench: scorers and codec libraries pinned in a cache outside the repository, and the tables they give.

mod cache;
mod codec;
mod pins;
mod visqol;

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus, Stdio};

use clap::Subcommand;
use rustix::fs::{Mode, OFlags};

use super::print_table;
use crate::report;

#[derive(Debug, Subcommand)]
pub(crate) enum BenchCommand {
    /// Makes the cache in a directory that does not exist yet: downloads the pinned sources and music, checks each SHA-256, builds the two codec libraries and installs the scorer.
    Setup {
        /// The directory to create, outside the repository.
        #[arg(long)]
        cache: PathBuf,
    },
    /// Passes each reference excerpt through SBC-XQ and AAC as `PipeWire` sets them, scores each decode with `ViSQOL` in audio mode, and writes the table.
    Codec(codec::CodecArgs),
}

/// Why a tool of the bench gave nothing; what it said went to standard error as it ran.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ToolError {
    #[error("could not start {program}: {source}")]
    Start { program: String, source: io::Error },
    #[error("{program} ended with {status}")]
    Failed { program: String, status: ExitStatus },
}

/// Runs one tool to its end with its standard error on ours, and gives what it printed.
fn output_of(command: &mut Command) -> Result<Vec<u8>, ToolError> {
    let program = command.get_program().display().to_string();
    let ran = command.stdin(Stdio::null()).stderr(Stdio::inherit());
    match ran.output() {
        Ok(output) if output.status.success() => Ok(output.stdout),
        Ok(output) => Err(ToolError::Failed {
            program,
            status: output.status,
        }),
        Err(source) => Err(ToolError::Start { program, source }),
    }
}

/// Writes `text` to a new file of mode 0600; an existing name, a symbolic link included, is refused and never followed.
fn write_new(path: &Path, text: &str) -> io::Result<()> {
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC;
    let file = rustix::fs::open(path, flags, Mode::RUSR | Mode::WUSR)?;
    File::from(file).write_all(text.as_bytes())
}

pub(crate) fn run(command: &BenchCommand) -> ExitCode {
    match command {
        BenchCommand::Setup { cache } => match cache::setup(cache) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                report(&format!("xtask probe: {error}"));
                ExitCode::FAILURE
            }
        },
        BenchCommand::Codec(args) => print_table(codec::run(args)),
    }
}
