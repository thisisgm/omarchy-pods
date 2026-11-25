//! A fake audio server at `<runtime dir>/pulse/native`, where the desktop's `PipeWire` pulse server listens; its threads end on drop.

mod capture;
mod cards;
mod lists;
mod server;
pub(crate) mod wire;

use std::ffi::CString;
use std::fs::{self, DirBuilder};
use std::io::ErrorKind;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use pods_core::policy::audio::AUDIO_CALL_TIMEOUT_MS;
use pulseaudio::protocol::{ChannelVolume, CommandTag, Props, SinkState, SubscriptionMask, Volume};
use rustix::net::{self, Shutdown};

pub(crate) use capture::{Recorder, RecorderId};
pub(crate) use cards::FakeProfile;
use server::{Shared, State};
use wire::{percent_of, volume_of};

/// `sockaddr_un` holds 108 bytes, the closing NUL among them.
const SOCKET_PATH_MAX: usize = 107;
const SOCKET_DIR_MODE: u32 = 0o700;

/// The daemon's wait for any one answer of the audio server.
pub(crate) const CALL_TIMEOUT: Duration = Duration::from_millis(AUDIO_CALL_TIMEOUT_MS);
/// The daemon's waits between attempts, the last one repeating.
pub(crate) const LADDER_MS: [u64; 5] = [250, 500, 1000, 2000, 5000];
/// The volume the pods' sink starts at.
pub(crate) const PERCENT: u8 = 40;

/// One command the fake received, in the terms a test asserts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Asked {
    Auth {
        version: u16,
    },
    ClientName,
    ClientList,
    ServerInfo,
    SinkInfo {
        name: String,
    },
    SinkList,
    Volume {
        name: String,
        index: Option<u32>,
        percent: u8,
        at: Instant,
    },
    Subscribe {
        mask: SubscriptionMask,
    },
    CardInfo {
        name: String,
    },
    CardList,
    Profile {
        card: String,
        profile: String,
    },
    SourceList,
    DefaultSource {
        name: String,
    },
    RecorderList,
    /// `name` is `None` for an index no sink has.
    Suspend {
        name: Option<String>,
        suspend: bool,
        at: Instant,
    },
    Unknown {
        command: u32,
    },
}

impl Asked {
    pub(super) fn only_reads(&self) -> bool {
        match self {
            Self::Auth { .. }
            | Self::ClientName
            | Self::ClientList
            | Self::ServerInfo
            | Self::SinkInfo { .. }
            | Self::SinkList
            | Self::Subscribe { .. }
            | Self::CardInfo { .. }
            | Self::CardList
            | Self::SourceList
            | Self::RecorderList => true,
            Self::Volume { .. }
            | Self::Profile { .. }
            | Self::DefaultSource { .. }
            | Self::Suspend { .. }
            | Self::Unknown { .. } => false,
        }
    }
}

/// The commands in `requests` that are not reads, in order.
pub(crate) fn writes_of(requests: &[Asked]) -> Vec<Asked> {
    let writes = requests.iter().filter(|asked| !asked.only_reads());
    writes.cloned().collect()
}

