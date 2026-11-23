//! The fake audio server's state and threads: one listener, one thread per client, and the commands it answers.

use std::ffi::{CStr, CString};
use std::io::{ErrorKind, Write};
use std::mem;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use pulseaudio::protocol::{
    AuthReply, ChannelVolume, Command, CommandTag, DEFAULT_SINK, GetCardInfo, GetSinkInfo,
    MAX_VERSION, PulseError, ServerInfo, SetCardProfileParams, SetClientNameReply,
    SetDeviceVolumeParams, SinkInfo, SinkState, SubscriptionEvent, SubscriptionEventFacility,
    SubscriptionEventType, SubscriptionMask, Volume,
};

use super::Asked;
use super::capture::{Capture, Recorder, RecorderId, monitor_name};
use super::cards::{Cards, FakeProfile};
use super::wire::{self, CONTROL_CHANNEL, percent_of};

/// A test opens a handful of clients at once.
const CLIENTS_MAX: usize = 16;
/// Far past what one test asks; more is a client asking in a loop.
const REQUESTS_MAX: usize = 4096;
/// Messages kept for one client while replies are held.
const HELD_MAX: usize = 64;
/// A client that reads nothing for this long is stuck, and the fake records a fault.
const WRITE_LIMIT: Duration = Duration::from_secs(2);
/// Stereo, as `SinkInfo::new_dummy` describes every sink.
const SINK_CHANNELS: usize = 2;
/// The commands the fake answers; any other is refused with `PulseError::Command`.
const UNDERSTOOD: [CommandTag; 12] = [
    CommandTag::Auth,
    CommandTag::SetClientName,
    CommandTag::GetServerInfo,
    CommandTag::GetSinkInfo,
    CommandTag::GetSinkInfoList,
    CommandTag::SetSinkVolume,
    CommandTag::Subscribe,
    CommandTag::GetCardInfo,
    CommandTag::GetCardInfoList,
    CommandTag::SetCardProfile,
    CommandTag::GetSourceInfoList,
    CommandTag::GetSourceOutputInfoList,
];

pub(super) struct Shared {
    state: Mutex<State>,
    pub(super) changed: Condvar,
}

struct Sink {
    index: u32,
    name: CString,
    volume: Volume,
    added: Volume,
    monitor: u32,
    running: bool,
}

struct Client {
    id: u32,
    /// The negotiated protocol version, `None` until the client authorised.
    version: Option<u16>,
    subscribed: SubscriptionMask,
    /// A second handle on the socket, for writes from any thread and for `shutdown` on stop.
    stream: UnixStream,
    held: Vec<Vec<u8>>,
}

#[derive(Default)]
pub(super) struct State {
    sinks: Vec<Sink>,
    default: Option<u32>,
    next_sink: u32,
    pub(super) cards: Cards,
    pub(super) capture: Capture,
    pub(super) requests: Vec<Asked>,
    clients: Vec<Client>,
    next_client: u32,
    threads: Vec<JoinHandle<()>>,
    hold: bool,
    pub(super) stopping: bool,
    pub(super) fault: Option<String>,
}

impl Shared {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::default(),
            changed: Condvar::new(),
        })
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `change` under the lock and wakes every wait.
    pub(super) fn change<T>(&self, change: impl FnOnce(&mut State) -> T) -> T {
        let result = change(&mut self.lock());
        self.changed.notify_all();
        result
    }
}

impl Client {
    fn version(&self) -> u16 {
        self.version.unwrap_or(MAX_VERSION)
    }

    fn send(&mut self, hold: bool, bytes: Vec<u8>) -> Result<(), String> {
        if !hold {
            return self.write(&bytes);
        }
        if self.held.len() >= HELD_MAX {
            return Err(format!("more than {HELD_MAX} messages held for one client"));
        }
        self.held.push(bytes);
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        match self.stream.write_all(bytes) {
            Ok(()) => Ok(()),
            // corner: the client left; its own thread sees the close and removes it
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::BrokenPipe | ErrorKind::ConnectionReset
                ) =>
            {
                Ok(())
            }
            Err(error) => {
                // corner: shut so every later write fails at once instead of waiting the limit again
                drop(self.stream.shutdown(std::net::Shutdown::Both));
                Err(format!(
                    "writing to client {}: {error}, limit {WRITE_LIMIT:?}",
                    self.id
                ))
            }
        }
    }
}

