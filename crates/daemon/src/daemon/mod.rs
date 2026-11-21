//! The daemon: it owns the state, serves the control socket in protocol 1 and the C++ daemon's dialect, publishes the state file and serves D-Bus.

mod dbus;
mod hub;
mod legacy_dialect;
mod proto1;
mod signals;
mod socket;
pub(crate) mod state_file;

use std::future::poll_fn;
use std::io::{self, Write};
use std::path::PathBuf;
use std::pin::{Pin, pin};
use std::process::ExitCode;
use std::task::Poll;

use pods_core::status::to_status_line;
use tokio::runtime::Builder;
use tokio::task::JoinHandle;

use hub::Hub;
use state_file::{StateFile, WriteFailures};

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

/// Becomes the one daemon on the socket, publishes, serves, and cleans up on a stop signal or when a task it serves with ends.
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
    let _lock = match socket::lock(&paths.socket).await {
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
    // Before the socket exists, so a client that can connect finds the state file unless the write failed.
    let first = to_status_line(&updates.borrow());
    let mut failures = WriteFailures::default();
    failures.note(&file, file.publish(first).await);
    let listener = match socket::bind(&paths.socket).await {
        Ok(listener) => listener,
        Err(error) => {
            log(&error);
            remove_state_file(&file).await;
            return ExitCode::FAILURE;
        }
    };

    let accept = tokio::spawn(socket::accept_loop(listener, hub.clone()));
    let follow = tokio::spawn(state_file::follow(updates, file.clone(), failures));
    // After the bind, so a missing or slow session bus never delays the socket or the state file.
    let bus = tokio::spawn(dbus::serve(hub.clone()));
    let code = until_stopped(stop.first(), accept, follow).await;
    bus.abort();
    // Awaited so the connection closes and the name is released first; the result is only the cancellation.
    drop(bus.await);
    if let Err(error) = socket::remove(&paths.socket).await {
        log(&error);
    }
    remove_state_file(&file).await;
    code
}

/// Waits for `stop` or for either task to end, logs which, ends both tasks, and gives the exit code: 1 when a task ended, so the service manager restarts the daemon.
async fn until_stopped(
    stop: impl Future<Output = &'static str>,
    mut accept: JoinHandle<()>,
    mut follow: JoinHandle<()>,
) -> ExitCode {
    let mut stop = pin!(stop);
    let code = poll_fn(|context| {
        if let Poll::Ready(signal) = stop.as_mut().poll(context) {
            log(&format!("{signal} received, stopping"));
            return Poll::Ready(ExitCode::SUCCESS);
        }
        let tasks = [
            ("the control socket's accept loop", &mut accept),
            ("the state file writer", &mut follow),
        ];
        for (task, handle) in tasks {
            if let Poll::Ready(result) = Pin::new(handle).poll(context) {
                let how =
                    result.map_or_else(|error| error.to_string(), |()| "it returned".to_owned());
                log(&format!("{task} ended ({how}); stopping with exit 1"));
                return Poll::Ready(ExitCode::FAILURE);
            }
        }
        Poll::Pending
    })
    .await;
    accept.abort();
    follow.abort();
    code
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

#[cfg(test)]
mod tests {
    use std::future;
    use std::time::Duration;

    use tokio::time::timeout;

    use super::*;

    /// A wait still running after this long missed the end it was waiting for.
    const HANG_LIMIT: Duration = Duration::from_secs(10);

    /// Runs `until_stopped` with `stop` and the two tasks the futures make, failing if it never returns.
    fn exit_code(
        stop: impl Future<Output = &'static str>,
        accept: impl Future<Output = ()> + Send + 'static,
        follow: impl Future<Output = ()> + Send + 'static,
    ) -> ExitCode {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let waiting = until_stopped(stop, tokio::spawn(accept), tokio::spawn(follow));
            timeout(HANG_LIMIT, waiting).await.unwrap()
        })
    }

    #[test]
    fn a_stop_signal_exits_zero() {
        let code = exit_code(
            future::ready("SIGTERM"),
            future::pending(),
            future::pending(),
        );
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn an_accept_loop_that_returns_exits_one() {
        let code = exit_code(future::pending(), future::ready(()), future::pending());
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn a_state_file_writer_that_panics_exits_one() {
        let code = exit_code(future::pending(), future::pending(), async {
            panic!("the writer failed");
        });
        assert_eq!(code, ExitCode::FAILURE);
    }
}