/// The writes a sink assertion checks, apart from the microphone's default-source request.
pub(crate) fn writes_except_default_source(requests: &[Asked]) -> Vec<Asked> {
    let mut writes = writes_of(requests);
    writes.retain(|asked| !matches!(asked, Asked::DefaultSource { .. }));
    writes
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

    /// Applies changes, including sink removals, without sending subscription events.
    pub(crate) fn silence_events(&self) {
        self.change(|state| state.silent_events = true);
    }

    /// Replaces the sink as one server change so a list cannot see the name missing between indices.
    pub(crate) fn replace_default_sink(&self, name: &str, percent: u8) {
        self.change(|state| {
            state.remove_sink(name);
            state.add_sink(name, volume_of(percent));
            state.set_default_sink(name);
        });
    }

    /// Refuses every later default-source write with `PulseError::NoEntity`.
    pub(crate) fn refuse_default_source(&self) {
        self.change(|state| state.refuse_default_source = true);
    }

    pub(crate) fn set_default_sink(&self, name: &str) {
        self.change(|state| state.set_default_sink(name));
    }

    pub(crate) fn announce_server_change(&self) {
        self.change(State::announce_server_change);
    }

    pub(crate) fn channel_volume(&self, name: &str) -> ChannelVolume {
        self.state().channel_volume(name)
    }

    pub(crate) fn set_channel_volume(&self, name: &str, raw: &[u32]) {
        let mut volumes = ChannelVolume::empty();
        for raw in raw {
            volumes.push(Volume::from_u32_clamped(*raw));
        }
        self.change(|state| state.set_channel_volume(name, volumes));
    }

    /// As a user would set it, so every subscriber hears the change.
    pub(crate) fn set_volume(&self, name: &str, percent: u8) {
        self.change(|state| state.set_volume(name, volume_of(percent)));
    }

    /// As the sink's streams would move it, so every subscriber hears the change.
    pub(crate) fn set_sink_state(&self, name: &str, state: SinkState) {
        self.change(|state_of| state_of.set_sink_state(name, state));
    }

    pub(crate) fn add_card(&self, name: &str, profiles: &[FakeProfile], active: &str) {
        self.change(|state| state.add_card(name, profiles, active, Props::new()));
    }

    pub(crate) fn add_bluetooth_card(&self, name: &str, address: &str, device_id: &str) {
        let mut props = Props::new();
        for (key, value) in [
            (c"device.api", "bluez5"),
            (c"api.bluez5.address", address),
            (c"object.id", device_id),
        ] {
            props.set_bytes(key, CString::new(value).unwrap().as_bytes_with_nul());
        }
        let off = FakeProfile {
            name: "off",
            available: true,
            sinks: 0,
            sources: 0,
        };
        self.change(|state| state.add_card(name, &[off], "off", props));
    }

    pub(crate) fn remove_card(&self, name: &str) {
        self.change(|state| state.remove_card(name));
    }

    pub(crate) fn active_profile(&self, card: &str) -> String {
        self.state().cards.active(card).to_owned()
    }

    /// Every profile write a client sent for the card, in order, refused ones included.
    pub(crate) fn profile_writes(&self, card: &str) -> Vec<String> {
        let requests = self.requests().into_iter();
        let writes = requests.filter_map(|asked| {
            if let Asked::Profile {
                card: named,
                profile,
            } = asked
                && named == card
            {
                Some(profile)
            } else {
                None
            }
        });
        writes.collect()
    }

    /// As the user sets it, so it is announced and not recorded as a write.
    pub(crate) fn set_profile_as_user(&self, card: &str, profile: &str) {
        self.change(|state| state.set_profile_as_user(card, profile));
    }

    /// The next `times` writes of an offered profile are answered with an error and change nothing.
    pub(crate) fn refuse_profile_writes(&self, times: u32) {
        self.change(|state| state.cards.refuse(times));
    }

    pub(crate) fn add_client(&self, hint: Option<&str>) -> u32 {
        self.change(|state| state.add_client(hint))
    }

    pub(crate) fn remove_client(&self, index: u32) {
        self.change(|state| state.remove_client(index));
    }

    pub(crate) fn add_source(&self, name: &str) {
        self.change(|state| state.add_source(name));
    }

    /// A record stream on `target`: a sink for `Recorder::Monitor`, a source for every other kind, linked to none when absent.
    pub(crate) fn add_recorder(&self, target: &str, kind: Recorder) -> RecorderId {
        self.change(|state| state.add_recorder(target, kind))
    }

    pub(crate) fn remove_recorder(&self, id: RecorderId) {
        self.change(|state| state.remove_recorder(id));
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
        assert!(!self.requests().is_empty(), "the daemon asked nothing");
        let writes = self.writes();
        assert!(writes.is_empty(), "the daemon asked for {writes:?}");
    }

    /// Every command received that is not a read, in order.
    pub(crate) fn writes(&self) -> Vec<Asked> {
        writes_of(&self.requests())
    }

    pub(crate) fn wait_writes(&self, count: usize, within: Duration) -> Vec<Asked> {
        let what = format!("{count} writes");
        self.wait(&what, within, |state| {
            let writes = writes_of(&state.requests);
            (writes.len() >= count).then_some(writes)
        })
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

    /// Starts holding messages at the next card-list reply.
    pub(crate) fn hold_at_card_list(&self) {
        self.hold_at_reply(CommandTag::GetCardInfoList);
    }

    /// Starts holding messages at the next reply to `tag`.
    pub(crate) fn hold_at_reply(&self, tag: CommandTag) {
        self.change(|state| state.hold_at_reply(tag));
    }

    /// Sends one sink event even when silenced, holding every reply it prompts until released.
    pub(crate) fn request_sink_refresh_and_hold_replies(&self) {
        self.change(State::request_sink_refresh_and_hold_replies);
    }

    /// A sink change event cut inside its payload, with replies held; `hold_replies(false)` sends the rest.
    pub(crate) fn send_split_event(&self) {
        self.change(State::send_split_event);
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
