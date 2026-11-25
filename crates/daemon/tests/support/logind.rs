//! A fake logind on the private system bus, and an impostor that sends its signal under another name.

use std::future::poll_fn;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use tokio::io::AsyncReadExt;
use tokio::net::unix::pipe;

use tokio::runtime::{Builder as RuntimeBuilder, Runtime};
use tokio::sync::oneshot;
use zbus::connection::Builder;
use zbus::export::futures_core::Stream;
use zbus::message::Type;
use zbus::names::MemberName;
use zbus::{Connection, MessageStream, OwnedFd};

use super::bus::{Bus, in_time};

pub(crate) const LOGIND_NAME: &str = "org.freedesktop.login1";
pub(crate) const MANAGER_PATH: &str = "/org/freedesktop/login1";
pub(crate) const MANAGER_INTERFACE: &str = "org.freedesktop.login1.Manager";
pub(crate) const SLEEP_SIGNAL: &str = "PrepareForSleep";
pub(crate) const INHIBIT: &str = "Inhibit";

/// The call the fake sends itself to mark the end of what reached it before.
const MARKER: &str = "FixtureMarker";

/// One connection to the bus and the runtime that drives it; dropping it ends every task the connection started.
struct Sender {
    connection: Connection,
    runtime: Arc<Runtime>,
}

/// Owns `org.freedesktop.login1` on the private system bus and answers delay-inhibitor requests.
pub(crate) struct FakeLogind {
    sender: Sender,
    state: Arc<Mutex<State>>,
    worker: Option<(oneshot::Sender<()>, JoinHandle<()>)>,
}

#[derive(Default)]
struct State {
    calls: Vec<String>,
    refuse: bool,
    held_inhibit: Option<oneshot::Receiver<()>>,
    inhibitors: Vec<Option<Instant>>,
}

/// Connected under its unique name only, never asking for the logind name.
pub(crate) struct Impostor {
    sender: Sender,
}

impl Sender {
    fn connect(bus: &Bus) -> Self {
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let connection = in_time(&runtime, Builder::address(bus.address.as_str()).build()).unwrap();
        Self {
            connection,
            runtime: Arc::new(runtime),
        }
    }

    fn prepare_for_sleep(&self, going_down: bool) {
        let emitted = self.connection.emit_signal(
            None::<&str>,
            MANAGER_PATH,
            MANAGER_INTERFACE,
            SLEEP_SIGNAL,
            &going_down,
        );
        in_time(&self.runtime, emitted).unwrap();
    }
}

impl FakeLogind {
    pub(crate) fn start(bus: &Bus) -> Self {
        let sender = Sender::connect(bus);
        let inbox = MessageStream::from(&sender.connection);
        in_time(&sender.runtime, sender.connection.request_name(LOGIND_NAME)).unwrap();
        let state = Arc::new(Mutex::new(State::default()));
        let serving = Arc::clone(&state);
        let connection = sender.connection.clone();
        let runtime = Arc::clone(&sender.runtime);
        let (stop, stopped) = oneshot::channel();
        let worker =
            thread::spawn(move || runtime.block_on(serve(connection, inbox, serving, stopped)));
        Self {
            sender,
            state,
            worker: Some((stop, worker)),
        }
    }

    /// All method calls received by the fake, with a bus marker to flush calls already queued.
    pub(crate) fn method_calls(&self) -> Vec<String> {
        let connection = &self.sender.connection;
        let marker = connection.call_method(
            connection.unique_name(),
            MANAGER_PATH,
            Some(MANAGER_INTERFACE),
            MARKER,
            &(),
        );
        in_time(&self.sender.runtime, marker).unwrap();
        self.state.lock().unwrap().calls.clone()
    }

    pub(crate) fn refuse_inhibit(&self) {
        self.state.lock().unwrap().refuse = true;
    }

    /// Holds the next Inhibit reply until the test sends on the returned handle.
    pub(crate) fn hold_inhibit(&self) -> oneshot::Sender<()> {
        let (release, held) = oneshot::channel();
        let mut state = self.state.lock().unwrap();
        assert!(state.held_inhibit.is_none());
        state.held_inhibit = Some(held);
        release
    }

