//! A driver on its own thread, so a test calls `submit` from any runtime and blocks on the pods.

mod driver;
mod failures;

use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use pods_core::state::State;
use pods_proto::addr::BdAddr;
use pods_proto::noise::NoiseMode;
use pods_sys::l2cap::ChannelError;
use rustix::io::Errno;
use tokio::runtime::Builder;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

use super::{Connector, DeviceEvent, Input, REQUEST_QUEUE, drive, queue};
use crate::daemon::hub::Hub;

/// A wait in a test still running after this long has hung, and the test fails.
pub(in crate::daemon) const HANG_LIMIT: Duration = Duration::from_secs(10);

/// A current thread runtime with a clock, as the daemon's own.
pub(in crate::daemon) fn block_on<F: Future>(future: F) -> F::Output {
    Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(future)
}

/// The bound past an exact "not before" that a real time check allows a loaded host.
const REAL_TIME_SLACK: Duration = Duration::from_millis(500);

fn device() -> BdAddr {
    "02:00:00:00:00:01".parse().unwrap()
}

#[expect(
    clippy::print_stderr,
    reason = "a timing test prints the range it saw, pass or fail"
)]
fn report(line: &str) {
    eprintln!("{line}");
}

fn device_up(inputs: &mpsc::Sender<Input>) {
    inputs
        .blocking_send(Input::Device(DeviceEvent::Up(device())))
        .unwrap();
}

fn wait_for_state(
    hub: &Hub,
    within: Duration,
    what: &str,
    ready: impl Fn(&State) -> bool,
) -> State {
    block_on(wait_for_state_async(hub, within, what, ready))
}

async fn wait_for_state_async(
    hub: &Hub,
    within: Duration,
    what: &str,
    ready: impl Fn(&State) -> bool,
) -> State {
    let mut updates = hub.subscribe();
    let found = timeout(within, async {
        loop {
            let state = updates.borrow_and_update().clone();
            if ready(&state) {
                return state;
            }
            updates.changed().await.unwrap();
        }
    });
    found.await.unwrap_or_else(|_| {
        panic!(
            "{what} not seen within {within:?}; the state is {:?}",
            hub.state()
        )
    })
}

fn wire(mode: NoiseMode) -> u8 {
    let [wire, ..] = mode.command().value;
    wire
}

/// Sees each input a reader sends, before the driver does.
pub(super) type Tap = Box<dyn Fn(&Input) + Send>;

/// Dropping it aborts the driver and joins its thread.
#[derive(Debug)]
pub(in crate::daemon) struct Running {
    hub: Hub,
    inputs: mpsc::Sender<Input>,
    halt: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Running {
    /// Its connector refuses every connect, for tests that never bring a link up.
    pub(in crate::daemon) fn new() -> Self {
        let refuse: Connector = Arc::new(|_| Err(ChannelError::Connect(Errno::HOSTDOWN)));
        Self::start(refuse, None)
    }

    pub(super) fn start(connector: Connector, tap: Option<Tap>) -> Self {
        let (link, inputs) = queue();
        let hub = Hub::new(link.clone());
        let fed = link.clone();
        let (halt, asked) = oneshot::channel::<()>();
        let shared = hub.clone();
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        let body = move || {
            runtime.block_on(async move {
                let (requests, _unread) = mpsc::channel(REQUEST_QUEUE);
                let feedback = match tap {
                    None => link,
                    Some(tap) => forward_through(tap, link),
                };
                let driver = tokio::spawn(drive(shared, inputs, feedback, requests, connector));
                // An error is a dropped `Running`, which stops the driver all the same.
                asked.await.ok();
                driver.abort();
                // A driver that panicked fails the test through this thread's join.
                if let Err(error) = driver.await
                    && error.is_panic()
                {
                    std::panic::resume_unwind(error.into_panic());
                }
            });
        };
        let thread = thread::Builder::new()
            .name("link-driver".to_owned())
            .spawn(body)
            .unwrap();
        Self {
            inputs: fed,
            hub,
            halt: Some(halt),
            thread: Some(thread),
        }
    }

    pub(in crate::daemon) fn hub(&self) -> Hub {
        self.hub.clone()
    }

    pub(super) fn inputs(&self) -> &mpsc::Sender<Input> {
        &self.inputs
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(halt) = self.halt.take() {
            // A driver thread that already ended has dropped its receiver, which is the end wanted.
            halt.send(()).ok();
        }
        if let Some(thread) = self.thread.take() {
            let joined = thread.join();
            if !thread::panicking() {
                joined.unwrap();
            }
        }
    }
}

fn forward_through(tap: Tap, link: mpsc::Sender<Input>) -> mpsc::Sender<Input> {
    let (tapped, mut taken) = mpsc::channel(super::INPUT_QUEUE);
    tokio::spawn(async move {
        while let Some(input) = taken.recv().await {
            tap(&input);
            if link.send(input).await.is_err() {
                return;
            }
        }
    });
    tapped
}
