//! A fake logind on the private system bus, and an impostor that sends its signal under another name.

use std::cell::RefCell;
use std::future::poll_fn;
use std::pin::Pin;

use tokio::runtime::{Builder as RuntimeBuilder, Runtime};
use zbus::connection::Builder;
use zbus::export::futures_core::Stream;
use zbus::message::{Flags, Type};
use zbus::{Connection, Message, MessageStream};

use super::bus::{Bus, in_time};

pub(crate) const LOGIND_NAME: &str = "org.freedesktop.login1";
pub(crate) const MANAGER_PATH: &str = "/org/freedesktop/login1";
pub(crate) const MANAGER_INTERFACE: &str = "org.freedesktop.login1.Manager";
pub(crate) const SLEEP_SIGNAL: &str = "PrepareForSleep";

/// The call the fake sends itself to mark the end of what reached it before.
const MARKER: &str = "FixtureMarker";

/// One connection to the bus and the runtime that drives it; dropping it ends every task the connection started.
struct Sender {
    connection: Connection,
    runtime: Runtime,
}

/// Owns `org.freedesktop.login1` on the private system bus from `start` on, and answers no method call.
pub(crate) struct FakeLogind {
    /// Every message that reached the fake since `start`, read only by `method_calls`.
    inbox: RefCell<MessageStream>,
    sender: Sender,
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
            runtime,
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
        let inbox = RefCell::new(MessageStream::from(&sender.connection));
        in_time(&sender.runtime, sender.connection.request_name(LOGIND_NAME)).unwrap();
        Self { inbox, sender }
    }

    /// The member of every method call that reached the fake, read up to a call it sends itself, which the bus delivers after them.
    pub(crate) fn method_calls(&self) -> Vec<String> {
        let connection = &self.sender.connection;
        let own = connection.unique_name().unwrap().to_string();
        let marker = Message::method_call(MANAGER_PATH, MARKER)
            .destination(own.as_str())
            .with_flags(Flags::NO_REPLY_EXPECTED)
            .build(&())
            .unwrap();
        let mut inbox = self.inbox.borrow_mut();
        in_time(&self.sender.runtime, async {
            connection.send(&marker).await.unwrap();
            let mut calls = Vec::new();
            loop {
                let next = poll_fn(|context| Pin::new(&mut *inbox).poll_next(context)).await;
                let message = next.expect("the fake logind's inbox ended").unwrap();
                let header = message.header();
                if header.message_type() != Type::MethodCall {
                    continue;
                }
                let member = header
                    .member()
                    .map_or_else(String::new, ToString::to_string);
                let from_itself = header.sender().is_some_and(|sender| sender.as_str() == own);
                if from_itself && member == MARKER {
                    return calls;
                }
                calls.push(member);
            }
        })
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
