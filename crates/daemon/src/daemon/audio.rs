//! Follows the desktop's audio server while a device's link is up: reads the sinks, capture clients and the device's active profile, and writes only the suspend and resume of a restore.

mod streams;
mod wire;

use std::convert::Infallible;
use std::ffi::CStr;
use std::fmt;
use std::future::{self, poll_fn};
use std::io;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::task::{Context, Poll};
use std::time::Duration;

use pods_core::policy::audio::{
    AUDIO_CALL_TIMEOUT_MS, AudioFact, AudioRequest, RESTORE_HOLD_MS, Recorders, RestoreId,
    SinkFact, SinkState, card_name, sink_prefix,
};
use pods_core::state::State;
use pods_proto::addr::BdAddr;
use pulseaudio::protocol::{
    AuthParams, Command, CommandTag, Prop, Props, PulseError, SubscriptionMask, SuspendParams,
};
use tokio::net::UnixStream;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};

use super::link::{Input, STOP_WAIT_MS};
use super::log;
use wire::{Frame, Malformed, Message, PROTOCOL_VERSION, read_frame, take_frame, write_frame};

/// The driver asks once per link signal, and the adapter takes each request at once.
pub(super) const REQUEST_QUEUE: usize = 8;

/// A restarted server is read again within 10 s, so the largest gap is 5 s, which repeats; runs only while a device is followed.
pub(super) const AUDIO_RETRY_MS: [u64; 5] = [250, 500, 1000, 2000, 5000];

/// Where `libpulse` finds the server, under the runtime directory.
const SOCKET: &str = "pulse/native";

/// What `pactl list clients` shows.
const CLIENT_NAME: &CStr = c"librepods";

/// The cookie's length in the auth request; `PipeWire`'s pulse server does not check its bytes.
const COOKIE_LENGTH: usize = 256;

#[expect(
    clippy::as_conversions,
    reason = "the discriminant is the PulseAudio protocol's error code"
)]
const NO_ENTITY: u32 = PulseError::NoEntity as u32;

/// Facilities read by the follower on this connection.
const EVENTS: SubscriptionMask = SubscriptionMask::SINK
    .union(SubscriptionMask::SERVER)
    .union(SubscriptionMask::SOURCE)
    .union(SubscriptionMask::SOURCE_OUTPUT)
    .union(SubscriptionMask::CLIENT)
    .union(SubscriptionMask::CARD);

const QUEUE_FULL: &str = "audio: request queue full";

const ADAPTER_GONE: &str = "audio: the adapter has ended; request dropped";

const RESTORE_SKIPPED_LINE: &str = "mic: restore skipped, sink suspended";

const RESTORE_HEADSET_LINE: &str = "mic: restore skipped, headset profile active";

const RESTORE_OWED_LINE: &str = "mic: restore owed, no audio server";

/// The one place the queue's bound is set.
pub(super) fn queue() -> (mpsc::Sender<AudioRequest>, mpsc::Receiver<AudioRequest>) {
    mpsc::channel(REQUEST_QUEUE)
}

/// Queues `request` without waiting; a full queue or an ended adapter drops it with a log line.
pub(super) fn offer(requests: &mpsc::Sender<AudioRequest>, request: AudioRequest) {
    match requests.try_send(request) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => log(QUEUE_FULL),
        // The adapter ends on its own only after the driver, so a live driver sees this only after a panic.
        Err(TrySendError::Closed(_)) => log(ADAPTER_GONE),
    }
}

