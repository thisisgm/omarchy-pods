//! The daemon: it owns the state, serves the control socket in the C++ daemon's dialect and publishes the state file.

mod hub;
mod legacy_dialect;
mod signals;
mod socket;
pub(crate) mod state_file;

use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use pods_core::status::to_status_line;
use tokio::runtime::Builder;

use hub::Hub;
use state_file::StateFile;

/// What `daemon/main.cpp` logs when the session has no runtime directory.
pub(crate) const NO_RUNTIME_DIR: &str = "XDG_RUNTIME_DIR is unset; cannot place the control socket";

/// What the daemon logs when neither variable can place the state file.
pub(crate) const NO_STATE_HOME: &str =
    "XDG_STATE_HOME is not an absolute path and HOME is unset; cannot place the state file";

/// Where one daemon serves and publishes.
#[derive(Debug)]
pub(crate) struct Paths {
    pub(crate) socket: PathBuf,
    pub(crate) state_file: PathBuf,
}

/// Runs the daemon until a stop signal, on a current thread runtime.
pub(crate) fn run(paths: Paths) -> ExitCode {
    match Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime.block_on(serve(paths)),
        Err(error) => {
            log(&format!("cannot start the daemon's async runtime: {error}"));
            ExitCode::FAILURE
        }
    }
}

/// Becomes the one daemon on the socket, publishes, serves, and cleans up on a stop signal.
async fn serve(paths: Paths) -> ExitCode {
    let mut stop = match signals::listen() {
        Ok(stop) => stop,
        Err(error) => {
            log(&error);
            return ExitCode::FAILURE;
        }
    };
    if socket::ask_running_daemon_to_reopen(&paths.socket).await {
        log(&format!(
            "a daemon already answers on {}; asked it to reopen, exiting",
            paths.socket.display()
        ));
        return ExitCode::SUCCESS;
    }
    let _lock = match socket::lock(&paths.socket) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            log(&format!(
                "another daemon is starting on {}; leaving the socket to it",
                paths.socket.display()
            ));
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            log(&error);
            return ExitCode::FAILURE;
        }
    };

    let hub = Hub::new();
    let file = StateFile::new(paths.state_file);
    let updates = hub.subscribe();
    // Before the socket exists, so a client that can connect always finds the state file.
    let first = to_status_line(&updates.borrow());
    if let Err(error) = file.publish_now(&first) {
        log(&error);
        return ExitCode::FAILURE;
    }
    let listener = match socket::bind(&paths.socket) {
        Ok(listener) => listener,
        Err(error) => {
            log(&error);
            remove_state_file(&file).await;
            return ExitCode::FAILURE;
        }
    };

    let accept = tokio::spawn(socket::accept_loop(listener, hub.clone()));
    let follow = tokio::spawn(state_file::follow(updates, file.clone()));
    let signal = stop.first().await;
    log(&format!("{signal} received, stopping"));
    accept.abort();
    follow.abort();
    match fs::remove_file(&paths.socket) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => log(&format!(
            "cannot remove the control socket {}: {error}",
            paths.socket.display()
        )),
    }
    remove_state_file(&file).await;
    ExitCode::SUCCESS
}

/// Removes the state file, which is how a watcher learns the daemon stopped.
async fn remove_state_file(file: &StateFile) {
    if let Err(error) = file.remove().await {
        log(&error);
    }
}

/// Writes one line to stderr, which the service manager keeps in the journal.
pub(crate) fn log(message: &str) {
    // A failed write to stderr has nowhere left to be reported.
    drop(writeln!(io::stderr().lock(), "{message}"));
}