    /// One entry per granted pipe, open until the fake observes EOF and records its time.
    pub(crate) fn inhibitors(&self) -> Vec<Option<Instant>> {
        self.state.lock().unwrap().inhibitors.clone()
    }

    pub(crate) fn prepare_for_sleep(&self, going_down: bool) {
        self.sender.prepare_for_sleep(going_down);
    }

    pub(crate) fn drop_name(&self) {
        let released = in_time(
            &self.sender.runtime,
            self.sender.connection.release_name(LOGIND_NAME),
        );
        assert!(
            released.unwrap(),
            "the fake logind did not own {LOGIND_NAME}"
        );
    }
}

impl Drop for FakeLogind {
    fn drop(&mut self) {
        if let Some((stop, worker)) = self.worker.take() {
            stop.send(()).ok();
            let ended = worker.join();
            if !thread::panicking() {
                ended.unwrap();
            }
        }
    }
}

async fn serve(
    connection: Connection,
    mut inbox: MessageStream,
    state: Arc<Mutex<State>>,
    mut stopped: oneshot::Receiver<()>,
) {
    let destination = connection.unique_name().unwrap().as_str();
    while let Some(Ok(message)) = poll_fn(|context| {
        if Pin::new(&mut stopped).poll(context).is_ready() {
            return Poll::Ready(None);
        }
        Pin::new(&mut inbox).poll_next(context)
    })
    .await
    {
        let header = message.header();
        if header.message_type() != Type::MethodCall {
            continue;
        }
        let member = header.member().map_or("", MemberName::as_str);
        if member == MARKER
            && header
                .sender()
                .is_some_and(|name| name.as_str() == destination)
        {
            connection.reply(&header, &()).await.unwrap();
            continue;
        }
        let refuse = {
            let mut state = state.lock().unwrap();
            state.calls.push(member.to_owned());
            state.refuse
        };
        if member != INHIBIT {
            continue;
        }
        assert_eq!(header.path().unwrap().as_str(), MANAGER_PATH);
        assert_eq!(header.interface().unwrap().as_str(), MANAGER_INTERFACE);
        // Sample request: ("sleep", "librepods", "Stop dictation before sleep", "delay").
        let body = message.body();
        let (what, who, why, mode): (&str, &str, &str, &str) = body.deserialize().unwrap();
        assert_eq!((what, mode), ("sleep", "delay"));
        assert!(!who.is_empty() && !why.is_empty());
        if refuse {
            connection
                .reply_error(
                    &header,
                    "org.freedesktop.DBus.Error.AccessDenied",
                    &"inhibitor refused",
                )
                .await
                .unwrap();
        } else {
            let held = state.lock().unwrap().held_inhibit.take();
            let connection = connection.clone();
            let state = Arc::clone(&state);
            drop(tokio::spawn(async move {
                if let Some(held) = held
                    && held.await.is_err()
                {
                    return;
                }
                let (write, mut read) = pipe::pipe().unwrap();
                let write = OwnedFd::from(write.into_nonblocking_fd().unwrap());
                let position = {
                    let mut state = state.lock().unwrap();
                    let position = state.inhibitors.len();
                    state.inhibitors.push(None);
                    position
                };
                connection.reply(&message.header(), &write).await.unwrap();
                drop(write);
                assert_eq!(read.read(&mut [0; 1]).await.unwrap(), 0);
                let mut state = state.lock().unwrap();
                let Some(closed) = state.inhibitors.get_mut(position) else {
                    panic!("the fake logind lost a granted inhibitor");
                };
                *closed = Some(Instant::now());
            }));
        }
    }
}

impl Impostor {
    pub(crate) fn connect(bus: &Bus) -> Self {
        Self {
            sender: Sender::connect(bus),
        }
    }

    pub(crate) fn prepare_for_sleep(&self, going_down: bool) {
        self.sender.prepare_for_sleep(going_down);
    }
}