/// Follows each device a request names until the driver drops the queue, which closes the socket; `state` names the microphone's source.
pub(super) async fn run(
    inputs: mpsc::Sender<Input>,
    mut requests: mpsc::Receiver<AudioRequest>,
    runtime_dir: PathBuf,
    state: watch::Receiver<State>,
) {
    let socket = runtime_dir.join(SOCKET);
    let mut followed = None;
    loop {
        let request = match followed {
            None => requests.recv().await,
            Some(device) => match Follower::new(&inputs, &mut requests, device, state.clone())
                .follow(&socket)
                .await
            {
                Left::Request(request) => request,
                Left::DriverGone => return,
            },
        };
        match request {
            Some(AudioRequest::Follow(device)) => followed = device,
            // Nothing is followed, so no sink is left to restore.
            Some(AudioRequest::Restore(id)) => {
                if inputs
                    .send(Input::Audio(AudioFact::Restored(id)))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Some(AudioRequest::CancelRestore) => {}
            None => return,
        }
    }
}

/// Ends the adapter inside `STOP_WAIT_MS` once the driver is gone, aborting one still running.
pub(super) async fn stop(mut adapter: JoinHandle<()>) {
    match timeout(Duration::from_millis(STOP_WAIT_MS), &mut adapter).await {
        Ok(Ok(())) => {}
        Ok(Err(ended)) => log(&format!("audio: the adapter ended abnormally: {ended}")),
        Err(_) => {
            log(&format!(
                "audio: the adapter did not stop within {STOP_WAIT_MS} ms; aborting it"
            ));
            adapter.abort();
            // Awaited so the socket is closed first; the result is only the cancellation.
            drop(adapter.await);
        }
    }
}

/// The driver's queue closed, so nothing hears a fact any more.
struct DriverGone;

/// Why a connection ended, for the one log line an outage gets.
#[derive(Debug)]
enum Lost {
    Io(io::Error),
    Malformed(Malformed),
    Timeout,
    Refused { command: CommandTag, code: u32 },
    OldProtocol(u16),
    Unasked(u32),
}

impl fmt::Display for Lost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Malformed(malformed) => write!(f, "{malformed}"),
            Self::Timeout => write!(f, "no answer within {AUDIO_CALL_TIMEOUT_MS} ms"),
            Self::Refused { command, code } => {
                write!(f, "it refused {command:?} with error {code}")
            }
            Self::OldProtocol(version) => {
                write!(f, "it speaks protocol {version}, below {PROTOCOL_VERSION}")
            }
            Self::Unasked(seq) => write!(f, "an answer to request {seq}, which nothing waits for"),
        }
    }
}

impl From<io::Error> for Lost {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<Malformed> for Lost {
    fn from(malformed: Malformed) -> Self {
        Self::Malformed(malformed)
    }
}

/// What ends a connection, or the wait for the next one, other than the server's loss.
enum Left {
    DriverGone,
    /// A device to follow or none, or `None` when the driver dropped the queue.
    Request(Option<AudioRequest>),
}

impl From<DriverGone> for Left {
    fn from(DriverGone: DriverGone) -> Self {
        Self::DriverGone
    }
}

enum Ended {
    Left(Left),
    Lost(Lost),
}

impl From<Left> for Ended {
    fn from(left: Left) -> Self {
        Self::Left(left)
    }
}

impl From<DriverGone> for Ended {
    fn from(DriverGone: DriverGone) -> Self {
        Self::Left(Left::DriverGone)
    }
}

impl From<Lost> for Ended {
    fn from(lost: Lost) -> Self {
        Self::Lost(lost)
    }
}

/// A restore the driver asked for: `Asked` waits for its answer, `Resuming` too with its resume left to the next connection, and `Owed` was answered while the server was down and is done once it is back.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Restore {
    Clear,
    Asked(RestoreId),
    Resuming(RestoreId),
    Owed,
}

/// The driver's queue as a follower takes it: a restore is kept here, any other request ends the connection or the wait for the next one.
struct Requests<'a> {
    queue: &'a mut mpsc::Receiver<AudioRequest>,
    restore: Restore,
}