impl Sink {
    fn info(&self) -> SinkInfo {
        let mut cvolume = ChannelVolume::empty();
        for _ in 0..SINK_CHANNELS {
            cvolume.push(self.volume);
        }
        let monitor = monitor_name(&self.name.to_string_lossy());
        SinkInfo {
            name: self.name.clone(),
            cvolume,
            state: if self.running {
                SinkState::Running
            } else {
                SinkState::Idle
            },
            monitor_source_index: Some(self.monitor),
            monitor_source_name: Some(CString::new(monitor).unwrap()),
            ..SinkInfo::new_dummy(self.index)
        }
    }
}

impl State {
    pub(super) fn fail(&mut self, fault: String) {
        self.fault.get_or_insert(fault);
    }

    fn record(&mut self, asked: Asked) {
        if self.requests.len() >= REQUESTS_MAX {
            self.fail(format!("more than {REQUESTS_MAX} requests"));
            return;
        }
        self.requests.push(asked);
    }

    pub(super) fn clients(&self) -> usize {
        self.clients.len()
    }

    fn client(&mut self, id: u32) -> &mut Client {
        self.clients
            .iter_mut()
            .find(|client| client.id == id)
            .expect("a serving client stays registered until its thread ends")
    }

    fn send(&mut self, id: u32, bytes: Vec<u8>) {
        let hold = self.hold;
        if let Err(fault) = self.client(id).send(hold, bytes) {
            self.fail(fault);
        }
    }

    /// Sends `message` to every client `to` picks, each in its own protocol version.
    fn send_each(&mut self, to: impl Fn(&Client) -> bool, message: impl Fn(u16) -> Vec<u8>) {
        let hold = self.hold;
        let mut faults = Vec::new();
        for client in self.clients.iter_mut().filter(|client| to(client)) {
            let bytes = message(client.version());
            faults.extend(client.send(hold, bytes).err());
        }
        for fault in faults {
            self.fail(fault);
        }
    }

