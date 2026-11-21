//! The librepods binary. Today it is the command line client of the C++ daemon, under either of its names.

mod cli;
mod client;
mod ipc_path;

use std::env;
use std::ffi::OsString;
use std::io;
use std::process::ExitCode;

use cli::Invocation;

/// The variable that names the per-user runtime directory holding the daemon socket.
const RUNTIME_DIR_VAR: &str = "XDG_RUNTIME_DIR";

fn main() -> ExitCode {
    let mut command_line = env::args_os();
    let argv0 = command_line.next().unwrap_or_default();
    let args: Vec<OsString> = command_line.collect();
    match cli::parse(&argv0, &args) {
        Ok(Invocation::LegacyClient { verb }) => run_client(&verb),
        Ok(Invocation::LegacyUsage { program }) => client::usage(program, &mut io::stderr().lock()),
        Err(error) => report_parse_error(&error),
    }
}

/// Runs the client against the socket under the session's runtime directory.
fn run_client(verb: &str) -> ExitCode {
    let mut err = io::stderr().lock();
    let Some(socket) = ipc_path::socket_path(env::var_os(RUNTIME_DIR_VAR).as_deref()) else {
        return client::no_runtime_dir(&mut err);
    };
    client::run_legacy(verb, &socket, &mut io::stdout().lock(), &mut err)
}

/// Prints a clap error, help or version where clap chooses, and exits with clap's code.
fn report_parse_error(error: &clap::Error) -> ExitCode {
    // A failed write to the terminal has nowhere left to be reported.
    drop(error.print());
    u8::try_from(error.exit_code()).map_or(ExitCode::FAILURE, ExitCode::from)
}