impl Requests<'_> {
    fn poll_next(&mut self, context: &mut Context<'_>) -> Poll<Option<AudioRequest>> {
        loop {
            match self.queue.poll_recv(context) {
                Poll::Ready(Some(AudioRequest::Restore(id))) => self.restore = Restore::Asked(id),
                Poll::Ready(Some(AudioRequest::CancelRestore)) => self.restore = Restore::Clear,
                Poll::Ready(request) => return Poll::Ready(request),
                Poll::Pending => return Poll::Pending,
            }
        }
    }

    /// Runs `work` with requests taken first, so a new one never waits on the connection it replaces.
    async fn watched<T>(
        &mut self,
        work: impl Future<Output = Result<T, Lost>>,
    ) -> Result<T, Ended> {
        let mut work = pin!(work);
        poll_fn(|context| {
            if let Poll::Ready(request) = self.poll_next(context) {
                return Poll::Ready(Err(Ended::Left(Left::Request(request))));
            }
            work.as_mut().poll(context).map_err(Ended::Lost)
        })
        .await
    }

    /// As `watched` for a wait that holds nothing, which a restore asked for ends with `None`.
    async fn waited<T>(&mut self, work: impl Future<Output = T>) -> Result<Option<T>, Left> {
        let mut work = pin!(work);
        poll_fn(|context| {
            if let Poll::Ready(request) = self.poll_next(context) {
                return Poll::Ready(Err(Left::Request(request)));
            }
            if matches!(self.restore, Restore::Asked(_)) {
                return Poll::Ready(Ok(None));
            }
            work.as_mut().poll(context).map(|done| Ok(Some(done)))
        })
        .await
    }
}

/// One device followed: its connections, one after another, and what was told of them.
struct Follower<'a> {
    inputs: &'a mpsc::Sender<Input>,
    requests: Requests<'a>,
    device: BdAddr,
    /// Never logged: it holds the device's address.
    prefix: Vec<u8>,
    /// Never logged: it holds the device's address.
    card: Vec<u8>,
    told: Told,
    /// Attempts failed since the server was last read, which picks the step of the ladder.
    misses: usize,
    up: bool,
    /// A suspend sent and not yet undone, which the next connection resumes.
    suspended: bool,
    state: watch::Receiver<State>,
    /// The record streams last told, none once the server is told down.
    recorders: Recorders,
    hint: bool,
    headset: Option<bool>,
}

impl<'a> Follower<'a> {
    fn new(
        inputs: &'a mpsc::Sender<Input>,
        queue: &'a mut mpsc::Receiver<AudioRequest>,
        device: BdAddr,
        state: watch::Receiver<State>,
    ) -> Self {
        Self {
            inputs,
            requests: Requests {
                queue,
                restore: Restore::Clear,
            },
            device,
            prefix: sink_prefix(device).into_bytes(),
            card: card_name(device).into_bytes(),
            told: Told::Nothing,
            misses: 0,
            up: false,
            suspended: false,
            state,
            recorders: Recorders::default(),
            hint: false,
            headset: None,
        }
    }

    /// Ends at a request for another device or for none; one for this device again, which every link up sends, keeps the restore and the suspend and only dials again.
    async fn follow(mut self, socket: &Path) -> Left {
        let again = Some(AudioRequest::Follow(Some(self.device)));
        loop {
            let left = match self.serve(socket).await {
                Err(Ended::Left(left)) => left,
                Err(Ended::Lost(lost)) => match self.retry(lost).await {
                    Ok(()) => continue,
                    Err(left) => left,
                },
            };
            match left {
                // The driver forgot the sink at the link's up, so it is told again even when unchanged.
                Left::Request(request) if request == again => self.told = Told::Nothing,
                Left::DriverGone | Left::Request(_) => return left,
            }
        }
    }

    /// Tells the loss once an outage, answers a restore asked meanwhile as owed unless its suspend went out, and waits out the ladder's step.
    async fn retry(&mut self, lost: Lost) -> Result<(), Left> {
        if self.misses == 0 {
            log(&format!("audio: the audio server is down: {lost}"));
        }
        if std::mem::take(&mut self.up) || self.misses == 0 {
            // The next connection reads the sink afresh, so it is told again even when unchanged.
            self.told = Told::Nothing;
            self.recorders = Recorders::default();
            self.hint = false;
            self.tell(AudioFact::Down).await?;
        }
        let [.., longest] = AUDIO_RETRY_MS;
        let wait = AUDIO_RETRY_MS.get(self.misses).copied().unwrap_or(longest);
        let mut step = pin!(sleep(Duration::from_millis(wait)));
        loop {
            if let Restore::Asked(id) = self.requests.restore {
                if self.suspended {
                    self.requests.restore = Restore::Resuming(id);
                } else {
                    log(RESTORE_OWED_LINE);
                    self.requests.restore = Restore::Owed;
                    self.tell(AudioFact::Restored(id)).await?;
                }
            }
            if self.requests.waited(step.as_mut()).await?.is_some() {
                break;
            }
        }
        self.misses = self.misses.saturating_add(1);
        Ok(())
    }