    /// A real server posts the event to each subscriber whose mask has the facility's bit.
    #[expect(
        clippy::as_conversions,
        reason = "the discriminant of a fieldless enum is the protocol's facility number"
    )]
    fn announce(
        &mut self,
        facility: SubscriptionEventFacility,
        kind: SubscriptionEventType,
        index: Option<u32>,
    ) {
        let mask = SubscriptionMask::from_bits_truncate(1 << facility as u32);
        let event = Command::SubscribeEvent(SubscriptionEvent {
            event_facility: facility,
            event_type: kind,
            index,
        });
        self.send_each(
            |client| client.subscribed.contains(mask),
            |version| wire::event(&event, version),
        );
    }

    fn position(&self, name: &CStr) -> Option<usize> {
        if name == DEFAULT_SINK {
            return self
                .sinks
                .iter()
                .position(|sink| Some(sink.index) == self.default);
        }
        self.sinks
            .iter()
            .position(|sink| sink.name.as_c_str() == name)
    }

    fn named(&self, name: &str) -> usize {
        let name = CString::new(name).unwrap();
        self.position(&name)
            .unwrap_or_else(|| panic!("no sink named {name:?}"))
    }

    pub(super) fn add_sink(&mut self, name: &str, volume: Volume) {
        let c_name = CString::new(name).unwrap();
        assert!(
            self.position(&c_name).is_none(),
            "a second sink named {name:?}"
        );
        let index = self.next_sink;
        self.next_sink = self.next_sink.saturating_add(1);
        let monitor = self.capture.add_monitor(name, index);
        self.sinks.push(Sink {
            index,
            name: c_name,
            volume,
            added: volume,
            monitor,
            running: false,
        });
        self.announce(
            SubscriptionEventFacility::Sink,
            SubscriptionEventType::New,
            Some(index),
        );
    }

    pub(super) fn remove_sink(&mut self, name: &str) {
        let index = self.sinks.remove(self.named(name)).index;
        self.capture.remove_monitor(index);
        self.announce(
            SubscriptionEventFacility::Sink,
            SubscriptionEventType::Removed,
            Some(index),
        );
        if self.default == Some(index) {
            // corner: a real server ranks the remaining sinks; the fake takes the earliest added
            self.default = self.sinks.first().map(|sink| sink.index);
            self.announce(
                SubscriptionEventFacility::Server,
                SubscriptionEventType::Changed,
                None,
            );
        }
    }

    pub(super) fn set_default_sink(&mut self, name: &str) {
        let index = self.sinks[self.named(name)].index;
        if self.default != Some(index) {
            self.default = Some(index);
            self.announce(
                SubscriptionEventFacility::Server,
                SubscriptionEventType::Changed,
                None,
            );
        }
    }

    pub(super) fn volume(&self, name: &str) -> Volume {
        self.sinks[self.named(name)].volume
    }

    pub(super) fn set_volume(&mut self, name: &str, volume: Volume) {
        self.change_volume(self.named(name), volume);
    }

    /// A real server announces a volume only when it changed.
    fn change_volume(&mut self, position: usize, volume: Volume) {
        let sink = &mut self.sinks[position];
        if sink.volume != volume {
            sink.volume = volume;
            let index = sink.index;
            self.announce(
                SubscriptionEventFacility::Sink,
                SubscriptionEventType::Changed,
                Some(index),
            );
        }
    }

    pub(super) fn set_sink_running(&mut self, name: &str, running: bool) {
        let position = self.named(name);
        let sink = &mut self.sinks[position];
        if sink.running != running {
            sink.running = running;
            let index = sink.index;
            self.announce(
                SubscriptionEventFacility::Sink,
                SubscriptionEventType::Changed,
                Some(index),
            );
        }
    }

    pub(super) fn add_card(&mut self, name: &str, profiles: &[FakeProfile], active: &str) {
        let index = self.cards.add(name, profiles, active);
        self.announce(
            SubscriptionEventFacility::Card,
            SubscriptionEventType::New,
            Some(index),
        );
    }

    pub(super) fn set_profile_as_user(&mut self, card: &str, profile: &str) {
        if let Some(index) = self.cards.set(card, profile) {
            self.announce_card(index);
        }
    }

    pub(super) fn add_recorder(&mut self, target: &str, kind: Recorder) -> RecorderId {
        let id = self.capture.add(target, kind);
        self.announce(
            SubscriptionEventFacility::SourceOutput,
            SubscriptionEventType::New,
            Some(id.0),
        );
        id
    }

    pub(super) fn remove_recorder(&mut self, id: RecorderId) {
        let index = self.capture.remove(id);
        self.announce(
            SubscriptionEventFacility::SourceOutput,
            SubscriptionEventType::Removed,
            Some(index),
        );
    }

    fn announce_card(&mut self, index: u32) {
        self.announce(
            SubscriptionEventFacility::Card,
            SubscriptionEventType::Changed,
            Some(index),
        );
    }

    pub(super) fn forget_volumes(&mut self) {
        for sink in &mut self.sinks {
            sink.volume = sink.added;
        }
    }

    pub(super) fn hold(&mut self, hold: bool) {
        self.hold = hold;
        if hold {
            return;
        }
        let mut faults = Vec::new();
        for client in &mut self.clients {
            for bytes in mem::take(&mut client.held) {
                faults.extend(client.write(&bytes).err());
            }
        }
        for fault in faults {
            self.fail(fault);
        }
    }

    pub(super) fn send_oversized_frame(&mut self) {
        self.send_each(|_| true, |_| wire::oversized_frame());
    }

    /// Shuts every client's socket so its thread reads the close, and hands over the threads to join.
    pub(super) fn close_clients(&mut self) -> Vec<JoinHandle<()>> {
        for client in &self.clients {
            // corner: a client that already left has a socket that refuses shutdown
            drop(client.stream.shutdown(std::net::Shutdown::Both));
        }
        mem::take(&mut self.threads)
    }

    fn admit(&mut self, shared: &Arc<Shared>, stream: UnixStream) {
        let (finished, running) = mem::take(&mut self.threads)
            .into_iter()
            .partition::<Vec<_>, _>(JoinHandle::is_finished);
        self.threads = running;
        for thread in finished {
            if thread.join().is_err() {
                self.fail("a client thread panicked".to_owned());
            }
        }
        if self.clients.len() >= CLIENTS_MAX {
            self.fail(format!("more than {CLIENTS_MAX} clients at once"));
            return;
        }
        stream.set_write_timeout(Some(WRITE_LIMIT)).unwrap();
        let id = self.next_client;
        self.next_client = self.next_client.saturating_add(1);
        self.clients.push(Client {
            id,
            version: None,
            subscribed: SubscriptionMask::empty(),
            stream: stream.try_clone().unwrap(),
            held: Vec::new(),
        });
        let shared = Arc::clone(shared);
        self.threads
            .push(thread::spawn(move || serve(&shared, id, stream)));
    }

    #[expect(
        clippy::as_conversions,
        reason = "the discriminant of a fieldless enum is the protocol's command number"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "the fake answers twelve of the protocol's commands and refuses every other alike"
    )]
    fn answer(&mut self, id: u32, frame: &[u8]) -> Result<(), String> {
        let (channel, command, seq) = wire::header(frame)?;
        if channel != CONTROL_CHANNEL {
            return Err(format!("client {id} sent stream data on channel {channel}"));
        }
        if !UNDERSTOOD
            .iter()
            .any(|tag| u32::from(*tag as u8) == command)
        {
            self.record(Asked::Unknown { command });
            self.send(id, wire::error(seq, PulseError::Command));
            return Ok(());
        }
        let version = self.client(id).version;
        let parsed = wire::command(frame, version.unwrap_or(MAX_VERSION))?;
        self.record(asked(command, &parsed));
        if let Command::Auth(params) = &parsed {
            let version = params.version.min(MAX_VERSION);
            self.client(id).version = Some(version);
            let reply = AuthReply {
                version,
                ..AuthReply::default()
            };
            self.send(id, wire::reply(seq, &reply, version));
            return Ok(());
        }
        let Some(version) = version else {
            self.send(id, wire::error(seq, PulseError::AccessDenied));
            return Ok(());
        };
        let bytes = match parsed {
            Command::SetClientName(_) => {
                wire::reply(seq, &SetClientNameReply { client_id: id }, version)
            }
            Command::GetServerInfo => {
                let default_sink_name = self
                    .default
                    .and_then(|index| self.sinks.iter().find(|sink| sink.index == index))
                    .map(|sink| sink.name.clone());
                let info = ServerInfo {
                    default_sink_name,
                    ..ServerInfo::default()
                };
                wire::reply(seq, &info, version)
            }
            Command::GetSinkInfo(GetSinkInfo {
                index: None,
                name: Some(name),
            }) => match self.position(&name) {
                Some(position) => wire::reply(seq, &self.sinks[position].info(), version),
                None => wire::error(seq, PulseError::NoEntity),
            },
            Command::GetSinkInfoList => {
                let sinks: Vec<SinkInfo> = self.sinks.iter().map(Sink::info).collect();
                wire::reply(seq, &sinks, version)
            }
            Command::SetSinkVolume(SetDeviceVolumeParams {
                device_index: None,
                device_name: Some(name),
                volume,
            }) => {
                self.write_volume(id, seq, &name, &volume);
                return Ok(());
            }
            Command::Subscribe(mask) => {
                self.client(id).subscribed = mask;
                wire::ack(seq)
            }
            Command::GetCardInfo(GetCardInfo {
                index: None,
                name: Some(name),
            }) => match self.cards.info(&name) {
                Some(card) => wire::reply(seq, &card, version),
                None => wire::error(seq, PulseError::NoEntity),
            },
            Command::GetCardInfoList => wire::reply(seq, &self.cards.list(), version),
            Command::SetCardProfile(SetCardProfileParams {
                card_index: None,
                card_name: Some(card),
                profile_name,
            }) => {
                self.write_profile(id, seq, &card, &profile_name);
                return Ok(());
            }
            Command::GetSourceInfoList => wire::reply(seq, &self.capture.sources(), version),
            Command::GetSourceOutputInfoList => {
                wire::reply(seq, &self.capture.recorders(), version)
            }
            _ => wire::error(seq, PulseError::Command),
        };
        self.send(id, bytes);
        Ok(())
    }

    /// The ack goes out before the change event, as a real server sends them.
    fn write_volume(&mut self, id: u32, seq: u32, name: &CStr, volume: &ChannelVolume) {
        let channels = volume.channels();
        if channels.len() != 1 && channels.len() != SINK_CHANNELS {
            self.send(id, wire::error(seq, PulseError::Invalid));
            return;
        }
        let Some(position) = self.position(name) else {
            self.send(id, wire::error(seq, PulseError::NoEntity));
            return;
        };
        self.send(id, wire::ack(seq));
        self.change_volume(position, loudest(volume));
    }

    /// The ack goes out before the change event, as for a volume.
    fn write_profile(&mut self, id: u32, seq: u32, card: &CStr, profile: &CStr) {
        match self.cards.write(card, profile) {
            Ok(changed) => {
                self.send(id, wire::ack(seq));
                if let Some(index) = changed {
                    self.announce_card(index);
                }
            }
            Err(error) => self.send(id, wire::error(seq, error)),
        }
    }
}

