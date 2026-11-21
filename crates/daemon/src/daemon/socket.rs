//! The control socket: one daemon per runtime directory, a socket file only its owner can use, one task per client.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::ErrorKind;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::Path;
use std::time::Duration;

use pods_core::legacy::REOPEN_VERB;
use rustix::fs::Mode;
use rustix::process::umask;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};

use super::hub::Hub;
use super::{legacy_dialect, log};

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
const MAX_CONNECTIONS: usize = 64;

/// The pause after a failed accept, so a lasting failure such as no free descriptors does not spin.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);

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
pub(super) fn lock(socket: &Path) -> Result<Option<File>, String> {
    let path = socket.with_extension(LOCK_EXTENSION);
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(LOCK_MODE)
        .open(&path)
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
pub(super) fn bind(socket: &Path) -> Result<UnixListener, String> {
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
    // The umask is process wide; the current thread runtime has started no other thread to create a file meanwhile.
    let previous = umask(Mode::from_raw_mode(SOCKET_UMASK));
    let bound = StdUnixListener::bind(socket);
    umask(previous);
    let listener = bound.map_err(|error| {
        format!(
            "cannot bind the control socket {}: {error}",
            socket.display()
        )
    })?;
    listener
        .set_nonblocking(true)
        .and_then(|()| UnixListener::from_std(listener))
        .map_err(|error| {
            format!(
                "cannot serve the control socket {}: {error}",
                socket.display()
            )
        })
}

/// Accepts clients until aborted, each in its own task, so a slow or silent client delays no other.
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
        connections.spawn(legacy_dialect::serve_connection(stream, hub.clone()));
    }
}

#[cfg(test)]
mod tests {
    use cpp_source::MAIN;

    use super::*;

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
        let held = lock(&socket).unwrap();
        assert!(held.is_some());
        assert!(lock(&socket).unwrap().is_none());
        drop(held);
        assert!(lock(&socket).unwrap().is_some());
        let mode = fs::metadata(dir.path().join("librepods.lock"))
            .unwrap()
            .permissions();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
            LOCK_MODE
        );
    }
}