    /// One connection from its connect to its loss; events arriving during a refresh mark what the next refresh needs.
    async fn serve(&mut self, socket: &Path) -> Result<Infallible, Ended> {
        let mut connection = self.requests.watched(Connection::open(socket)).await?;
        self.headset = None;
        let mut stale = EVENTS;
        let mut read = false;
        let mut default_sink = None;
        let mut sink = None;
        let mut source_name = String::new();
        let mut source_index = None;
        loop {
            let change = if stale.is_empty() {
                let changed = connection.changed(&mut self.state, &source_name);
                self.requests.waited(changed).await?
            } else {
                self.requests.waited(future::ready(Ok(stale))).await?
            };
            let refresh = change.transpose()?.unwrap_or(EVENTS);
            stale = SubscriptionMask::empty();
            if refresh.contains(SubscriptionMask::SERVER) {
                let default = connection.call(&Command::GetServerInfo, &mut stale, |body| {
                    Ok(wire::default_sink(body)?.map(<[u8]>::to_vec))
                });
                default_sink = self.requests.watched(default).await?;
            }
            if refresh.intersects(SubscriptionMask::SERVER | SubscriptionMask::SINK) {
                let listed = connection.call(&Command::GetSinkInfoList, &mut stale, |body| {
                    device_sink(body, &self.prefix, default_sink.as_deref())
                });
                sink = self.requests.watched(listed).await?;
            }
            if !read {
                self.tell(AudioFact::Up).await?;
                self.up = true;
            }
            let headset = if refresh.contains(SubscriptionMask::CARD) {
                let listed = connection.call(&Command::GetCardInfoList, &mut stale, |body| {
                    wire::headset(body, &self.card)
                });
                self.requests.watched(listed).await?
            } else {
                self.headset.unwrap_or_default()
            };
            let hint = if refresh.contains(SubscriptionMask::CLIENT) {
                let listed = connection.call(
                    &Command::GetClientInfoList,
                    &mut stale,
                    wire::wants_fast_capture,
                );
                self.requests.watched(listed).await?
            } else {
                self.hint
            };
            if !read {
                read = true;
                self.misses = 0;
            }
            // Apply a new block before a want, and lift it only after the current want and sink are known.
            if headset {
                self.tell_headset(true).await?;
            }
            if self.hint != hint {
                self.hint = hint;
                self.tell(AudioFact::Hint(hint)).await?;
            }
            let fact = sink.map(|sink| sink.fact);
            if self.told != Told::Sink(fact) {
                self.told = Told::Sink(fact);
                self.tell(AudioFact::Sink(fact)).await?;
            }
            if !headset {
                self.tell_headset(false).await?;
            }
            self.restore(&mut connection, sink, &mut stale).await?;
            let name = self.state.borrow().mic_source();
            let reread_source = refresh.contains(SubscriptionMask::SOURCE) || name != source_name;
            if reread_source {
                source_name = name;
                source_index = if source_name.is_empty() {
                    None
                } else {
                    let listed = connection.call(&Command::GetSourceInfoList, &mut stale, |body| {
                        streams::source_index(body, source_name.as_bytes())
                    });
                    let index = self.requests.watched(listed).await?;
                    self.restore(&mut connection, sink, &mut stale).await?;
                    index
                };
            }
            if reread_source || refresh.contains(SubscriptionMask::SOURCE_OUTPUT) {
                let recorders = self
                    .record_streams(&mut connection, sink, source_index, &mut stale)
                    .await?;
                if recorders != self.recorders {
                    self.recorders = recorders;
                    self.tell(AudioFact::Recorders(recorders)).await?;
                }
            }
        }
    }

    /// The record streams on `source`; a restore asked during the read is done as it ends, on the sink as last read.
    async fn record_streams(
        &mut self,
        connection: &mut Connection,
        sink: Option<Followed>,
        source: Option<u32>,
        stale: &mut SubscriptionMask,
    ) -> Result<Recorders, Ended> {
        let Some(source) = source else {
            return Ok(Recorders::default());
        };
        let counted = |body: &[u8]| streams::recorders(body, source);
        let recorders = connection.call(&Command::GetSourceOutputInfoList, stale, counted);
        let recorders = self.requests.watched(recorders).await?;
        self.restore(connection, sink, stale).await?;
        Ok(recorders)
    }

