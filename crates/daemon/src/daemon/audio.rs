//! Follows the desktop's audio server while a device's link is up: reads the default sink and the device's own, and writes only the suspend and resume of a restore.

mod wire;

use std::convert::Infallible;
use std::ffi::CStr;
use std::fmt;
use std::future::poll_fn;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::task::{Context, Poll};
use std::time::Duration;

use pods_core::policy::audio::{
    AUDIO_CALL_TIMEOUT_MS, AudioFact, AudioRequest, RESTORE_HOLD_MS, SinkFact, sink_prefix,
};
use pods_proto::addr::BdAddr;
use pulseaudio::protocol::{
    AuthParams, Command, CommandTag, Prop, Props, SinkState, SubscriptionMask, SuspendParams,
};
use tokio::net::UnixStream;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};

use super::link::{Input, STOP_WAIT_MS};
use super::log;
use wire::{Frame, Malformed, Message, PROTOCOL_VERSION, read_frame, write_frame};

/// The driver asks once per link signal, and the adapter takes each request at once.
pub(super) const REQUEST_QUEUE: usize = 8;

/// A restarted server is read again within 10 s, so the largest gap is 5 s, which repeats; runs only while a device is followed.
const AUDIO_RETRY_MS: [u64; 5] = [250, 500, 1000, 2000, 5000];

/// Where `libpulse` finds the server, under the runtime directory.
const SOCKET: &str = "pulse/native";

/// What `pactl list clients` shows.
const CLIENT_NAME: &CStr = c"librepods";

/// The cookie's length in the auth request; `PipeWire`'s pulse server does not check its bytes.
const COOKIE_LENGTH: usize = 256;

/// Sinks added, removed or changed in volume, and the server's default sink.
const EVENTS: SubscriptionMask = SubscriptionMask::SINK.union(SubscriptionMask::SERVER);

const QUEUE_FULL: &str = "audio: request queue full";

const ADAPTER_GONE: &str = "audio: the adapter has ended; request dropped";

const RESTORE_SKIPPED_LINE: &str = "mic: restore skipped, sink suspended";

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

