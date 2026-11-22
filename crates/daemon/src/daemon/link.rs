//! Runs the link machine: events, SDUs, commands and the clock in; state and actions out.

mod reader;

use std::collections::{HashMap, VecDeque};
use std::future;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use pods_core::command::{Command, Outcome};
use pods_core::link::{
    Action, Call, End, Event, Machine, Phase, STATE_POPULATED_LINE, Ticket, phase_change_line,
};
use pods_core::policy::notices::{self, Notice};
use pods_core::state::State;
use pods_proto::addr::BdAddr;
use pods_proto::model::Model;
use pods_sys::l2cap::{Channel, ChannelError};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{self, JoinHandle};
use tokio::time::{Instant, timeout, timeout_at};

use super::hub::Hub;
use super::{MAX_CONNECTIONS, log, notify};

/// From aacp.rs in librepods-org/librepods pull request 655; a connect waits this long at most.
pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// One command per client, shared with SDUs; when full a reader waits, a command is refused.
pub(super) const INPUT_QUEUE: usize = MAX_CONNECTIONS;

/// One sweep reads every device, so a few waiting cover any burst of events.
pub(super) const REQUEST_QUEUE: usize = 8;

/// Stop steps never block, so a driver still busy after this is stuck and is aborted.
pub(super) const STOP_WAIT_MS: u64 = 500;

const READER_THREAD: &str = "link-reader";

#[derive(Debug, Clone, Copy)]
pub(super) enum DeviceEvent {
    Up(BdAddr),
    Down(BdAddr),
    Gone(BdAddr),
    Lost,
    CallFailed(Call),
}

#[derive(Debug)]
pub(super) enum Input {
    Device(DeviceEvent),
    Opened(Arc<Channel>),
    Sdu(Box<[u8]>),
    Ended(End),
    /// Made by the driver itself when a write fails.
    SendFailed,
    Command {
        command: Command,
        reply: oneshot::Sender<Outcome>,
    },
    Shutdown(oneshot::Sender<()>),
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Request {
    Sweep,
    Call { device: BdAddr, call: Call },
}

/// Where the policy's notices go, one queue per adapter.
#[derive(Debug)]
pub(super) struct Outlets {
    pub(super) notices: mpsc::Sender<Notice>,
}

pub(super) type Connector = Arc<dyn Fn(BdAddr) -> Result<Channel, ChannelError> + Send + Sync>;

/// The one place the queue's bound is set.
pub(super) fn queue() -> (mpsc::Sender<Input>, mpsc::Receiver<Input>) {
    mpsc::channel(INPUT_QUEUE)
}

/// A reader thread still inside its connect is left to the end of the process.
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
    // Awaited so the driver's channel and replies drop first; the result is only the cancellation.
    drop(driver.await);
}

pub(super) async fn drive(
    hub: Hub,
    mut inputs: mpsc::Receiver<Input>,
    feedback: mpsc::Sender<Input>,
    requests: mpsc::Sender<Request>,
    connector: Connector,
    outlets: Outlets,
) {
    let mut driver = Driver {
        hub,
        machine: Machine::new(),
        outlets,
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
        };
        if flow.is_break() {
            return;
        }
        // One change, then every subscriber gets its turn.
        task::yield_now().await;
    }
}

enum Turn {
    Input(Input),
    Tick,
}

struct Driver {
    hub: Hub,
    machine: Machine,
    outlets: Outlets,
    started: Instant,
    feedback: mpsc::Sender<Input>,
    requests: mpsc::Sender<Request>,
    connector: Connector,
    /// The open channel, from its `Opened` until its close or its `Ended`.
    channel: Option<Arc<Channel>>,
    /// At most `PENDING_MAX` outlive their turn: the machine resolves every other ticket at once.
    replies: HashMap<Ticket, oneshot::Sender<Outcome>>,
    next_ticket: u64,
    /// At most one per action of a step, taken before the queue.
    fed_back: VecDeque<Input>,
    /// `STATE_POPULATED_LINE` was logged since the link last came up.
    announced: bool,
}

impl Driver {
    async fn wait(&self, inputs: &mut mpsc::Receiver<Input>) -> Turn {
        let deadline = self
            .machine
            .deadline()
            .and_then(|at| self.started.checked_add(at));
        let received = match deadline {
            // Served before the queue, which can stay full for as long as the pods keep talking.
            Some(at) if at <= Instant::now() => return Turn::Tick,
            Some(at) => match timeout_at(at, inputs.recv()).await {
                Ok(received) => received,
                Err(_) => return Turn::Tick,
            },
            None => inputs.recv().await,
        };
        match received {
            Some(input) => Turn::Input(input),
            // Never: the driver holds `feedback`, a sender of this same queue.
            None => future::pending().await,
        }
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
            Input::SendFailed => self.step(Event::SendFailed),
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

    /// The link machine, then the policy on its signals, then one publish.
    fn step(&mut self, event: Event<'_>) {
        let before = self.machine.phase();
        let mut next = self.hub.state();
        let actions = self.machine.step(&mut next, event, self.started.elapsed());
        self.log_progress(before, &next);
        for action in &actions {
            if let Action::Signal(signal) = action
                && let Some(notice) = notices::disconnected(*signal)
            {
                notify::offer(&self.outlets.notices, notice);
            }
        }
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
            Action::Call { device, call } => {
                if self
                    .requests
                    .try_send(Request::Call { device, call })
                    .is_err()
                {
                    let failed = DeviceEvent::CallFailed(call);
                    self.fed_back.push_back(Input::Device(failed));
                }
            }
            Action::Resolve { ticket, outcome } => {
                if let Some(reply) = self.replies.remove(&ticket) {
                    // A client that left no longer waits for its answer.
                    reply.send(outcome).ok();
                }
            }
            // Already handed to the policy, before the publish.
            Action::Signal(_) => {}
        }
    }

    fn open(&mut self, device: BdAddr) {
        let connector = Arc::clone(&self.connector);
        let feedback = self.feedback.clone();
        // Never joined: a thread inside connect cannot be woken; the end of the process ends it.
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
            log(&format!("link: {error}"));
            // First, so the machine learns of it before any input that came after the write.
            self.fed_back.push_front(Input::SendFailed);
        }
    }

    fn log_progress(&mut self, before: Phase, state: &State) {
        let after = self.machine.phase();
        // The stop already has its own line, the signal's.
        if before.name() != after.name() && after != Phase::Stopped {
            log(&phase_change_line(before, after));
            self.announced = false;
        }
        if matches!(after, Phase::Up { .. }) && !self.announced && populated(state) {
            log(STATE_POPULATED_LINE);
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
            Self::CallFailed(call) => Event::CallFailed(call),
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

#[cfg(test)]
pub(super) mod tests;