    /// Skips restores under a headset profile and takes no request between a suspend and its resume.
    async fn restore(
        &mut self,
        connection: &mut Connection,
        sink: Option<Followed>,
        stale: &mut SubscriptionMask,
    ) -> Result<(), Ended> {
        if self.requests.restore == Restore::Clear && !self.suspended {
            return Ok(());
        }
        let owed = self.requests.restore != Restore::Clear;
        let is_suspended = |state| state == SinkState::Suspended;
        match sink {
            _ if self.headset == Some(true) => log(RESTORE_HEADSET_LINE),
            Some(Followed { index, fact })
                if (owed && !is_suspended(fact.state))
                    || (self.suspended && is_suspended(fact.state)) =>
            {
                let began = Instant::now();
                let restored = async {
                    if !is_suspended(fact.state) {
                        self.suspended = true;
                        connection.suspend(index, true, stale).await?;
                        if fact.state == SinkState::Running {
                            sleep(Duration::from_millis(RESTORE_HOLD_MS)).await;
                        }
                    }
                    connection.suspend(index, false, stale).await
                }
                .await;
                match restored {
                    Ok(()) => {
                        let took = began.elapsed().as_millis();
                        log(&format!("mic: restore done in {took} ms"));
                    }
                    // The sink vanished since it was read, so refresh and settle as for a missing sink.
                    Err(Lost::Refused {
                        command: CommandTag::SuspendSink,
                        code: NO_ENTITY,
                    }) => *stale |= SubscriptionMask::SINK,
                    Err(lost) => return Err(lost.into()),
                }
            }
            Some(_) if owed => log(RESTORE_SKIPPED_LINE),
            _ => {}
        }
        self.suspended = false;
        let asked = match self.requests.restore {
            Restore::Asked(id) | Restore::Resuming(id) => Some(id),
            Restore::Clear | Restore::Owed => None,
        };
        self.requests.restore = Restore::Clear;
        if let Some(id) = asked {
            self.tell(AudioFact::Restored(id)).await?;
        }
        Ok(())
    }

    async fn tell_headset(&mut self, headset: bool) -> Result<(), DriverGone> {
        if self.headset != Some(headset) {
            self.headset = Some(headset);
            self.tell(AudioFact::Headset(headset)).await?;
        }
        Ok(())
    }

    async fn tell(&self, fact: AudioFact) -> Result<(), DriverGone> {
        if self.inputs.send(Input::Audio(fact)).await.is_err() {
            return Err(DriverGone);
        }
        Ok(())
    }
}

/// The last sink fact sent, so an unchanged one is not sent again.
#[derive(PartialEq, Eq)]
enum Told {
    Nothing,
    Sink(Option<SinkFact>),
}

/// One connection past its opening exchange.
struct Connection {
    stream: UnixStream,
    seq: u32,
    pending: Vec<u8>,
}

impl Connection {
    /// Connects, authorises, names the client and subscribes, each step within the call timeout.
    async fn open(socket: &Path) -> Result<Self, Lost> {
        let stream = within(async { Ok(UnixStream::connect(socket).await?) }).await?;
        let mut connection = Self {
            stream,
            seq: 0,
            pending: Vec::new(),
        };
        // Before the subscription no event can come.
        let mut stale = SubscriptionMask::empty();
        let auth = Command::Auth(AuthParams {
            version: PROTOCOL_VERSION,
            cookie: vec![0; COOKIE_LENGTH],
            ..AuthParams::default()
        });
        let granted = connection
            .call(&auth, &mut stale, wire::auth_version)
            .await?;
        if granted < PROTOCOL_VERSION {
            return Err(Lost::OldProtocol(granted));
        }
        let mut name = Props::new();
        name.set(Prop::ApplicationName, CLIENT_NAME);
        let named = Command::SetClientName(name);
        connection.call(&named, &mut stale, |_| Ok(())).await?;
        let subscribe = Command::Subscribe(EVENTS);
        connection.call(&subscribe, &mut stale, |_| Ok(())).await?;
        Ok(connection)
    }

