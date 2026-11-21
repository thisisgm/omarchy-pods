//! Runs the link machine: device events, the channel's SDUs, commands and the clock go in, the state is published and the actions carried out.

mod reader;

use std::collections::{HashMap, VecDeque};
use std::ops::ControlFlow;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use pods_core::command::{Command, Outcome};
use pods_core::link::{Action, DeviceAction, End, Event, Machine, Phase, Ticket};
use pods_core::state::State;
use pods_proto::addr::BdAddr;
use pods_proto::model::Model;
use pods_sys::l2cap::{Channel, ChannelError};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{self, JoinHandle};
use tokio::time::{Instant, timeout, timeout_at};

use super::hub::Hub;
use super::{MAX_CONNECTIONS, log};

/// The connect timeout of aacp.rs in librepods-org/librepods pull request 655; a reader thread waits in a connect this long at most.
pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Room for one command from each client, shared with the reader's SDUs; a reader waits for room, a command finding none is refused.
pub(super) const INPUT_QUEUE: usize = MAX_CONNECTIONS;

/// Device requests waiting for the watcher; one sweep reads every device, so a few waiting cover any burst of events.
pub(super) const REQUEST_QUEUE: usize = 8;

/// A stop step is a few calls that never block, so a driver still busy after this is stuck and is aborted.
const STOP_WAIT_MS: u64 = 500;

const READER_THREAD: &str = "link-reader";

#[derive(Debug, Clone, Copy)]
#[expect(dead_code, reason = "the device watcher, still to come, sends these")]
pub(super) enum DeviceEvent {
    Up(BdAddr),
    Down(BdAddr),
    Gone(BdAddr),
    Lost,
    CallFailed(DeviceAction),
}

#[derive(Debug)]
pub(super) enum Input {
    Device(DeviceEvent),
    Opened(Arc<Channel>),
    Sdu(Box<[u8]>),
    Ended(End),
    Command {
        command: Command,
        reply: oneshot::Sender<Outcome>,
    },
    Shutdown(oneshot::Sender<()>),
}

#[derive(Debug, Clone, Copy)]
#[expect(dead_code, reason = "the device watcher, still to come, reads these")]
pub(super) enum Request {
    Sweep,
    Call {
        device: BdAddr,
        action: DeviceAction,
    },
}

pub(super) type Connector = Arc<dyn Fn(BdAddr) -> Result<Channel, ChannelError> + Send + Sync>;

/// The driver's input queue, the one place its bound is set.
pub(super) fn queue() -> (mpsc::Sender<Input>, mpsc::Receiver<Input>) {
    mpsc::channel(INPUT_QUEUE)
}

/// Asks the driver to stop and waits `STOP_WAIT_MS` for it, then aborts it; a reader thread still inside its connect is left to the end of the process.
pub(super) async fn stop(link: &mpsc::Sender<Input>, driver: JoinHandle<()>) {
    let (done, stopped) = oneshot::channel();
    let asked = async {
        if link.send(Input::Shutdown(done)).await.is_ok() {
            // An error is a driver that ended without answering, which is stopped all the same.
            stopped.await.ok();
        }
    };
    if timeout(Duration::from_millis(STOP_WAIT_MS), asked)
        .await
        .is_err()
    {
        log(&format!(
            "link: the driver did not stop within {STOP_WAIT_MS} ms; aborting it"
        ));
    }
    driver.abort();
    // Awaited so the driver's channel and replies are dropped before the daemon goes on; the result is only the cancellation.
    drop(driver.await);
}

pub(super) async fn drive(
    hub: Hub,
    mut inputs: mpsc::Receiver<Input>,
    feedback: mpsc::Sender<Input>,
    requests: mpsc::Sender<Request>,
    connector: Connector,
) {
    let mut driver = Driver {
        hub,
        machine: Machine::new(),
        started: Instant::now(),
        feedback,
        requests,
        connector,
        channel: None,
        replies: HashMap::new(),
        next_ticket: 0,
        fed_back: VecDeque::new(),
        announced: false,
    };
    loop {
        let turn = match driver.fed_back.pop_front() {
            Some(input) => Turn::Input(input),
            None => driver.wait(&mut inputs).await,
        };
        let flow = match turn {
            Turn::Input(input) => driver.take(input),
            Turn::Tick => {
                driver.step(Event::Tick);
                ControlFlow::Continue(())
            }
            Turn::Closed => {
                driver.step(Event::Shutdown);
                ControlFlow::Break(())
            }
        };
        if flow.is_break() {
            return;
        }
        // One change, then every subscriber gets its turn.
        task::yield_now().await;
    }
}

/// What woke the driver.
enum Turn {
    Input(Input),
    /// The machine's deadline passed.
    Tick,
    /// Every sender is gone, which a driver holding `feedback` never sees.
    Closed,
}

struct Driver {
    hub: Hub,
    machine: Machine,
    started: Instant,
    feedback: mpsc::Sender<Input>,
    requests: mpsc::Sender<Request>,
    connector: Connector,
    /// The open channel, from its `Opened` until its close or its `Ended`.
    channel: Option<Arc<Channel>>,
    /// At most `PENDING_MAX` entries outlive the turn they came in, since the machine resolves every other ticket at once.
    replies: HashMap<Ticket, oneshot::Sender<Outcome>>,
    next_ticket: u64,
    /// Inputs the driver makes itself, at most one per action of a step, taken before the queue.
    fed_back: VecDeque<Input>,
    /// `link: state populated` was logged since the link last came up.
    announced: bool,
}