/// What the test sees of a command the fake understands; one addressed by index is not understood.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "the fake names twelve of the protocol's commands and records every other by number"
)]
fn asked(command: u32, parsed: &Command) -> Asked {
    match parsed {
        Command::Auth(params) => Asked::Auth {
            version: params.version,
        },
        Command::SetClientName(_) => Asked::ClientName,
        Command::GetServerInfo => Asked::ServerInfo,
        Command::GetSinkInfo(GetSinkInfo {
            index: None,
            name: Some(name),
        }) => Asked::SinkInfo {
            name: name.to_string_lossy().into_owned(),
        },
        Command::GetSinkInfoList => Asked::SinkList,
        Command::SetSinkVolume(SetDeviceVolumeParams {
            device_index: None,
            device_name: Some(name),
            volume,
        }) => Asked::Volume {
            name: name.to_string_lossy().into_owned(),
            percent: percent_of(loudest(volume)),
        },
        Command::Subscribe(mask) => Asked::Subscribe { mask: *mask },
        Command::GetCardInfo(GetCardInfo {
            index: None,
            name: Some(name),
        }) => Asked::CardInfo {
            name: name.to_string_lossy().into_owned(),
        },
        Command::GetCardInfoList => Asked::CardList,
        Command::SetCardProfile(SetCardProfileParams {
            card_index: None,
            card_name: Some(card),
            profile_name,
        }) => Asked::Profile {
            card: card.to_string_lossy().into_owned(),
            profile: profile_name.to_string_lossy().into_owned(),
        },
        Command::GetSourceInfoList => Asked::SourceList,
        Command::GetSourceOutputInfoList => Asked::RecorderList,
        _ => Asked::Unknown { command },
    }
}

