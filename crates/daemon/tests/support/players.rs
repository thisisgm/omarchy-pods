//! Fake MPRIS media players on a private session bus, each serving `/org/mpris/MediaPlayer2` from one thread of its own.

use std::collections::HashMap;
use std::future::poll_fn;
use std::mem;
use std::panic;
use std::pin::Pin;
use std::sync::mpsc::{self as reply_channel, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::task::Poll;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tokio::runtime::Builder as RuntimeBuilder;
use tokio::sync::mpsc;
use zbus::connection::Builder;
use zbus::export::futures_core::Stream;
use zbus::message::Type;
use zbus::names::{InterfaceName, MemberName};
use zbus::{Connection, Message, MessageStream, ObjectPath, Variant};

use super::HANG_LIMIT;
use super::bus::{Bus, in_time};

pub(crate) const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";
pub(crate) const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
pub(crate) const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
pub(crate) const STATUS_PROPERTY: &str = "PlaybackStatus";
/// The error the bus gives for a method nobody serves, which a real player gives for one it lacks.
pub(crate) const UNKNOWN_METHOD: &str = "org.freedesktop.DBus.Error.UnknownMethod";
const NAME_PREFIX: &str = "org.mpris.MediaPlayer2.";
const CHANGED_SIGNAL: &str = "PropertiesChanged";
const THREAD_NAME: &str = "fake-player";
/// Orders from the test waiting for the serving thread; the test waits for each to be carried out.
const ORDERS_QUEUE: usize = 1;
/// Calls held unanswered at once; a test holds a handful.
const HELD_MAX: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Playback {
    Playing,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Asked {
    Pause,
    Play,
}

/// Owns `org.mpris.MediaPlayer2.<name>` from `start` until `leave` or the drop.
pub(crate) struct FakePlayer {
    bus_name: String,
    shared: Arc<Shared>,
    server: Option<Server>,
}

/// The sender of orders, whose drop ends the serving thread, and that thread.
type Server = (mpsc::Sender<(Order, Done)>, JoinHandle<()>);

/// Told once an order is carried out, its signal emitted.
type Done = reply_channel::Sender<()>;

#[derive(Clone, Copy)]
enum Order {
    Set(Playback),
    Hold(bool),
}

/// The state the serving thread changes and the test reads, and the signal of each recorded call.
struct Shared {
    state: Mutex<State>,
    recorded: Condvar,
}

struct State {
    playback: Playback,
    asked: Vec<Asked>,
    /// One entry per call an earlier `wait_asked` returned on.
    waited: Vec<Asked>,
    refused: Vec<String>,
    hold: bool,
    held: Vec<(Message, Request)>,
}

/// A call as the fake reads it.
enum Request {
    Asked(Asked),
    Status,
    Refused(String),
}

/// What one call or order gives back: the reply to send, then the status to announce.
struct Outcome {
    reply: Option<Message>,
    changed: Option<Playback>,
}

impl Playback {
    fn as_str(self) -> &'static str {
        match self {
            Self::Playing => "Playing",
            Self::Paused => "Paused",
            Self::Stopped => "Stopped",
        }
    }

    /// The status after `asked`: a pause leaves a stopped player stopped, a play starts any player.
    fn after(self, asked: Asked) -> Self {
        match (asked, self) {
            (Asked::Pause, Self::Playing) => Self::Paused,
            (Asked::Pause, unchanged) => unchanged,
            (Asked::Play, _) => Self::Playing,
        }
    }
}

impl FakePlayer {
    pub(crate) fn start(bus: &Bus, name: &str, playback: Playback) -> Self {
        let bus_name = format!("{NAME_PREFIX}{name}");
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                playback,
                asked: Vec::new(),
                waited: Vec::new(),
                refused: Vec::new(),
                hold: false,
                held: Vec::new(),
            }),
            recorded: Condvar::new(),
        });
        let (orders, received) = mpsc::channel(ORDERS_QUEUE);
        let (ready, started) = reply_channel::channel();
        let address = bus.address.clone();
        let thread = {
            let bus_name = bus_name.clone();
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name(THREAD_NAME.to_owned())
                .spawn(move || serve(&address, &bus_name, &shared, received, &ready))
                .unwrap()
        };
        let player = Self {
            bus_name,
            shared,
            server: Some((orders, thread)),
        };
        // The thread bounds its own connect, so it either owns the name or ends.
        started
            .recv()
            .unwrap_or_else(|_| panic!("the fake player {} never owned its name", player.bus_name));
        player
    }

    pub(crate) fn bus_name(&self) -> &str {
        &self.bus_name
    }

    pub(crate) fn playback(&self) -> Playback {
        self.shared.lock().playback
    }

    /// Changes the status as the user would, announcing it when it differs.
    pub(crate) fn set(&self, playback: Playback) {
        self.order(Order::Set(playback));
    }

    /// Every `Pause` and `Play` the player received, oldest first, held ones included.
    pub(crate) fn asked(&self) -> Vec<Asked> {
        self.shared.lock().asked.clone()
    }

    /// Every call the player refused, as `interface.member`, oldest first.
    pub(crate) fn refused(&self) -> Vec<String> {
        self.shared.lock().refused.clone()
    }

    /// Waits for a `call` no earlier wait returned on, failing with the record at `within`.
    pub(crate) fn wait_asked(&self, call: Asked, within: Duration) {
        let (mut state, _) = self
            .shared
            .recorded
            .wait_timeout_while(self.shared.lock(), within, |state| !state.unwaited(call))
            .unwrap_or_else(PoisonError::into_inner);
        assert!(
            state.unwaited(call),
            "{call:?} not asked of {} within {within:?}: {:?}",
            self.bus_name,
            state.asked
        );
        state.waited.push(call);
    }

    /// Fails with the record if any call but a status read arrives within `quiet`.
    pub(crate) fn none_for(&self, quiet: Duration) {
        let before = self.shared.lock().calls();
        let (state, waited) = self
            .shared
            .recorded
            .wait_timeout_while(self.shared.lock(), quiet, |state| state.calls() == before)
            .unwrap_or_else(PoisonError::into_inner);
        assert!(
            waited.timed_out(),
            "{} called within {quiet:?}: asked {:?}, refused {:?}",
            self.bus_name,
            state.asked,
            state.refused
        );
    }

    /// While set, calls are recorded and left unanswered; clearing it answers them in order.
    pub(crate) fn hold_replies(&self, hold: bool) {
        self.order(Order::Hold(hold));
    }

    /// Releases the name and ends the serving thread, failing the test if that thread panicked.
    pub(crate) fn leave(self) {
        drop(self);
    }

    fn order(&self, order: Order) {
        let (orders, _) = self.server.as_ref().expect("a serving fake player");
        let (done, carried_out) = reply_channel::channel();
        orders
            .blocking_send((order, done))
            .expect("the fake player stopped serving");
        match carried_out.recv_timeout(HANG_LIMIT) {
            Ok(()) => {}
            Err(RecvTimeoutError::Timeout) => panic!("the fake player hung on an order"),
            Err(RecvTimeoutError::Disconnected) => panic!("the fake player stopped serving"),
        }
    }
}

