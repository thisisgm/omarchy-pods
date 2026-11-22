//! A fake audio server at `<runtime dir>/pulse/native`, where the desktop's `PipeWire` pulse server listens; its threads end on drop.

mod server;
pub(crate) mod wire;

use std::fs::{self, DirBuilder};
use std::io::ErrorKind;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use pulseaudio::protocol::SubscriptionMask;
use rustix::net::{self, Shutdown};

use server::{Shared, State};
use wire::{percent_of, volume_of};

/// `sockaddr_un` holds 108 bytes, the closing NUL among them.
const SOCKET_PATH_MAX: usize = 107;
const SOCKET_DIR_MODE: u32 = 0o700;

/// The daemon's wait for any one answer of the audio server.
pub(crate) const CALL_TIMEOUT: Duration = Duration::from_millis(2000);
/// The daemon's waits between attempts, the last one repeating.
pub(crate) const LADDER_MS: [u64; 5] = [250, 500, 1000, 2000, 5000];
/// The volume the pods' sink starts at.
pub(crate) const PERCENT: u8 = 40;

/// One command the fake received, in the terms a test asserts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Asked {
    Auth { version: u16 },
    ClientName,
    ServerInfo,
    SinkInfo { name: String },
    SinkList,
    Volume { name: String, percent: u8 },
    Subscribe { mask: SubscriptionMask },
    Unknown { command: u32 },
}

impl Asked {
    fn only_reads(&self) -> bool {
        match self {
            Self::Auth { .. }
            | Self::ClientName
            | Self::ServerInfo
            | Self::SinkInfo { .. }
            | Self::SinkList
            | Self::Subscribe { .. } => true,
            Self::Volume { .. } | Self::Unknown { .. } => false,
        }
    }
}

/// Whether a restarted server keeps the volumes set before it, as a session manager that stored them does.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Volumes {
    Kept,
    Forgotten,
}

pub(crate) struct FakeAudio {
    shared: Arc<Shared>,
    socket: PathBuf,
    listener: Mutex<Option<(UnixListener, JoinHandle<()>)>>,
}

impl FakeAudio {
    pub(crate) fn start(runtime_dir: &Path) -> Self {
        let directory = runtime_dir.join("pulse");
        DirBuilder::new()
            .mode(SOCKET_DIR_MODE)
            .create(&directory)
            .unwrap_or_else(|error| panic!("creating {}: {error}", directory.display()));
        let socket = directory.join("native");
        let length = socket.as_os_str().len();
        assert!(
            length <= SOCKET_PATH_MAX,
            "the socket path {} is {length} bytes, past the {SOCKET_PATH_MAX} a unix socket takes",
            socket.display()
        );
        let audio = Self {
            shared: Shared::new(),
            socket,
            listener: Mutex::new(None),
        };
        audio.listen();
        audio
    }

    pub(crate) fn socket(&self) -> &Path {
        &self.socket
    }

    pub(crate) fn add_sink(&self, name: &str, percent: u8) {
        self.change(|state| state.add_sink(name, volume_of(percent)));
    }

    pub(crate) fn remove_sink(&self, name: &str) {
        self.change(|state| state.remove_sink(name));
    }

    pub(crate) fn set_default_sink(&self, name: &str) {
        self.change(|state| state.set_default_sink(name));
    }

    /// As a user would set it, so every subscriber hears the change.
    pub(crate) fn set_volume(&self, name: &str, percent: u8) {
        self.change(|state| state.set_volume(name, volume_of(percent)));
    }

    pub(crate) fn volume(&self, name: &str) -> u8 {
        percent_of(self.state().volume(name))
    }

    pub(crate) fn wait_volume(&self, name: &str, percent: u8, within: Duration) {
        let what = format!("{name} at {percent} %");
        self.wait(&what, within, |state| {
            (percent_of(state.volume(name)) == percent).then_some(())
        });
    }

    pub(crate) fn clients(&self) -> usize {
        self.state().clients()
    }