/// The sink's one volume is its loudest channel, as `pactl` reports it.
fn loudest(volume: &ChannelVolume) -> Volume {
    volume
        .channels()
        .iter()
        .copied()
        .max_by_key(Volume::as_u32)
        .unwrap_or(Volume::MUTED)
}

/// The listening socket, kept so `stop` can shut it and wake the accept, and the thread accepting on it.
pub(super) fn listen(shared: &Arc<Shared>, socket: &Path) -> (UnixListener, JoinHandle<()>) {
    let listener = UnixListener::bind(socket)
        .unwrap_or_else(|error| panic!("binding {}: {error}", socket.display()));
    let kept = listener.try_clone().unwrap();
    let shared = Arc::clone(shared);
    (kept, thread::spawn(move || accept(&shared, &listener)))
}

fn accept(shared: &Arc<Shared>, listener: &UnixListener) {
    for stream in listener.incoming() {
        let accepting = shared.change(|state| {
            if state.stopping {
                return false;
            }
            match stream {
                Ok(stream) => {
                    state.admit(shared, stream);
                    true
                }
                Err(error) => {
                    state.fail(format!("accepting a client: {error}"));
                    false
                }
            }
        });
        if !accepting {
            return;
        }
    }
}

/// Unregisters the client however `serve` ends, a panic included, so its peer reads the close.
struct Leave<'a> {
    shared: &'a Shared,
    id: u32,
}

impl Drop for Leave<'_> {
    fn drop(&mut self) {
        let id = self.id;
        self.shared.change(|state| {
            if thread::panicking() {
                state.fail(format!("the thread serving client {id} panicked"));
            }
            state.clients.retain(|client| client.id != id);
        });
    }
}

fn serve(shared: &Shared, id: u32, mut stream: UnixStream) {
    let _leave = Leave { shared, id };
    loop {
        let frame = match wire::read_frame(&mut stream) {
            Ok(frame) => frame,
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset
                ) =>
            {
                break;
            }
            Err(error) => {
                // corner: a stop shuts the socket under a half read frame, which is not the client's fault
                shared.change(|state| {
                    if !state.stopping {
                        state.fail(format!("reading client {id}: {error}"));
                    }
                });
                break;
            }
        };
        let answered = shared.change(|state| state.answer(id, &frame));
        if let Err(fault) = answered {
            shared.change(|state| state.fail(format!("client {id}: {fault}")));
            break;
        }
    }
}
