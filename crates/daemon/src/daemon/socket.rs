//! The control socket: one daemon per runtime directory, a socket file only its owner can use, one task per client in either dialect.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::ErrorKind;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::Path;
use std::time::Duration;

use pods_core::legacy::REOPEN_VERB;
use rustix::fs::Mode;
use rustix::process::umask;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::{JoinSet, spawn_blocking};
use tokio::time::{Instant, sleep, timeout};

use super::hub::Hub;
use super::{legacy_dialect, log, proto1};

/// How long the start waits for a live daemon to accept, as the C++ `waitForConnected(300)` does.
const LIVE_DAEMON_PROBE: Duration = Duration::from_millis(300);

/// How long the start gives `reopen` to go out, as the C++ `waitForBytesWritten(200)` does.
const REOPEN_WRITE_TIMEOUT: Duration = Duration::from_millis(200);

/// Bits cleared from the socket's mode while it is bound, which leaves 0600.
const SOCKET_UMASK: u32 = 0o177;

/// The lock file sits beside the socket under this extension.
const LOCK_EXTENSION: &str = "lock";

/// Mode of the lock file.
const LOCK_MODE: u32 = 0o600;

/// Clients served at once; a client past this is closed as soon as it is accepted.
pub(super) const MAX_CONNECTIONS: usize = 64;

/// The pause after a failed accept, so a lasting failure such as no free descriptors does not spin.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);

/// The first byte of every protocol 1 connection; any other first byte is a legacy verb.
const PROTOCOL_1_START: u8 = b'{';

/// Whether a daemon already answers on `socket`; one that does is asked to reopen, as the C++ second instance asks.
pub(super) async fn ask_running_daemon_to_reopen(socket: &Path) -> bool {
    let Ok(Ok(mut stream)) = timeout(LIVE_DAEMON_PROBE, UnixStream::connect(socket)).await else {
        return false;
    };
    // The C++ does not check that `reopen` went out either; the live daemon is what decides the exit.
    drop(
        timeout(
            REOPEN_WRITE_TIMEOUT,
            stream.write_all(REOPEN_VERB.as_bytes()),
        )
        .await,
    );
    true
}

/// Takes the lock beside `socket` that one starting or running daemon holds, or `None` while another holds it.
pub(super) async fn lock(socket: &Path) -> Result<Option<File>, String> {
    let path = socket.with_extension(LOCK_EXTENSION);
    let job = path.clone();
    spawn_blocking(move || take_lock(&job))
        .await
        .map_err(|error| {
            format!(
                "cannot take the daemon lock {}: the lock did not finish: {error}",
                path.display()
            )
        })?
}

/// Opens and locks `path` on the calling thread, which `lock` keeps off the runtime.
fn take_lock(path: &Path) -> Result<Option<File>, String> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(LOCK_MODE)
        .open(path)
        .map_err(|error| format!("cannot open the daemon lock {}: {error}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(format!(
            "cannot take the daemon lock {}: {error}",
            path.display()
        )),
    }
}

/// Removes a socket file a dead daemon left, then binds `socket` with mode 0600; the caller holds the lock.
pub(super) async fn bind(socket: &Path) -> Result<UnixListener, String> {
    let job = socket.to_path_buf();
    let listener = spawn_blocking(move || bind_now(&job))
        .await
        .map_err(|error| {
            format!(
                "cannot bind the control socket {}: the bind did not finish: {error}",
                socket.display()
            )
        })??;
    UnixListener::from_std(listener).map_err(|error| {
        format!(
            "cannot serve the control socket {}: {error}",
            socket.display()
        )
    })
}

/// The file work of `bind` on the calling thread, which `bind` keeps off the runtime.
fn bind_now(socket: &Path) -> Result<StdUnixListener, String> {
    match fs::remove_file(socket) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "cannot remove the stale control socket {}: {error}",
                socket.display()
            ));
        }
    }
    // The umask is process wide; startup awaits each blocking job before the next, so no other thread creates a file meanwhile.
    let previous = umask(Mode::from_raw_mode(SOCKET_UMASK));
    let bound = StdUnixListener::bind(socket);
    umask(previous);
    let listener = bound.map_err(|error| {
        format!(
            "cannot bind the control socket {}: {error}",
            socket.display()
        )
    })?;
    listener.set_nonblocking(true).map_err(|error| {
        format!(
            "cannot serve the control socket {}: {error}",
            socket.display()
        )
    })?;
    Ok(listener)
}

