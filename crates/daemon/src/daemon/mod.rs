//! Owns the state; serves the control socket in both dialects, the state file and D-Bus.

mod bluez;
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

/// As `daemon/main.cpp` logs it.
pub(crate) const NO_RUNTIME_DIR: &str = "XDG_RUNTIME_DIR is unset; cannot place the control socket";

pub(crate) const NO_STATE_HOME: &str =
    "XDG_STATE_HOME is not an absolute path and HOME is unset; cannot place the state file";

/// A client past this is closed as soon as it is accepted.
const MAX_CONNECTIONS: usize = 64;

/// Each client makes at most one change between two turns of a subscriber's task.
const SUBSCRIBER_BACKLOG: usize = MAX_CONNECTIONS;

#[derive(Debug)]
pub(crate) struct Paths {
    pub(crate) socket: PathBuf,
    pub(crate) state_file: PathBuf,
}

pub(crate) fn run(paths: Paths) -> ExitCode {
    match Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime.block_on(serve(paths)),
        Err(error) => {
            log(&format!("cannot start the daemon's async runtime: {error}"));
            ExitCode::FAILURE
        }
    }
}

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
    let (requests, watcher_requests) = mpsc::channel(link::REQUEST_QUEUE);
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
    // Before the socket, so any client that connects finds the state file unless the write failed.
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
    // After the bind, so a missing or slow bus never delays the socket or the state file.
    let bus = tokio::spawn(dbus::serve(hub.clone()));
    let watcher = tokio::spawn(bluez::watch(link.clone(), watcher_requests));
    let (code, driver) = until_stopped(stop.first(), accept, follow, driver, watcher).await;
    if let Some(driver) = driver {
        link::stop(&link, driver).await;
    }
    bus.abort();
    // Awaited so the bus name is released first; the result is only the cancellation.
    drop(bus.await);
    if let Err(error) = socket::remove(&paths.socket).await {
        log(&error);
    }
    remove_state_file(&file).await;
    code
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    Signal,
    Task,
    Driver,
}

/// A task ending exits 1 so the service restarts; a live driver comes back for `link::stop`.
async fn until_stopped(
    stop: impl Future<Output = &'static str>,
    mut accept: JoinHandle<()>,
    mut follow: JoinHandle<()>,
    mut driver: JoinHandle<()>,
    mut watcher: JoinHandle<()>,
) -> (ExitCode, Option<JoinHandle<()>>) {
    let mut stop = pin!(stop);
    let ended = poll_fn(|context| {
        if let Poll::Ready(signal) = stop.as_mut().poll(context) {
            log(&format!("{signal} received, stopping"));
            return Poll::Ready(Ended::Signal);
        }
        // The driver before the watcher, which returns when the driver is gone.
        let tasks = [
            ("the control socket's accept loop", &mut accept, Ended::Task),
            ("the state file writer", &mut follow, Ended::Task),
            ("the link driver", &mut driver, Ended::Driver),
            ("the BlueZ watcher", &mut watcher, Ended::Task),
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
    watcher.abort();
    match ended {
        Ended::Signal => (ExitCode::SUCCESS, Some(driver)),
        Ended::Task => (ExitCode::FAILURE, Some(driver)),
        // A finished handle must not be awaited again, so nothing is left to stop.
        Ended::Driver => (ExitCode::FAILURE, None),
    }
}

/// The file going is how a watcher learns the daemon stopped.
async fn remove_state_file(file: &StateFile) {
    if let Err(error) = file.remove().await {
        log(&error.to_string());
    }
}

/// The service manager keeps stderr in the journal.
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

    /// A wait with nothing ended must still wait this long; nothing in it takes time.
    const STILL_WAITING_AFTER: Duration = Duration::from_millis(100);

    fn exit_code(
        stop: impl Future<Output = &'static str>,
        accept: impl Future<Output = ()> + Send + 'static,
        follow: impl Future<Output = ()> + Send + 'static,
        driver: impl Future<Output = ()> + Send + 'static,
        watcher: impl Future<Output = ()> + Send + 'static,
    ) -> ExitCode {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let (accept, follow, driver, watcher) = (
                tokio::spawn(accept),
                tokio::spawn(follow),
                tokio::spawn(driver),
                tokio::spawn(watcher),
            );
            let waiting = until_stopped(stop, accept, follow, driver, watcher);
            timeout(HANG_LIMIT, waiting).await.unwrap().0
        })
    }

    /// An aborted task drops its future, which sets the flag.
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
            let watcher = tokio::spawn(future::pending());
            let waiting = until_stopped(future::pending(), accept, follow, driver, watcher);
            timeout(STILL_WAITING_AFTER, waiting).await
        });
        assert!(waited.is_err(), "it returned {waited:?} with nothing ended");
    }

    #[test]
    fn the_tasks_still_running_are_ended() {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        let flags = [(); 3].map(|()| Arc::new(AtomicBool::new(false)));
        let ended = |flag: &Arc<AtomicBool>| {
            let flag = DropFlag(Arc::clone(flag));
            async move {
                let _flag = flag;
                future::pending::<()>().await;
            }
        };
        runtime.block_on(async {
            let [accept, follow, watcher] = flags.each_ref().map(|flag| tokio::spawn(ended(flag)));
            // One turn so each task holds its flag before the stop.
            task::yield_now().await;
            let driver = tokio::spawn(future::pending());
            let stopped = until_stopped(future::ready("SIGTERM"), accept, follow, driver, watcher);
            assert_eq!(stopped.await.0, ExitCode::SUCCESS);
            task::yield_now().await;
            let dropped = flags.each_ref().map(|flag| flag.load(Ordering::Relaxed));
            assert_eq!(dropped, [true, true, true], "accept, follow, watcher");
        });
    }

    #[test]
    fn a_stop_signal_exits_zero() {
        let code = exit_code(
            future::ready("SIGTERM"),
            future::pending(),
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
            future::pending(),
        );
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn a_bluez_watcher_that_returns_exits_one_and_hands_back_the_driver() {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let accept = tokio::spawn(future::pending());
            let follow = tokio::spawn(future::pending());
            let driver = tokio::spawn(future::pending());
            let watcher = tokio::spawn(future::ready(()));
            let stopped = until_stopped(future::pending(), accept, follow, driver, watcher);
            let (code, driver) = timeout(HANG_LIMIT, stopped).await.unwrap();
            assert_eq!(code, ExitCode::FAILURE);
            driver.expect("the running driver").abort();
        });
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
            let watcher = tokio::spawn(future::pending());
            let waiting = until_stopped(future::pending(), accept, follow, driver, watcher);
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
            let watcher = tokio::spawn(future::pending());
            let stopped = until_stopped(future::ready("SIGTERM"), accept, follow, driver, watcher);
            let (code, driver) = timeout(HANG_LIMIT, stopped).await.unwrap();
            assert_eq!(code, ExitCode::SUCCESS);
            let driver = driver.expect("the running driver");
            assert!(!driver.is_finished());
            driver.abort();
        });
    }
}