impl Driver {
    async fn wait(&self, inputs: &mut mpsc::Receiver<Input>) -> Turn {
        let deadline = self
            .machine
            .deadline()
            .and_then(|at| self.started.checked_add(at));
        let received = match deadline {
            Some(at) => match timeout_at(at, inputs.recv()).await {
                Ok(received) => received,
                Err(_) => return Turn::Tick,
            },
            None => inputs.recv().await,
        };
        received.map_or(Turn::Closed, Turn::Input)
    }

    fn take(&mut self, input: Input) -> ControlFlow<()> {
        match input {
            Input::Device(device) => self.step(device.event()),
            Input::Opened(channel) => {
                self.channel = Some(channel);
                self.step(Event::ChannelOpened);
            }
            Input::Sdu(sdu) => self.step(Event::Packet(&sdu)),
            Input::Ended(end) => {
                self.channel = None;
                self.step(Event::ChannelEnded(end));
            }
            Input::Command { command, reply } => {
                let ticket = Ticket(self.next_ticket);
                self.next_ticket = self.next_ticket.wrapping_add(1);
                self.replies.insert(ticket, reply);
                self.step(Event::Command { ticket, command });
            }
            Input::Shutdown(done) => {
                self.step(Event::Shutdown);
                // A stop that stopped waiting has nobody left to tell.
                done.send(()).ok();
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }

    fn step(&mut self, event: Event<'_>) {
        let before = self.machine.phase();
        let mut next = self.hub.state();
        let actions = self.machine.step(&mut next, event, self.started.elapsed());
        self.log_progress(before, &next);
        self.hub.publish(next);
        for action in actions {
            self.execute(action);
        }
    }

    fn execute(&mut self, action: Action) {
        match action {
            Action::OpenChannel(device) => self.open(device),
            Action::CloseChannel => self.close(),
            Action::Send(sdu) => self.send(&sdu),
            Action::Sweep => {
                // Dropped when the queue is full: nothing waits on a sweep's answer.
                self.requests.try_send(Request::Sweep).ok();
            }
            Action::Call { device, action } => {
                if self
                    .requests
                    .try_send(Request::Call { device, action })
                    .is_err()
                {
                    let failed = DeviceEvent::CallFailed(action);
                    self.fed_back.push_back(Input::Device(failed));
                }
            }
            Action::Resolve { ticket, outcome } => {
                if let Some(reply) = self.replies.remove(&ticket) {
                    // A client that left no longer waits for its answer.
                    reply.send(outcome).ok();
                }
            }
        }
    }

    fn open(&mut self, device: BdAddr) {
        let connector = Arc::clone(&self.connector);
        let feedback = self.feedback.clone();
        // Never joined, at stop or anywhere else: a thread inside connect cannot be woken, and the end of the process ends it.
        let spawned = thread::Builder::new()
            .name(READER_THREAD.to_owned())
            .spawn(move || reader::run(connector, device, feedback));
        if let Err(error) = spawned {
            log(&format!("link: cannot start the reader thread: {error}"));
            self.fed_back.push_back(Input::Ended(End::ConnectFailed));
        }
    }

    fn close(&mut self) {
        if let Some(channel) = self.channel.take() {
            // An error is a channel already down, which is the end wanted.
            channel.shutdown().ok();
        }
    }

    fn send(&mut self, sdu: &[u8]) {
        let Some(channel) = &self.channel else {
            return;
        };
        if let Err(error) = channel.try_send(sdu) {
            log(&format!("link: {error}; closing the channel"));
            self.close();
        }
    }

    fn log_progress(&mut self, before: Phase, state: &State) {
        let after = self.machine.phase();
        let (from, to) = (phase_name(before), phase_name(after));
        // The stop already has its own line, the signal's.
        if from != to && after != Phase::Stopped {
            log(&format!("link: {from} to {to}"));
            self.announced = false;
        }
        if matches!(after, Phase::Up { .. }) && !self.announced && populated(state) {
            log("link: state populated");
            self.announced = true;
        }
    }
}

impl DeviceEvent {
    fn event(self) -> Event<'static> {
        match self {
            Self::Up(device) => Event::DeviceUp(device),
            Self::Down(device) => Event::DeviceDown(device),
            Self::Gone(device) => Event::DeviceGone(device),
            Self::Lost => Event::BluezLost,
            Self::CallFailed(action) => Event::CallFailed(action),
        }
    }
}

/// A name, a model and one battery reading: what the bar shows first.
fn populated(state: &State) -> bool {
    let batteries = state.batteries;
    let any_battery = [
        batteries.left,
        batteries.right,
        batteries.case,
        batteries.headset,
    ]
    .iter()
    .any(Option::is_some);
    !state.device_name.is_empty() && state.model != Model::Unknown && any_battery
}

/// The phase as fixed text for the log, without the data it carries.
fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Idle => "idle",
        Phase::Opening { .. } => "opening",
        Phase::AwaitHandshakeAck { .. } => "handshake",
        Phase::AwaitFeaturesAck { .. } => "features",
        Phase::Up { .. } => "up",
        Phase::Closing { .. } => "closing",
        Phase::Backoff { .. } => "backoff",
        Phase::Parked { .. } => "parked",
        Phase::Grace { .. } => "grace",
        Phase::Stopped => "stopped",
    }
}

#[cfg(test)]
pub(super) mod tests;
