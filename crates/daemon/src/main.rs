//! The librepods binary: the daemon, and the command line client under either of its names.

mod cli;
mod client;
mod daemon;
mod ipc_path;
mod protocol;

use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use cli::Invocation;

const RUNTIME_DIR_VAR: &str = "XDG_RUNTIME_DIR";

const STATE_HOME_VAR: &str = "XDG_STATE_HOME";

const CONFIG_HOME_VAR: &str = "XDG_CONFIG_HOME";

const HOME_VAR: &str = "HOME";

fn main() -> ExitCode {
    let mut command_line = env::args_os();
    let argv0 = command_line.next().unwrap_or_default();
    let args: Vec<OsString> = command_line.collect();
    match cli::parse(&argv0, &args) {
        Ok(Invocation::LegacyClient { verb }) => {
            run_client(|socket, out, err| client::run_legacy(&verb, socket, out, err))
        }
        Ok(Invocation::LegacyUsage { program }) => client::usage(program, &mut io::stderr().lock()),
        Ok(Invocation::Daemon) => run_daemon(),
        Ok(Invocation::Status) => run_client(client::proto1::status),
        Ok(Invocation::Watch) => run_client(client::proto1::watch),
        Ok(Invocation::Set { key, value }) => {
            run_client(|socket, _out, err| client::proto1::set(&key, &value, socket, err))
        }
        Ok(Invocation::Do { action }) => {
            run_client(|socket, _out, err| client::proto1::act(&action, socket, err))
        }
        Err(error) => report_parse_error(&error),
    }
}

fn run_client(command: impl FnOnce(&Path, &mut dyn Write, &mut dyn Write) -> ExitCode) -> ExitCode {
    let mut err = io::stderr().lock();
    let Some(socket) = ipc_path::socket_path(env::var_os(RUNTIME_DIR_VAR).as_deref()) else {
        return client::no_runtime_dir(&mut err);
    };
    command(&socket, &mut io::stdout().lock(), &mut err)
}

fn run_daemon() -> ExitCode {
    let Some(socket) = ipc_path::socket_path(env::var_os(RUNTIME_DIR_VAR).as_deref()) else {
        daemon::log(daemon::NO_RUNTIME_DIR);
        return ExitCode::FAILURE;
    };
    let state_home = env::var_os(STATE_HOME_VAR);
    let home = env::var_os(HOME_VAR);
    let Some(state_file) = daemon::state_file::path(state_home.as_deref(), home.as_deref()) else {
        daemon::log(daemon::NO_STATE_HOME);
        return ExitCode::FAILURE;
    };
    let old_conf =
        daemon::prefs::old_conf_path(env::var_os(CONFIG_HOME_VAR).as_deref(), home.as_deref());
    daemon::run(daemon::Paths {
        socket,
        state_file,
        old_conf,
    })
}

/// Prints a clap error, help or version where clap chooses, and exits with clap's code.
fn report_parse_error(error: &clap::Error) -> ExitCode {
    // A failed write to the terminal has nowhere left to be reported.
    drop(error.print());
    u8::try_from(error.exit_code()).map_or(ExitCode::FAILURE, ExitCode::from)
}
