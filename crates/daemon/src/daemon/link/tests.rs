//! A driver on its own thread, so a test calls `submit` from any runtime and blocks on the pods.

mod driver;
mod failures;
mod ladders;
mod settle;

use std::sync::{Arc, mpsc as std_mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use pods_core::policy::MicRequest;
use pods_core::prefs::Prefs;
use pods_core::state::State;
use pods_proto::addr::BdAddr;
use pods_proto::noise::NoiseMode;
use pods_sys::l2cap::ChannelError;
use rustix::io::Errno;
use tokio::runtime::{Builder, Handle};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::timeout;

use super::{Connector, DeviceEvent, Input, Outlets, REQUEST_QUEUE, Request, drive, queue};
use crate::daemon::hub::Hub;
use crate::daemon::{audio, keys, media, notify};

/// A wait in a test still running after this long has hung, and the test fails.
pub(in crate::daemon) const HANG_LIMIT: Duration = Duration::from_secs(10);

/// Outlets whose queues nobody reads, for a test that looks only at the link.
pub(in crate::daemon) fn unread_outlets() -> Outlets {
    let (notices, _unread) = notify::queue();
    let (prefs, _unwritten) = watch::channel(Prefs::default());
    let (audio, _unread) = audio::queue();
    let (keys, _unstored) = keys::queue();
    let (media, _unread) = media::queue();
    let (monitor, _unwatched) = watch::channel(false);
    let (mic, _unread) = watch::channel(MicRequest::Close);
    Outlets {
        notices,
        prefs,
        audio,
        keys,
        media,
        monitor,
        mic,
        tap: crate::daemon::mic::Tap::new(),
    }
}

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

/// Sees each input a connect thread sends, before the driver does.
pub(super) type Tap = Box<dyn Fn(&Input) + Send>;

/// Dropping it aborts the driver and joins its thread.
#[derive(Debug)]
pub(in crate::daemon) struct Running {
    hub: Hub,
    inputs: mpsc::Sender<Input>,
    halt: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    runtime: Handle,
    requests: Option<mpsc::Receiver<Request>>,
}

/// While alive, the paused clock of a `Running::paused` stands still; dropping it lets the clock move again.
pub(super) struct ClockHold {
    _release: std_mpsc::Sender<()>,
}

impl Running {
    /// Its connector refuses every connect, for tests that never bring a link up.
    pub(in crate::daemon) fn new() -> Self {
        let refuse: Connector = Arc::new(|_| Err(ChannelError::Connect(Errno::HOSTDOWN)));
        Self::start(refuse, None)
    }

    pub(super) fn start(connector: Connector, tap: Option<Tap>) -> Self {
        Self::launch(connector, tap, Builder::new_current_thread().enable_all())
    }

    /// On a paused clock, which jumps to the next deadline whenever every task waits, so a rung takes exactly its delay.
    pub(super) fn paused(connector: Connector, tap: Tap) -> Self {
        let mut builder = Builder::new_current_thread();
        Self::launch(
            connector,
            Some(tap),
            builder.enable_all().start_paused(true),
        )
    }

    /// Runs `script` on the driver's runtime, so its waits and stamps read the driver's clock.
    pub(super) fn script<F>(&self, script: F) -> F::Output
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let task = self.runtime.spawn(script);
        block_on(async { timeout(HANG_LIMIT, task).await })
            .unwrap_or_else(|_| panic!("the script did not end within {HANG_LIMIT:?}"))
            .unwrap()
    }

    /// The requests the driver hands BlueZ; only the first call has them.
    pub(super) fn requests(&mut self) -> mpsc::Receiver<Request> {
        self.requests.take().unwrap()
    }

    /// The runtime holds a paused clock still while a blocking task runs, so real work on other threads cannot pass for time.
    pub(super) fn hold_clock(&self) -> ClockHold {
        let (release, held) = std_mpsc::channel::<()>();
        drop(self.runtime.spawn_blocking(move || held.recv()));
        ClockHold { _release: release }
    }

    fn launch(connector: Connector, tap: Option<Tap>, builder: &mut Builder) -> Self {
        let (link, inputs) = queue();
        let hub = Hub::new(link.clone(), State::default());
        let fed = link.clone();
        let (halt, asked) = oneshot::channel::<()>();
        let shared = hub.clone();
        let runtime = builder.build().unwrap();
        let handle = runtime.handle().clone();
        let (requests, unread) = mpsc::channel(REQUEST_QUEUE);
        let body = move || {
            runtime.block_on(async move {
                let feedback = match tap {
                    None => link,
                    Some(tap) => forward_through(tap, link),
                };
                let driver = tokio::spawn(drive(
                    shared,
                    inputs,
                    feedback,
                    requests,
                    connector,
                    unread_outlets(),
                    None,
                ));
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
            runtime: handle,
            requests: Some(unread),
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
