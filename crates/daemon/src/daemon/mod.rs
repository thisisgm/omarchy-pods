//! The daemon: it owns the state, serves the control socket in protocol 1 and the C++ daemon's dialect, publishes the state file and serves D-Bus.

mod dbus;
mod hub;
mod legacy_dialect;
mod link;
mod proto1;
mod signals;
mod socket;
pub(crate) mod state_file;

use std::future::poll_fn;
use std::io::{self, Write};
use std::path::PathBuf;
use std::pin::{Pin, pin};
use std::process::ExitCode;
use std::sync::Arc;
use std::task::Poll;

use pods_core::status::to_status_line;
use pods_proto::packets::CONTROL_PSM;
use pods_sys::l2cap;
use tokio::runtime::Builder;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use hub::Hub;
use state_file::{StateFile, WriteFailures};

/// What `daemon/main.cpp` logs when the session has no runtime directory.
pub(crate) const NO_RUNTIME_DIR: &str = "XDG_RUNTIME_DIR is unset; cannot place the control socket";

/// What the daemon logs when neither variable can place the state file.
pub(crate) const NO_STATE_HOME: &str =
    "XDG_STATE_HOME is not an absolute path and HOME is unset; cannot place the state file";

/// Clients served at once; a client past this is closed as soon as it is accepted.
const MAX_CONNECTIONS: usize = 64;

/// Changes a subscriber may leave untaken before the hub drops it; each client makes at most one change between two turns of a subscriber's task.
const SUBSCRIBER_BACKLOG: usize = MAX_CONNECTIONS;

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

    let (link, inputs) = link::queue();
    let hub = Hub::new(link.clone());
    // Held for the device watcher, which reads it; until it exists nothing sends a device event, so nothing is requested.
    let (requests, _watcher_requests) = mpsc::channel(link::REQUEST_QUEUE);
    let connector: link::Connector =
        Arc::new(|device| l2cap::connect(device, CONTROL_PSM, link::CONNECT_TIMEOUT));
    let driver = tokio::spawn(link::drive(
        hub.clone(),
        inputs,
        link.clone(),
        requests,
        connector,
    ));
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
            link::stop(&link, driver).await;
            remove_state_file(&file).await;
            return ExitCode::FAILURE;
        }
    };

    let accept = tokio::spawn(socket::accept_loop(listener, hub.clone()));
    let follow = tokio::spawn(state_file::follow(updates, file.clone(), failures));
    // After the bind, so a missing or slow session bus never delays the socket or the state file.
    let bus = tokio::spawn(dbus::serve(hub.clone()));
    let (code, driver) = until_stopped(stop.first(), accept, follow, driver).await;
    if let Some(driver) = driver {
        link::stop(&link, driver).await;
    }
    bus.abort();
    // Awaited so the connection closes and the name is released first; the result is only the cancellation.
    drop(bus.await);
    if let Err(error) = socket::remove(&paths.socket).await {
        log(&error);
    }
    remove_state_file(&file).await;
    code
}

/// What ended the wait in `until_stopped`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    Signal,
    Task,
    Driver,
}

/// Waits for `stop` or for any task to end, logs which, and ends `accept` and `follow`; the exit code is 1 when a task ended, so the service manager restarts the daemon, and the driver comes back while it still runs, for `link::stop`.
async fn until_stopped(
    stop: impl Future<Output = &'static str>,
    mut accept: JoinHandle<()>,
    mut follow: JoinHandle<()>,
    mut driver: JoinHandle<()>,
) -> (ExitCode, Option<JoinHandle<()>>) {
    let mut stop = pin!(stop);
    let ended = poll_fn(|context| {
        if let Poll::Ready(signal) = stop.as_mut().poll(context) {
            log(&format!("{signal} received, stopping"));
            return Poll::Ready(Ended::Signal);
        }
        let tasks = [
            ("the control socket's accept loop", &mut accept, Ended::Task),
            ("the state file writer", &mut follow, Ended::Task),
            ("the link driver", &mut driver, Ended::Driver),
        ];
        for (task, handle, ended) in tasks {
            if let Poll::Ready(result) = Pin::new(handle).poll(context) {
                let how =
                    result.map_or_else(|error| error.to_string(), |()| "it returned".to_owned());
                log(&format!("{task} ended ({how}); stopping with exit 1"));
                return Poll::Ready(ended);
            }
        }
        Poll::Pending
    })
    .await;
    accept.abort();
    follow.abort();
    match ended {
        Ended::Signal => (ExitCode::SUCCESS, Some(driver)),
        Ended::Task => (ExitCode::FAILURE, Some(driver)),
        // A finished handle must not be awaited again, so nothing is left to stop.
        Ended::Driver => (ExitCode::FAILURE, None),
    }
}

