//! Microphone probe commands. `init` creates the private session directory that captures go into.

mod store;

use std::env;
use std::ffi::OsString;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::{print_line, report};
use store::Session;

/// Commands that prepare and record a microphone probe session.
#[derive(Debug, Parser)]
#[command(name = "probe", bin_name = "cargo xtask probe")]
struct Probe {
    #[command(subcommand)]
    command: ProbeCommand,
}

#[derive(Debug, Subcommand)]
enum ProbeCommand {
    /// Creates or checks the session directory under $HOME/omapods-captures and prints its path.
    Init {
        /// The session stamp, from date -u +%Y%m%dT%H%M%SZ.
        #[arg(long)]
        stamp: String,
    },
}

/// Runs one probe command; `args` starts with the word `probe`.
pub(crate) fn run(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    match Probe::try_parse_from(args) {
        Ok(Probe {
            command: ProbeCommand::Init { stamp },
        }) => init(&stamp),
        Err(error) => usage(&error),
    }
}

/// Opens the session for `stamp` and prints its path, which is all init writes on stdout.
fn init(stamp: &str) -> ExitCode {
    let Some(home) = env::var_os("HOME") else {
        report("xtask probe: HOME is not set, and the store lives under it");
        return ExitCode::FAILURE;
    };
    // Every path printed below is built from HOME, and a lossy print would name a directory that does not exist.
    if home.to_str().is_none() {
        report(
            "xtask probe: HOME is not valid UTF-8, so the session path could not be printed as it is on disk",
        );
        return ExitCode::FAILURE;
    }
    match Session::open(&store::root(&home), stamp) {
        Ok(session) => {
            print_line(&session.path().display().to_string());
            ExitCode::SUCCESS
        }
        Err(error) => {
            report(&format!("xtask probe: {error}"));
            ExitCode::FAILURE
        }
    }
}

/// Help goes to stdout and succeeds; a parse error goes to stderr and fails.
fn usage(error: &clap::Error) -> ExitCode {
    let text = error.render().to_string();
    if error.use_stderr() {
        report(text.trim_end());
        ExitCode::FAILURE
    } else {
        print_line(text.trim_end());
        ExitCode::SUCCESS
    }
}