/// Removes the socket file at shutdown, on the blocking pool; one already gone is not an error.
pub(super) async fn remove(socket: &Path) -> Result<(), String> {
    let job = socket.to_path_buf();
    let removal = spawn_blocking(move || fs::remove_file(job)).await;
    match removal {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) if error.kind() == ErrorKind::NotFound => Ok(()),
        Ok(Err(error)) => Err(format!(
            "cannot remove the control socket {}: {error}",
            socket.display()
        )),
        Err(error) => Err(format!(
            "cannot remove the control socket {}: the removal did not finish: {error}",
            socket.display()
        )),
    }
}

/// Accepts clients until aborted, each in its own task, so a slow or silent client delays no other; dropping the set at abort ends every client.
pub(super) async fn accept_loop(listener: UnixListener, hub: Hub) {
    let mut connections = JoinSet::new();
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(error) => {
                log(&format!("control socket: accept failed: {error}"));
                sleep(ACCEPT_RETRY_DELAY).await;
                continue;
            }
        };
        // A connection task returns nothing and cannot panic, so a finished one only needs reaping.
        while connections.try_join_next().is_some() {}
        if connections.len() >= MAX_CONNECTIONS {
            log(&format!(
                "control socket: {MAX_CONNECTIONS} clients already connected, closing a new one"
            ));
            continue;
        }
        connections.spawn(serve_client(stream, hub.clone()));
    }
}

/// Reads the first byte within the legacy request deadline and serves the dialect it starts.
async fn serve_client(mut stream: UnixStream, hub: Hub) {
    let accepted = Instant::now();
    let mut first = [0_u8; 1];
    match timeout(legacy_dialect::REQUEST_DEADLINE, stream.read(&mut first)).await {
        Ok(Ok(1)) if first == [PROTOCOL_1_START] => {
            proto1::serve_connection(stream, hub, first.to_vec()).await;
        }
        Ok(Ok(1)) => {
            let left = legacy_dialect::REQUEST_DEADLINE.saturating_sub(accepted.elapsed());
            legacy_dialect::serve_connection(stream, hub, first.to_vec(), left).await;
        }
        // Nothing arrived, which the legacy dialect has always answered with nothing.
        Ok(Ok(_) | Err(_)) | Err(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;

    use cpp_source::MAIN;
    use rustix::fs::{CWD, OFlags, mkfifoat, open};
    use tokio::runtime::Builder;

    use super::*;

    /// A lock still being taken after this long has blocked the thread it runs on.
    const HANG_LIMIT: Duration = Duration::from_secs(10);

    fn block_on<F: Future>(future: F) -> F::Output {
        Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(future)
    }

    /// Opens the FIFO at `path` to read without waiting for a writer.
    fn open_reader(path: &Path) -> File {
        File::from(open(path, OFlags::RDONLY | OFlags::NONBLOCK, Mode::empty()).unwrap())
    }

    #[test]
    fn start_probe_matches_the_cpp_single_instance_guard() {
        let connect = MAIN.find(None, "socket_check.waitForConnected(", ')');
        assert_eq!(connect, LIVE_DAEMON_PROBE.as_millis().to_string());
        let write = MAIN.find(None, "socket_check.waitForBytesWritten(", ')');
        assert_eq!(write, REOPEN_WRITE_TIMEOUT.as_millis().to_string());
        let verb = MAIN.find(None, "socket_check.write(\"", '"');
        assert_eq!(verb, REOPEN_VERB);
    }

    #[test]
    fn lock_is_refused_while_another_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("librepods.sock");
        let held = block_on(lock(&socket)).unwrap();
        assert!(held.is_some());
        assert!(block_on(lock(&socket)).unwrap().is_none());
        drop(held);
        assert!(block_on(lock(&socket)).unwrap().is_some());
        let mode = fs::metadata(dir.path().join("librepods.lock"))
            .unwrap()
            .permissions();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
            LOCK_MODE
        );
    }

    #[test]
    fn lock_leaves_the_runtime_thread_free_while_it_opens_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("librepods.sock");
        let lock_path = socket.with_extension(LOCK_EXTENSION);
        // Opening a FIFO to write waits for a reader, and only a task on the runtime thread opens one.
        mkfifoat(CWD, &lock_path, Mode::from_raw_mode(LOCK_MODE)).unwrap();
        let reader_path = lock_path.clone();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            let outcome = block_on(async {
                let reader = tokio::spawn(async move { open_reader(&reader_path) });
                let taken = lock(&socket).await;
                (taken, reader.await)
            });
            // The test has already failed if nobody is left to receive this.
            drop(done.send(outcome));
        });
        match finished.recv_timeout(HANG_LIMIT) {
            Ok((taken, reader)) => {
                assert!(taken.unwrap().is_some());
                drop(reader.unwrap());
            }
            Err(error) => {
                drop(open_reader(&lock_path));
                panic!("taking the lock held the runtime thread: {error}");
            }
        }
    }
}