impl Drop for FakePlayer {
    fn drop(&mut self) {
        let Some((orders, thread)) = self.server.take() else {
            return;
        };
        drop(orders);
        if let Err(panic) = thread.join()
            && !thread::panicking()
        {
            panic::resume_unwind(panic);
        }
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records `call` and answers it, or keeps it while replies are held.
    fn receive(&self, call: Message) -> Outcome {
        let request = Request::read(&call);
        let mut state = self.lock();
        match &request {
            Request::Asked(asked) => state.asked.push(*asked),
            Request::Refused(member) => state.refused.push(member.clone()),
            Request::Status => {}
        }
        self.recorded.notify_all();
        if state.hold {
            assert!(
                state.held.len() < HELD_MAX,
                "more than {HELD_MAX} calls held"
            );
            state.held.push((call, request));
            return Outcome::nothing();
        }
        state.answer(&call, &request)
    }

    fn obey(&self, order: Order) -> Vec<Outcome> {
        let mut state = self.lock();
        match order {
            Order::Set(playback) => vec![Outcome {
                reply: None,
                changed: state.change(playback),
            }],
            Order::Hold(true) => {
                state.hold = true;
                Vec::new()
            }
            Order::Hold(false) => {
                state.hold = false;
                let held = mem::take(&mut state.held);
                held.iter()
                    .map(|(call, request)| state.answer(call, request))
                    .collect()
            }
        }
    }
}

impl State {
    /// Whether more `call`s were asked than earlier waits returned on.
    fn unwaited(&self, call: Asked) -> bool {
        let count = |calls: &[Asked]| calls.iter().filter(|asked| **asked == call).count();
        count(&self.asked) > count(&self.waited)
    }

    fn calls(&self) -> usize {
        self.asked.len().saturating_add(self.refused.len())
    }

