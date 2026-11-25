//! Hands logind's sleep edges to the driver and shares its bus for session-scoped delay locks.

use std::future::poll_fn;
use std::ops::ControlFlow;
use std::pin::{Pin, pin};
use std::task::Poll;
use std::time::Duration;

use pods_core::policy::SleepEdge;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::sleep;
use zbus::export::futures_core::Stream;
use zbus::{Connection, Message, MessageStream, OwnedFd};

use super::bluez::{ErrorLog, SYSTEM_BUS_RETRY_MS};
use super::link::Input;
use super::log;

/// The bus delivers only logind's own signal, so another sender's `PrepareForSleep` never arrives.
const SLEEP_RULE: &str = "type='signal',sender='org.freedesktop.login1',path='/org/freedesktop/login1',interface='org.freedesktop.login1.Manager',member='PrepareForSleep'";

const LOGIND: &str = "org.freedesktop.login1";
const MANAGER_PATH: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";

const INHIBIT_FAILED: &str = "dictation: cannot inhibit sleep via logind";
const WATCHING: &str = "sleep: watching logind for a suspend";
const SLEEPING: &str = "sleep: the system is going to sleep";
const WOKE: &str = "sleep: the system woke";

pub(super) type Bus = watch::Receiver<Option<Connection>>;
/// Keeps the granted fd alive until the driver drops the receiver.
pub(super) type Inhibitor = oneshot::Receiver<OwnedFd>;

/// Ask at mode set and cancel the pending call when the receiver is dropped.
pub(super) fn inhibit(bus: &Bus) -> Option<Inhibitor> {
    let Some(connection) = bus.borrow().clone() else {
        log(&format!("{INHIBIT_FAILED}: no system bus"));
        return None;
    };
    let (mut acquired, inhibitor) = oneshot::channel();
    drop(tokio::spawn(async move {
        let mut call = pin!(request_inhibitor(&connection));
        let reply = poll_fn(|context| {
            if acquired.poll_closed(context).is_ready() {
                return Poll::Ready(None);
            }
            call.as_mut().poll(context).map(Some)
        })
        .await;
        match reply {
            Some(Ok(fd)) => drop(acquired.send(fd)),
            Some(Err(error)) => log(&format!("{INHIBIT_FAILED}: {error}")),
            None => {}
        }
    }));
    Some(inhibitor)
}

async fn request_inhibitor(connection: &Connection) -> zbus::Result<OwnedFd> {
    let reply = connection
        .call_method(
            Some(LOGIND),
            MANAGER_PATH,
            Some(MANAGER),
            "Inhibit",
            &("sleep", "librepods", "Stop dictation before sleep", "delay"),
        )
        .await?;
    // Sample reply: UNIX_FD 0, the first attached file descriptor.
    reply.body().deserialize()
}

/// Retries the system bus while it is out of reach; with no logind on it, nothing ever arrives.
pub(super) async fn watch(inputs: mpsc::Sender<Input>, bus: watch::Sender<Option<Connection>>) {
    let mut errors = ErrorLog::default();
    loop {
        // The BlueZ watcher already logs a system bus out of reach, with the same reason.
        if let Ok(connection) = Connection::system().await {
            match MessageStream::for_match_rule(SLEEP_RULE, &connection, None).await {
                Ok(signals) => {
                    errors.clear();
                    drop(bus.send_replace(Some(connection)));
                    log(WATCHING);
                    let flow = forward(signals, &inputs).await;
                    drop(bus.send_replace(None));
                    if flow.is_break() {
                        return;
                    }
                }
                Err(error) => errors.note(format!(
                    "sleep: cannot subscribe to logind: {error}; trying again every {SYSTEM_BUS_RETRY_MS} ms"
                )),
            }
        }
        sleep(Duration::from_millis(SYSTEM_BUS_RETRY_MS)).await;
    }
}

