//! One daemon per runtime directory, a socket only its owner can use, a task per client.

use std::fs::{self, File, TryLockError};
use std::io::ErrorKind;
use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::Path;
use std::time::Duration;

use pods_core::legacy::REOPEN_VERB;
use rustix::fs::{Mode, OFlags, open};
use rustix::io::Errno;
use rustix::process::umask;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::{JoinSet, spawn_blocking};
use tokio::time::{Instant, sleep, timeout};

use super::hub::Hub;
use super::{MAX_CONNECTIONS, legacy_dialect, log, proto1};

/// How long the start waits for a live daemon to accept, as the C++ `waitForConnected(300)` does.
const LIVE_DAEMON_PROBE: Duration = Duration::from_millis(300);

/// How long the start gives `reopen` to go out, as the C++ `waitForBytesWritten(200)` does.
const REOPEN_WRITE_TIMEOUT: Duration = Duration::from_millis(200);

/// Bits cleared from the socket's mode while it is bound, which leaves 0600.
const SOCKET_UMASK: u32 = 0o177;

const LOCK_EXTENSION: &str = "lock";

const LOCK_MODE: u32 = 0o600;

/// So a lasting failure such as no free descriptors does not spin.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Any other first byte is a legacy verb.
const PROTOCOL_1_START: u8 = b'{';

/// A live daemon is asked to reopen, as the C++ second instance asks.
pub(super) async fn ask_running_daemon_to_reopen(socket: &Path) -> bool {
    let Ok(Ok(mut stream)) = timeout(LIVE_DAEMON_PROBE, UnixStream::connect(socket)).await else {
        return false;
    };
    // The C++ does not check that `reopen` went out either; the live daemon decides the exit.
    drop(
        timeout(
            REOPEN_WRITE_TIMEOUT,
            stream.write_all(REOPEN_VERB.as_bytes()),
        )
        .await,
    );
    true
}

/// `None` while another starting or running daemon holds it.
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

/// Blocking; a link at `path` is never followed, a linked directory above it is.
fn take_lock(path: &Path) -> Result<Option<File>, String> {
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let file = match open(path, flags, Mode::from_raw_mode(LOCK_MODE)) {
        Ok(fd) => File::from(fd),
        // O_NOFOLLOW gives ELOOP on a final link, dangling or not, so its target is never created.
        Err(Errno::LOOP) => {
            return Err(format!(
                "cannot open the daemon lock {}: it is a symbolic link, which the daemon never opens",
                path.display()
            ));
        }
        Err(error) => {
            return Err(format!(
                "cannot open the daemon lock {}: {error}",
                path.display()
            ));
        }
    };
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(format!(
            "cannot take the daemon lock {}: {error}",
            path.display()
        )),
    }
}

/// Replaces a dead daemon's socket file; the caller holds the lock.
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
    // umask is per process; startup awaits each blocking job, so no other thread makes a file then.
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

/// One already gone is not an error.
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

/// Dropping the set at abort ends every client.
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
    use std::os::unix::fs::symlink;
    use std::sync::mpsc;
    use std::thread;

    use rustix::fs::{CWD, mkfifoat};
    use tokio::runtime::Builder;

    use super::*;
    use crate::daemon::link::tests::Running;
    use crate::daemon::paused::on_paused_clock;

    /// A lock still being taken after this long has blocked the thread it runs on.
    const HANG_LIMIT: Duration = Duration::from_secs(10);

    fn block_on<F: Future>(future: F) -> F::Output {
        Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(future)
    }

    fn open_reader(path: &Path) -> File {
        File::from(open(path, OFlags::RDONLY | OFlags::NONBLOCK, Mode::empty()).unwrap())
    }

    #[test]
    fn lock_is_refused_while_another_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("pods.sock");
        let held = block_on(lock(&socket)).unwrap();
        assert!(held.is_some());
        assert!(block_on(lock(&socket)).unwrap().is_none());
        drop(held);
        assert!(block_on(lock(&socket)).unwrap().is_some());
        let mode = fs::metadata(dir.path().join("pods.lock"))
            .unwrap()
            .permissions();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
            LOCK_MODE
        );
    }

    #[test]
    fn lock_is_never_opened_through_a_symbolic_link() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("pods.sock");
        let lock_path = socket.with_extension(LOCK_EXTENSION);
        let target = dir.path().join("target");
        symlink(&target, &lock_path).unwrap();
        let error = block_on(lock(&socket)).unwrap_err();
        assert!(error.contains(&lock_path.display().to_string()), "{error}");
        assert!(error.contains("symbolic link"), "{error}");
        assert!(
            fs::symlink_metadata(&target).is_err(),
            "the target was created"
        );
        fs::write(&target, "original").unwrap();
        let error = block_on(lock(&socket)).unwrap_err();
        assert!(error.contains("symbolic link"), "{error}");
        assert_eq!(fs::read_to_string(&target).unwrap(), "original");
    }

    #[test]
    fn lock_is_taken_in_a_linked_directory() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        let linked = dir.path().join("linked");
        symlink(&real, &linked).unwrap();
        let held = block_on(lock(&linked.join("pods.sock"))).unwrap();
        assert!(held.is_some());
        assert!(real.join("pods.lock").is_file());
    }

    #[test]
    fn lock_leaves_the_runtime_thread_free_while_it_opens_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("pods.sock");
        let lock_path = socket.with_extension(LOCK_EXTENSION);
        // Opening a FIFO to write waits for a reader; only a task on the runtime thread opens one.
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

    #[test]
    fn a_late_first_byte_leaves_the_legacy_request_only_the_rest_of_its_deadline() {
        // Well inside the deadline, so the byte lands before it ends.
        const FIRST_BYTE_AFTER: Duration = Duration::from_millis(600);
        let link = Running::new();
        let hub = link.hub();
        let closed = on_paused_clock(async {
            let (served, mut client) = UnixStream::pair().unwrap();
            let accepted = Instant::now();
            let serving = tokio::spawn(serve_client(served, hub));
            sleep(FIRST_BYTE_AFTER).await;
            client.write_all(b"s").await.unwrap();
            let mut rest = Vec::new();
            client.read_to_end(&mut rest).await.unwrap();
            serving.await.unwrap();
            (accepted.elapsed(), rest)
        });
        assert_eq!(closed, (legacy_dialect::REQUEST_DEADLINE, Vec::new()));
    }
}