    async fn suspend(
        &mut self,
        index: u32,
        suspend: bool,
        stale: &mut SubscriptionMask,
    ) -> Result<(), Lost> {
        let command = Command::SuspendSink(SuspendParams {
            device_index: Some(index),
            device_name: None,
            suspend,
        });
        self.call(&command, stale, |_| Ok(())).await
    }

    /// As `next_event`, ended also by the source's name moving from the `read` one, which no event of the server tells.
    async fn changed(
        &mut self,
        state: &mut watch::Receiver<State>,
        read: &str,
    ) -> Result<SubscriptionMask, Lost> {
        let mut renamed = pin!(async {
            let moved = state.wait_for(|state| state.mic_source() != read);
            let hub_gone = moved.await.is_err();
            // corner: the daemon is stopping, so the name moves no more
            if hub_gone {
                future::pending::<()>().await;
            }
        });
        let mut event = pin!(self.next_event());
        poll_fn(|context| {
            if renamed.as_mut().poll(context).is_ready() {
                return Poll::Ready(Ok(SubscriptionMask::SOURCE));
            }
            event.as_mut().poll(context)
        })
        .await
    }

    /// Waits for an event or the loss, folding in any other events already buffered with it.
    async fn next_event(&mut self) -> Result<SubscriptionMask, Lost> {
        let mut changed = SubscriptionMask::empty();
        loop {
            let frame = if changed.is_empty() {
                read_frame(&mut self.stream, &mut self.pending).await?
            } else if let Some(frame) = take_frame(&mut self.pending)? {
                frame
            } else {
                return Ok(changed);
            };
            match wire::message(&frame)? {
                Message::Event(event) => changed |= event.unwrap_or(EVENTS),
                Message::Other => {}
                Message::Reply { seq, .. } | Message::Refused { seq, .. } => {
                    return Err(Lost::Unasked(seq));
                }
            }
        }
    }

    /// Sends `command` and reads to its answer within the call timeout, an event meanwhile marking `stale`.
    async fn call<T>(
        &mut self,
        command: &Command,
        stale: &mut SubscriptionMask,
        parse: impl FnOnce(&[u8]) -> Result<T, Malformed>,
    ) -> Result<T, Lost> {
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        within(async {
            write_frame(&mut self.stream, &Frame::request(seq, command)?).await?;
            loop {
                let frame = read_frame(&mut self.stream, &mut self.pending).await?;
                match wire::message(&frame)? {
                    Message::Event(event) => *stale |= event.unwrap_or(EVENTS),
                    Message::Other => {}
                    Message::Reply {
                        seq: answered,
                        body,
                    } if answered == seq => {
                        return Ok(parse(body)?);
                    }
                    Message::Refused {
                        seq: answered,
                        code,
                    } if answered == seq => {
                        let command = command.tag();
                        return Err(Lost::Refused { command, code });
                    }
                    Message::Reply { seq: answered, .. }
                    | Message::Refused { seq: answered, .. } => {
                        return Err(Lost::Unasked(answered));
                    }
                }
            }
        })
        .await
    }
}

async fn within<T>(work: impl Future<Output = Result<T, Lost>>) -> Result<T, Lost> {
    timeout(Duration::from_millis(AUDIO_CALL_TIMEOUT_MS), work)
        .await
        .unwrap_or(Err(Lost::Timeout))
}

/// The followed device's sink: what the driver is told, and where a restore addresses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Followed {
    index: u32,
    fact: SinkFact,
}

/// The device's sink: the default when its name starts with `prefix`, else the first sink whose name does.
fn device_sink(
    body: &[u8],
    prefix: &[u8],
    default: Option<&[u8]>,
) -> Result<Option<Followed>, Malformed> {
    let mut found = None;
    for sink in wire::sinks(body) {
        let sink = sink?;
        if !sink.name.starts_with(prefix) {
            continue;
        }
        let is_default = default == Some(sink.name);
        if is_default || found.is_none() {
            let fact = SinkFact {
                is_default,
                volume: sink.volume,
                state: sink.state,
            };
            found = Some(Followed {
                index: sink.index,
                fact,
            });
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests;
