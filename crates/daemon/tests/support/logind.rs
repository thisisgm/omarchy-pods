//! A fake logind on the private system bus, and an impostor that sends its signal under another name.

use tokio::runtime::{Builder as RuntimeBuilder, Runtime};
use zbus::Connection;
use zbus::connection::Builder;

use super::bus::{Bus, in_time};

pub(crate) const LOGIND_NAME: &str = "org.freedesktop.login1";
pub(crate) const MANAGER_PATH: &str = "/org/freedesktop/login1";
pub(crate) const MANAGER_INTERFACE: &str = "org.freedesktop.login1.Manager";
pub(crate) const SLEEP_SIGNAL: &str = "PrepareForSleep";

/// One connection to the bus and the runtime that drives it; dropping it ends every task the connection started.
struct Sender {
    connection: Connection,
    runtime: Runtime,
}

/// Owns `org.freedesktop.login1` on the private system bus from `start` on.
pub(crate) struct FakeLogind {
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
        in_time(&sender.runtime, sender.connection.request_name(LOGIND_NAME)).unwrap();
        Self { sender }
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