/// `Break` once the driver is gone, `Continue` once the connection ended and a wake is handed over.
async fn forward(
    mut signals: impl Stream<Item = zbus::Result<Message>> + Unpin,
    inputs: &mpsc::Sender<Input>,
) -> ControlFlow<()> {
    loop {
        let message = match poll_fn(|context| Pin::new(&mut signals).poll_next(context)).await {
            Some(Ok(message)) => message,
            // A read error ends the connection, and its stream ends after it.
            Some(Err(error)) => {
                return lost(&format!("lost logind's signals: {error}"), inputs).await;
            }
            None => return lost("logind's signals ended", inputs).await,
        };
        let edge = match edge(&message) {
            Ok(edge) => edge,
            Err(error) => {
                log(&format!(
                    "sleep: PrepareForSleep without a boolean: {error}"
                ));
                continue;
            }
        };
        if inputs.send(Input::Sleep(edge)).await.is_err() {
            return ControlFlow::Break(());
        }
        log(match edge {
            SleepEdge::Sleeping => SLEEPING,
            SleepEdge::Woke => WOKE,
        });
    }
}

/// A wake missed while away cannot be asked for, so the driver always gets one: an open or kept window closes after the settle, else a settle long quiet one opens.
async fn lost(why: &str, inputs: &mpsc::Sender<Input>) -> ControlFlow<()> {
    log(&format!(
        "sleep: {why}; handing the driver a wake and subscribing again in {SYSTEM_BUS_RETRY_MS} ms"
    ));
    if inputs.send(Input::Sleep(SleepEdge::Woke)).await.is_err() {
        return ControlFlow::Break(());
    }
    ControlFlow::Continue(())
}

/// The signal's one argument; sample input `true`, the edge into sleep.
fn edge(message: &Message) -> zbus::Result<SleepEdge> {
    let going_down: bool = message.body().deserialize()?;
    Ok(if going_down {
        SleepEdge::Sleeping
    } else {
        SleepEdge::Woke
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::task::{Context, Poll};

    use super::*;

    /// Room for every edge one test hands over.
    const EDGES_MAX: usize = 8;

    /// The given items in order, then the end of the stream.
    struct Script(VecDeque<zbus::Result<Message>>);

    impl Stream for Script {
        type Item = zbus::Result<Message>;

        fn poll_next(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Option<Self::Item>> {
            Poll::Ready(self.0.pop_front())
        }
    }

    /// What `forward` returns on `items`, and the edges it handed the driver.
    fn forwarded(items: Vec<zbus::Result<Message>>) -> (ControlFlow<()>, Vec<SleepEdge>) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let (inputs, mut driver) = mpsc::channel(EDGES_MAX);
        let flow = runtime.block_on(forward(Script(items.into()), &inputs));
        let mut edges = Vec::new();
        while let Ok(Input::Sleep(edge)) = driver.try_recv() {
            edges.push(edge);
        }
        (flow, edges)
    }

    #[test]
    fn a_stream_that_ends_hands_the_driver_a_wake() {
        let (flow, edges) = forwarded(vec![Ok(signal(&true))]);
        assert_eq!(flow, ControlFlow::Continue(()));
        assert_eq!(edges, [SleepEdge::Sleeping, SleepEdge::Woke]);
    }

    #[test]
    fn a_read_error_hands_the_driver_a_wake() {
        let lost = zbus::Error::Failure("the socket closed".to_owned());
        let (flow, edges) = forwarded(vec![Ok(signal(&true)), Err(lost)]);
        assert_eq!(flow, ControlFlow::Continue(()));
        assert_eq!(edges, [SleepEdge::Sleeping, SleepEdge::Woke]);
    }

    #[test]
    fn a_gone_driver_ends_the_watch_without_a_wake() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let (inputs, driver) = mpsc::channel(EDGES_MAX);
        drop(driver);
        let flow = runtime.block_on(forward(Script(VecDeque::new()), &inputs));
        assert_eq!(flow, ControlFlow::Break(()));
    }

    fn signal(
        body: &(impl zbus::export::serde::Serialize + zbus::zvariant::DynamicType),
    ) -> Message {
        Message::signal(
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            "PrepareForSleep",
        )
        .build(body)
        .unwrap()
    }

    #[test]
    fn true_is_the_edge_into_sleep_and_false_the_wake() {
        assert_eq!(edge(&signal(&true)).unwrap(), SleepEdge::Sleeping);
        assert_eq!(edge(&signal(&false)).unwrap(), SleepEdge::Woke);
    }

    #[test]
    fn a_body_that_is_not_one_boolean_is_refused() {
        assert!(edge(&signal(&"true")).is_err());
        assert!(edge(&signal(&())).is_err());
        assert!(edge(&signal(&1_u32)).is_err());
    }
}