/// Removes the state file, which is how a watcher learns the daemon stopped.
async fn remove_state_file(file: &StateFile) {
    if let Err(error) = file.remove().await {
        log(&error.to_string());
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use pods_sys::l2cap::ChannelError;
    use rustix::io::Errno;
    use tokio::sync::oneshot;
    use tokio::task;
    use tokio::time::timeout;

    use super::*;
    use crate::daemon::link::tests::HANG_LIMIT;

    /// How long a wait with nothing ended must still be waiting; nothing in it takes time, so any wait shows a wrong return.
    const STILL_WAITING_AFTER: Duration = Duration::from_millis(100);

    /// Runs `until_stopped` with `stop` and the three tasks the futures make, failing if it never returns.
    fn exit_code(
        stop: impl Future<Output = &'static str>,
        accept: impl Future<Output = ()> + Send + 'static,
        follow: impl Future<Output = ()> + Send + 'static,
        driver: impl Future<Output = ()> + Send + 'static,
    ) -> ExitCode {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let (accept, follow, driver) = (
                tokio::spawn(accept),
                tokio::spawn(follow),
                tokio::spawn(driver),
            );
            let waiting = until_stopped(stop, accept, follow, driver);
            timeout(HANG_LIMIT, waiting).await.unwrap().0
        })
    }

    /// Sets its flag when dropped, which an aborted task does to its future.
    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    #[test]
    fn nothing_ended_keeps_it_waiting() {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        let waited = runtime.block_on(async {
            let accept = tokio::spawn(future::pending());
            let follow = tokio::spawn(future::pending());
            let driver = tokio::spawn(future::pending());
            let waiting = until_stopped(future::pending(), accept, follow, driver);
            timeout(STILL_WAITING_AFTER, waiting).await
        });
        assert!(waited.is_err(), "it returned {waited:?} with nothing ended");
    }

    #[test]
    fn the_tasks_still_running_are_ended() {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        let flags = [
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        ];
        let [accept_dropped, follow_dropped] = flags.clone();
        runtime.block_on(async {
            let accept = tokio::spawn(async move {
                let _flag = DropFlag(accept_dropped);
                future::pending::<()>().await;
            });
            let follow = tokio::spawn(async move {
                let _flag = DropFlag(follow_dropped);
                future::pending::<()>().await;
            });
            // One turn so each task holds its flag before the stop.
            task::yield_now().await;
            assert_eq!(
                until_stopped(
                    future::ready("SIGTERM"),
                    accept,
                    follow,
                    tokio::spawn(future::pending())
                )
                .await
                .0,
                ExitCode::SUCCESS
            );
            task::yield_now().await;
            let dropped = flags.each_ref().map(|flag| flag.load(Ordering::Relaxed));
            assert_eq!(dropped, [true, true], "accept, follow");
        });
    }

    #[test]
    fn a_stop_signal_exits_zero() {
        let code = exit_code(
            future::ready("SIGTERM"),
            future::pending(),
            future::pending(),
            future::pending(),
        );
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn an_accept_loop_that_returns_exits_one() {
        let code = exit_code(
            future::pending(),
            future::ready(()),
            future::pending(),
            future::pending(),
        );
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn a_state_file_writer_that_panics_exits_one() {
        let code = exit_code(
            future::pending(),
            future::pending(),
            async {
                panic!("the writer failed");
            },
            future::pending(),
        );
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn a_link_driver_that_panics_exits_one() {
        let code = exit_code(
            future::pending(),
            future::pending(),
            future::pending(),
            async {
                panic!("the driver failed");
            },
        );
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn a_link_driver_that_returns_while_not_stopping_exits_one() {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        let (code, driver) = runtime.block_on(async {
            let (link, inputs) = link::queue();
            let (requests, _unread) = mpsc::channel(link::REQUEST_QUEUE);
            let refuse: link::Connector = Arc::new(|_| Err(ChannelError::Connect(Errno::HOSTDOWN)));
            let hub = Hub::new(link.clone());
            let driver = tokio::spawn(link::drive(hub, inputs, link.clone(), requests, refuse));
            // The real driver returns after a `Shutdown`, here one the stop path never sent.
            let (done, _stopped) = oneshot::channel();
            link.send(link::Input::Shutdown(done)).await.unwrap();
            let accept = tokio::spawn(future::pending());
            let follow = tokio::spawn(future::pending());
            let waiting = until_stopped(future::pending(), accept, follow, driver);
            timeout(HANG_LIMIT, waiting).await.unwrap()
        });
        assert_eq!(code, ExitCode::FAILURE);
        assert!(
            driver.is_none(),
            "an ended driver handed back to be stopped"
        );
    }

    #[test]
    fn a_stop_signal_hands_back_the_running_driver() {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let accept = tokio::spawn(future::pending());
            let follow = tokio::spawn(future::pending());
            let driver = tokio::spawn(future::pending());
            let stopped = until_stopped(future::ready("SIGTERM"), accept, follow, driver);
            let (code, driver) = timeout(HANG_LIMIT, stopped).await.unwrap();
            assert_eq!(code, ExitCode::SUCCESS);
            let driver = driver.expect("the running driver");
            assert!(!driver.is_finished());
            driver.abort();
        });
    }
}