/// Follows each device a request names until the driver drops the queue, which closes the socket.
pub(super) async fn run(
    inputs: mpsc::Sender<Input>,
    mut requests: mpsc::Receiver<AudioRequest>,
    runtime_dir: PathBuf,
) {
    let socket = runtime_dir.join(SOCKET);
    let mut followed = None;
    loop {
        let request = match followed {
            None => requests.recv().await,
            Some(device) => match Follower::new(&inputs, &mut requests, device)
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
            Some(AudioRequest::Restore) => {
                if inputs
                    .send(Input::Audio(AudioFact::Restored))
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

/// What ends a follow for good.
enum Left {
    DriverGone,
    /// Another device to follow, or `None` when the driver dropped the queue.
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
    Asked,
    Resuming,
    Owed,
}

/// The driver's queue as a follower takes it: a restore is kept here, any other request ends the follow.
struct Requests<'a> {
    queue: &'a mut mpsc::Receiver<AudioRequest>,
    restore: Restore,
}

impl Requests<'_> {
    fn poll_next(&mut self, context: &mut Context<'_>) -> Poll<Option<AudioRequest>> {
        loop {
            match self.queue.poll_recv(context) {
                Poll::Ready(Some(AudioRequest::Restore)) => self.restore = Restore::Asked,
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
            if self.restore == Restore::Asked {
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
    /// Never logged: it holds the device's address.
    prefix: Vec<u8>,
    told: Told,
    /// Attempts failed since the server was last read, which picks the step of the ladder.
    misses: usize,
    /// A suspend sent and not yet undone, which the next connection resumes.
    suspended: bool,
}

impl<'a> Follower<'a> {
    fn new(
        inputs: &'a mpsc::Sender<Input>,
        queue: &'a mut mpsc::Receiver<AudioRequest>,
        device: BdAddr,
    ) -> Self {
        Self {
            inputs,
            requests: Requests {
                queue,
                restore: Restore::Clear,
            },
            prefix: sink_prefix(device).into_bytes(),
            told: Told::Nothing,
            misses: 0,
            suspended: false,
        }
    }

    async fn follow(mut self, socket: &Path) -> Left {
        loop {
            let lost = match self.serve(socket).await {
                Err(Ended::Left(left)) => return left,
                Err(Ended::Lost(lost)) => lost,
            };
            if let Err(left) = self.retry(lost).await {
                return left;
            }
        }
    }

    /// Tells the loss once an outage, answers a restore asked meanwhile as owed unless its suspend went out, and waits out the ladder's step.
    async fn retry(&mut self, lost: Lost) -> Result<(), Left> {
        if self.misses == 0 {
            log(&format!("audio: the audio server is down: {lost}"));
            // The next connection reads the sink afresh, so it is told again even when unchanged.
            self.told = Told::Nothing;
            self.tell(AudioFact::Down).await?;
        }
        let [.., longest] = AUDIO_RETRY_MS;
        let wait = AUDIO_RETRY_MS.get(self.misses).copied().unwrap_or(longest);
        let mut step = pin!(sleep(Duration::from_millis(wait)));
        loop {
            if self.requests.restore == Restore::Asked && self.suspended {
                self.requests.restore = Restore::Resuming;
            }
            if self.requests.restore == Restore::Asked {
                log(RESTORE_OWED_LINE);
                self.requests.restore = Restore::Owed;
                self.tell(AudioFact::Restored).await?;
            }
            if self.requests.waited(step.as_mut()).await?.is_some() {
                break;
            }
        }
        self.misses = self.misses.saturating_add(1);
        Ok(())
    }

    /// One connection from its connect to its loss; an event during a refresh costs one more refresh, never more.
    async fn serve(&mut self, socket: &Path) -> Result<Infallible, Ended> {
        let mut connection = self.requests.watched(Connection::open(socket)).await?;
        let mut stale = true;
        let mut read = false;
        loop {
            if !stale {
                let event = self.requests.waited(connection.next_event()).await?;
                event.transpose()?;
                stale = true;
                continue;
            }
            stale = false;
            let refresh = connection.refresh(&self.prefix, &mut stale);
            let sink = self.requests.watched(refresh).await?;
            if !read {
                read = true;
                self.misses = 0;
                self.tell(AudioFact::Up).await?;
            }
            let fact = sink.map(|sink| sink.fact);
            if self.told != Told::Sink(fact) {
                self.told = Told::Sink(fact);
                self.tell(AudioFact::Sink(fact)).await?;
            }
            if self.requests.restore != Restore::Clear || self.suspended {
                self.restore(&mut connection, sink, &mut stale).await?;
            }
        }
    }

    /// Suspends then resumes the sink with no request taken between, so none leaves it suspended; a running sink is held suspended first.
    async fn restore(
        &mut self,
        connection: &mut Connection,
        sink: Option<Followed>,
        stale: &mut bool,
    ) -> Result<(), Ended> {
        let owed = self.requests.restore != Restore::Clear;
        let is_suspended = |state| state == SinkState::Suspended;
        match sink {
            Some(Followed { index, state, .. })
                if (owed && !is_suspended(state)) || (self.suspended && is_suspended(state)) =>
            {
                let began = Instant::now();
                if !is_suspended(state) {
                    self.suspended = true;
                    connection.suspend(index, true, stale).await?;
                    if state == SinkState::Running {
                        sleep(Duration::from_millis(RESTORE_HOLD_MS)).await;
                    }
                }
                connection.suspend(index, false, stale).await?;
                let took = began.elapsed().as_millis();
                log(&format!("mic: restore done in {took} ms"));
            }
            Some(_) if owed => log(RESTORE_SKIPPED_LINE),
            _ => {}
        }
        self.suspended = false;
        let asked = matches!(self.requests.restore, Restore::Asked | Restore::Resuming);
        self.requests.restore = Restore::Clear;
        if asked {
            self.tell(AudioFact::Restored).await?;
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
        let mut stale = false;
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

    /// The device's sink, read after the default sink's name; an event in between marks it `stale`.
    async fn refresh(&mut self, prefix: &[u8], stale: &mut bool) -> Result<Option<Followed>, Lost> {
        let default = self
            .call(&Command::GetServerInfo, stale, |body| {
                Ok(wire::default_sink(body)?.map(<[u8]>::to_vec))
            })
            .await?;
        self.call(&Command::GetSinkInfoList, stale, |body| {
            device_sink(body, prefix, default.as_deref())
        })
        .await
    }

    async fn suspend(&mut self, index: u32, suspend: bool, stale: &mut bool) -> Result<(), Lost> {
        let command = Command::SuspendSink(SuspendParams {
            device_index: Some(index),
            device_name: None,
            suspend,
        });
        self.call(&command, stale, |_| Ok(())).await
    }

    /// As long as it takes: only an event or the loss ends the wait.
    async fn next_event(&mut self) -> Result<(), Lost> {
        loop {
            let frame = read_frame(&mut self.stream, &mut self.pending).await?;
            match wire::message(&frame)? {
                Message::Event => return Ok(()),
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
        stale: &mut bool,
        parse: impl FnOnce(&[u8]) -> Result<T, Malformed>,
    ) -> Result<T, Lost> {
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        within(async {
            write_frame(&mut self.stream, &Frame::request(seq, command)?).await?;
            loop {
                let frame = read_frame(&mut self.stream, &mut self.pending).await?;
                match wire::message(&frame)? {
                    Message::Event => *stale = true,
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
    state: SinkState,
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
            };
            found = Some(Followed {
                index: sink.index,
                state: sink.state,
                fact,
            });
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests;
