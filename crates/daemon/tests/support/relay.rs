//! A unix socket in front of a private bus that copies bytes both ways for each client, and can cut every client at once.

use std::io;
use std::mem;
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};

/// One end of each side of every client relayed so far, and the threads copying between them.
#[derive(Default)]
struct Clients {
    streams: Vec<UnixStream>,
    copies: Vec<JoinHandle<()>>,
}

/// Bytes only, so a file descriptor sent along is lost; nothing the daemon or the fakes do on the system bus passes one.
pub(crate) struct Relay {
    path: PathBuf,
    stopping: Arc<AtomicBool>,
    clients: Arc<Mutex<Clients>>,
    accepting: Option<JoinHandle<Result<(), String>>>,
}

impl Relay {
    /// Listens at `path` and relays each client to the bus socket at `target`.
    pub(crate) fn start(path: &Path, target: &Path) -> Self {
        let listener = UnixListener::bind(path).unwrap_or_else(|error| {
            panic!("the relay cannot listen at {}: {error}", path.display())
        });
        let stopping = Arc::new(AtomicBool::new(false));
        let clients = Arc::new(Mutex::new(Clients::default()));
        let accepting = {
            let stopping = Arc::clone(&stopping);
            let clients = Arc::clone(&clients);
            let target = target.to_owned();
            thread::spawn(move || accept(&listener, &target, &stopping, &clients))
        };
        Self {
            path: path.to_owned(),
            stopping,
            clients,
            accepting: Some(accepting),
        }
    }

    pub(crate) fn address(&self) -> String {
        format!("unix:path={}", self.path.display())
    }

    /// Closes every relayed connection; the listener stays, so a client that dials again gets through.
    pub(crate) fn cut(&self) {
        let streams = mem::take(&mut lock(&self.clients).streams);
        for stream in streams {
            // A side its peer already closed refuses the shutdown, which is no fault.
            stream.shutdown(Shutdown::Both).ok();
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        // `accept` blocks, so one connection of the relay's own wakes it to see the flag.
        drop(UnixStream::connect(&self.path));
        let accepted = self.accepting.take().map(JoinHandle::join);
        self.cut();
        let copies = mem::take(&mut lock(&self.clients).copies);
        let mut copies_panicked = false;
        for copy in copies {
            copies_panicked |= copy.join().is_err();
        }
        let failure = match accepted {
            Some(Ok(Err(failure))) => Some(failure),
            Some(Err(_)) => Some("the relay's accept thread panicked".to_owned()),
            Some(Ok(Ok(()))) | None if copies_panicked => {
                Some("a relay copy thread panicked".to_owned())
            }
            Some(Ok(Ok(()))) | None => None,
        };
        if let Some(failure) = failure {
            report(&failure);
        }
    }
}

/// Fails the test with `failure`, or names it beside the panic already unwinding, which a second panic would abort.
#[expect(
    clippy::print_stderr,
    reason = "a relay failure under a test's own panic goes into the test's output"
)]
fn report(failure: &str) {
    if thread::panicking() {
        eprintln!("{failure}");
    } else {
        panic!("{failure}");
    }
}

fn lock(clients: &Mutex<Clients>) -> MutexGuard<'_, Clients> {
    clients.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Relays every client until `stopping` is set.
fn accept(
    listener: &UnixListener,
    target: &Path,
    stopping: &AtomicBool,
    clients: &Mutex<Clients>,
) -> Result<(), String> {
    loop {
        let (client, _) = listener
            .accept()
            .map_err(|error| format!("the relay cannot accept: {error}"))?;
        if stopping.load(Ordering::SeqCst) {
            return Ok(());
        }
        let bus = UnixStream::connect(target).map_err(|error| {
            format!(
                "the relay cannot reach the bus at {}: {error}",
                target.display()
            )
        })?;
        relay(client, bus, clients)
            .map_err(|error| format!("the relay cannot copy a socket: {error}"))?;
    }
}

/// Starts the two copy threads of one client and keeps a handle on each side for `cut`.
fn relay(client: UnixStream, bus: UnixStream, clients: &Mutex<Clients>) -> io::Result<()> {
    let streams = [client.try_clone()?, bus.try_clone()?];
    let back = [bus.try_clone()?, client.try_clone()?];
    let mut clients = lock(clients);
    clients.streams.extend(streams);
    clients
        .copies
        .push(thread::spawn(move || copy(&client, &bus)));
    let [from, to] = back;
    clients.copies.push(thread::spawn(move || copy(&from, &to)));
    Ok(())
}

/// Ends on the peer's close, a cut or an error alike, and passes the end on as a close.
fn copy(mut from: &UnixStream, mut to: &UnixStream) {
    io::copy(&mut from, &mut to).ok();
    to.shutdown(Shutdown::Write).ok();
}