    fn change(&mut self, playback: Playback) -> Option<Playback> {
        (self.playback != playback).then(|| {
            self.playback = playback;
            playback
        })
    }

    fn answer(&mut self, call: &Message, request: &Request) -> Outcome {
        let header = call.header();
        let (reply, changed) = match request {
            Request::Asked(asked) => (
                Message::method_return(&header).build(&()),
                self.change(self.playback.after(*asked)),
            ),
            Request::Status => (
                Message::method_return(&header).build(&Variant::from(self.playback.as_str())),
                None,
            ),
            Request::Refused(_) => (
                Message::error(&header, UNKNOWN_METHOD).build(&("Unknown method",)),
                None,
            ),
        };
        Outcome {
            reply: Some(reply.unwrap()),
            changed,
        }
    }
}

impl Request {
    fn read(call: &Message) -> Self {
        let header = call.header();
        let interface = header.interface().map_or("", InterfaceName::as_str);
        let member = header.member().map_or("", MemberName::as_str);
        let path = header.path().map_or("", ObjectPath::as_str);
        let served = match (path, interface, member) {
            (PLAYER_PATH, PLAYER_INTERFACE, "Pause") => Some(Self::Asked(Asked::Pause)),
            (PLAYER_PATH, PLAYER_INTERFACE, "Play") => Some(Self::Asked(Asked::Play)),
            (PLAYER_PATH, PROPERTIES, "Get") if asks_status(call) => Some(Self::Status),
            _ => None,
        };
        served.unwrap_or_else(|| Self::Refused(format!("{interface}.{member}")))
    }
}

/// Whether the body of a `Properties.Get` is `("org.mpris.MediaPlayer2.Player", "PlaybackStatus")`.
fn asks_status(call: &Message) -> bool {
    call.body()
        .deserialize::<(&str, &str)>()
        .is_ok_and(|asked| asked == (PLAYER_INTERFACE, STATUS_PROPERTY))
}

impl Outcome {
    fn nothing() -> Self {
        Self {
            reply: None,
            changed: None,
        }
    }

    async fn deliver(self, connection: &Connection) {
        if let Some(reply) = self.reply {
            connection.send(&reply).await.unwrap();
        }
        if let Some(playback) = self.changed {
            let changed = HashMap::from([(STATUS_PROPERTY, Variant::from(playback.as_str()))]);
            let body = (PLAYER_INTERFACE, changed, Vec::<&str>::new());
            connection
                .emit_signal(None::<&str>, PLAYER_PATH, PROPERTIES, CHANGED_SIGNAL, &body)
                .await
                .unwrap();
        }
    }
}

/// What the serving thread wakes for.
enum Event {
    Order(Option<(Order, Done)>),
    Call(Option<zbus::Result<Message>>),
}

/// The thread's body: owns the name, answers calls and orders until the orders end, then releases the name.
fn serve(
    address: &str,
    bus_name: &str,
    shared: &Shared,
    mut orders: mpsc::Receiver<(Order, Done)>,
    ready: &reply_channel::Sender<()>,
) {
    let runtime = RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (connection, mut calls) = in_time(&runtime, async {
        let connection = Builder::address(address).build().await.unwrap();
        // The stream exists before the name, so no call to the name is missed.
        let calls = MessageStream::from(&connection);
        connection.request_name(bus_name).await.unwrap();
        (connection, calls)
    });
    ready.send(()).unwrap();
    runtime.block_on(async {
        loop {
            let next = poll_fn(|context| {
                if let Poll::Ready(order) = orders.poll_recv(context) {
                    return Poll::Ready(Event::Order(order));
                }
                Pin::new(&mut calls).poll_next(context).map(Event::Call)
            });
            match next.await {
                Event::Order(None) => return,
                Event::Order(Some((order, done))) => {
                    for outcome in shared.obey(order) {
                        outcome.deliver(&connection).await;
                    }
                    // A test that already failed dropped its receiver, which changes nothing here.
                    done.send(()).ok();
                }
                Event::Call(Some(Ok(call))) if call.message_type() == Type::MethodCall => {
                    shared.receive(call).deliver(&connection).await;
                }
                Event::Call(Some(Ok(_))) => {}
                Event::Call(Some(Err(error))) => {
                    panic!("the fake player {bus_name} cannot read its bus: {error}")
                }
                Event::Call(None) => {
                    panic!("the fake player {bus_name}'s bus closed before the end of the test")
                }
            }
        }
    });
    let released = in_time(&runtime, connection.release_name(bus_name));
    assert!(released.unwrap(), "the fake player did not own {bus_name}");
}
