//! The librepods binary: the daemon, and the command line client under either of its names.

mod cli;
mod client;
mod daemon;
mod ipc_path;
mod protocol;

use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cli::{Invocation, ParseError, ParseErrorKind};

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
        Ok(Invocation::StateDir) => print_state_dir(),
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
    let runtime_dir = env::var_os(RUNTIME_DIR_VAR);
    let (Some(socket), Some(runtime_dir)) = (
        ipc_path::socket_path(runtime_dir.as_deref()),
        runtime_dir.clone().map(PathBuf::from),
    ) else {
        daemon::log(daemon::NO_RUNTIME_DIR);
        return ExitCode::FAILURE;
    };
    let Some(state_file) = state_file() else {
        daemon::log(daemon::NO_STATE_HOME);
        return ExitCode::FAILURE;
    };
    let old_conf = daemon::prefs::old_conf_path(
        env::var_os(CONFIG_HOME_VAR).as_deref(),
        env::var_os(HOME_VAR).as_deref(),
    );
    daemon::run(daemon::Paths {
        socket,
        state_file,
        old_conf,
        runtime_dir,
    })
}

fn state_file() -> Option<PathBuf> {
    daemon::state_file::path(
        env::var_os(STATE_HOME_VAR).as_deref(),
        env::var_os(HOME_VAR).as_deref(),
    )
}

/// Prints the directory the daemon keeps its files in, so the installer's uninstall never guesses it.
fn print_state_dir() -> ExitCode {
    let Some(dir) = state_file().and_then(|file| file.parent().map(Path::to_path_buf)) else {
        daemon::log(daemon::NO_STATE_HOME);
        return ExitCode::FAILURE;
    };
    let mut out = io::stdout().lock();
    match out
        .write_all(dir.as_os_str().as_bytes())
        .and_then(|()| out.write_all(b"\n"))
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            daemon::log(&format!("cannot print the state directory: {error}"));
            ExitCode::FAILURE
        }
    }
}

fn report_parse_error(error: &ParseError) -> ExitCode {
    let message = error.message().as_bytes();
    // A failed write to the terminal has nowhere left to be reported.
    match error.kind() {
        ParseErrorKind::DisplayHelp | ParseErrorKind::DisplayVersion => {
            drop(io::stdout().lock().write_all(message));
            ExitCode::SUCCESS
        }
        ParseErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        | ParseErrorKind::InvalidSubcommand
        | ParseErrorKind::MissingRequiredArgument
        | ParseErrorKind::UnknownArgument
        | ParseErrorKind::InvalidUtf8 => {
            drop(io::stderr().lock().write_all(message));
            ExitCode::from(2)
        }
    }
}