    pub(crate) fn wait_clients(&self, count: usize, within: Duration) {
        let what = format!("{count} clients");
        self.wait(&what, within, |state| {
            (state.clients() == count).then_some(())
        });
    }

    /// Every command received since `start`, in order, across restarts.
    pub(crate) fn requests(&self) -> Vec<Asked> {
        self.state().requests.clone()
    }

    pub(crate) fn wait_requests(&self, count: usize, within: Duration) -> Vec<Asked> {
        let what = format!("{count} requests");
        self.wait(&what, within, |state| {
            (state.requests.len() >= count).then(|| state.requests.clone())
        })
    }

    /// Fails unless something was asked and every command received only reads: no volume, profile, stream or default sink write.
    pub(crate) fn assert_only_reads(&self) {
        let requests = self.requests();
        assert!(!requests.is_empty(), "the daemon asked nothing");
        let writes: Vec<&Asked> = requests
            .iter()
            .filter(|asked| !asked.only_reads())
            .collect();
        assert!(writes.is_empty(), "the daemon asked for {writes:?}");
    }

    /// Ends the listener and every client, joins their threads, and removes the socket.
    pub(crate) fn stop(&self) {
        let Some(listener) = self
            .listener
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let (listening, accepting) = listener;
        self.shared.lock().stopping = true;
        let mut panicked = Vec::new();
        // Linux wakes an accept blocked on a listening socket shut for reading, with EINVAL.
        match net::shutdown(&listening, Shutdown::Read) {
            Ok(()) => {
                if accepting.join().is_err() {
                    panicked.push("the listener thread panicked".to_owned());
                }
            }
            Err(error) => panicked.push(format!("shutting the listening socket: {error}")),
        }
        let clients = self.shared.lock().close_clients();
        for client in clients {
            if client.join().is_err() {
                panicked.push("a client thread panicked".to_owned());
            }
        }
        if let Err(error) = fs::remove_file(&self.socket)
            && error.kind() != ErrorKind::NotFound
        {
            panicked.push(format!("removing {}: {error}", self.socket.display()));
        }
        if !thread::panicking() {
            assert!(panicked.is_empty(), "the fake audio server: {panicked:?}");
            fail_on_fault(&self.shared.lock());
        }
    }

    pub(crate) fn restart(&self, volumes: Volumes) {
        self.stop();
        self.change(|state| {
            state.stopping = false;
            if let Volumes::Forgotten = volumes {
                state.forget_volumes();
            }
        });
        self.listen();
    }

    /// While held, every message to every client waits; release sends them in order.
    pub(crate) fn hold_replies(&self, hold: bool) {
        self.change(|state| state.hold(hold));
    }

    pub(crate) fn send_oversized_frame(&self) {
        self.change(State::send_oversized_frame);
    }

    fn listen(&self) {
        let listening = server::listen(&self.shared, &self.socket);
        *self.listener.lock().unwrap_or_else(PoisonError::into_inner) = Some(listening);
    }

    fn state(&self) -> MutexGuard<'_, State> {
        let state = self.shared.lock();
        fail_on_fault(&state);
        state
    }

    fn change<T>(&self, change: impl FnOnce(&mut State) -> T) -> T {
        fail_on_fault(&self.shared.lock());
        self.shared.change(change)
    }

    fn wait<T>(
        &self,
        what: &str,
        within: Duration,
        mut ready: impl FnMut(&State) -> Option<T>,
    ) -> T {
        let started = Instant::now();
        let mut state = self.state();
        loop {
            fail_on_fault(&state);
            if let Some(value) = ready(&state) {
                return value;
            }
            let left = within.saturating_sub(started.elapsed());
            assert!(!left.is_zero(), "{what} not seen within {within:?}");
            state = self
                .shared
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// Fails the test on a fault a server thread recorded.
fn fail_on_fault(state: &State) {
    if let Some(fault) = &state.fault {
        panic!("the fake audio server: {fault}");
    }
}

impl Drop for FakeAudio {
    fn drop(&mut self) {
        self.stop();
    }
}
