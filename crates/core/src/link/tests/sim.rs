//! A fake shell under virtual time: it feeds the machine, plays the channel and the watcher, and checks the invariants.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use pods_proto::addr::BdAddr;
use pods_proto::packets::{HANDSHAKE, REQUEST_NOTIFICATIONS, SET_SPECIFIC_FEATURES};

use crate::command::{Command, Outcome};
use crate::link::ladder::PENDING_MAX;
use crate::link::{Action, End, Event, Machine, Phase, Ticket};
use crate::state::State;

/// Synthetic device addresses, never a real one.
pub(super) const DEVICES: [&str; 2] = ["02:00:00:00:00:01", "02:00:00:00:00:02"];
/// Actions one step may return: every pending command resolved, plus the lifecycle's own.
const ACTIONS_MAX: usize = PENDING_MAX + 4;
/// Steps one input may cause before the simulation calls it a livelock.
const CASCADE_MAX: usize = 64;
/// Ticks one `run_until` may send before it calls the deadline stuck.
const TICKS_MAX: usize = 1000;

/// How the shell answers the next `OpenChannel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Answer {
    Open,
    Refuse,
    /// No answer until a `Release`.
    Hold,
}

/// One thing that happens to the shell.
#[derive(Debug, Clone)]
pub(super) enum Input {
    Up(usize),
    Down(usize),
    Gone(usize),
    BluezLost,
    Packet(Vec<u8>),
    Command(Command),
    CallFailed,
    Tick,
    /// Moves the clock on, ticking at every deadline on the way.
    Wait(Duration),
    /// Scripts the answer to the next open.
    NextOpen(Answer),
    /// Answers a held open: true opens it, false fails it.
    Release(bool),
    /// Scripts the next `Send` to fail.
    NextWriteFails,
    PeerCloses,
    Shutdown,
}

/// What BlueZ last said about a device, as the watcher would replay it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    Nothing,
    Up,
    Down,
    Gone,
    /// BlueZ left the bus after this device's last event.
    Lost,
}

/// The control channel as the reader thread sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Channel {
    Free,
    Held,
    Open,
}

/// An event waiting to be delivered, owned so the queue can hold it.
#[derive(Debug, Clone)]
enum Queued {
    Up(usize),
    Down(usize),
    Gone(usize),
    BluezLost,
    Opened,
    Ended(End),
    Packet(Vec<u8>),
    Command(Command),
    CallFailed,
    SendFailed,
    Tick,
    Shutdown,
}

/// One call of `step` as the log keeps it.
#[derive(Debug)]
pub(super) struct Step {
    pub(super) at: Duration,
    pub(super) event: String,
    pub(super) actions: Vec<String>,
}

#[derive(Debug)]
pub(super) struct Sim {
    pub(super) machine: Machine,
    pub(super) state: State,
    pub(super) now: Duration,
    pub(super) steps: Vec<Step>,
    default_answer: Answer,
    answers: VecDeque<Answer>,
    channel: Channel,
    seen: [Seen; DEVICES.len()],
    queue: VecDeque<Queued>,
    write_fails: bool,
    next_ticket: u64,
    resolves: BTreeMap<u64, Vec<Outcome>>,
    violations: Vec<String>,
}

impl Sim {
    /// A shell whose opens succeed until scripted otherwise.
    pub(super) fn new() -> Self {
        Self::answering(Answer::Open)
    }

    pub(super) fn answering(default_answer: Answer) -> Self {
        Self {
            machine: Machine::new(),
            state: State::default(),
            now: Duration::ZERO,
            steps: Vec::new(),
            default_answer,
            answers: VecDeque::new(),
            channel: Channel::Free,
            seen: [Seen::Nothing; DEVICES.len()],
            queue: VecDeque::new(),
            write_fails: false,
            next_ticket: 0,
            resolves: BTreeMap::new(),
            violations: Vec::new(),
        }
    }

    pub(super) fn feed(&mut self, input: Input) {
        let queued = match input {
            Input::Up(device) => Queued::Up(device),
            Input::Down(device) => Queued::Down(device),
            Input::Gone(device) => Queued::Gone(device),
            Input::BluezLost => Queued::BluezLost,
            Input::Packet(bytes) => Queued::Packet(bytes),
            Input::Command(command) => Queued::Command(command),
            Input::CallFailed => Queued::CallFailed,
            Input::Tick => Queued::Tick,
            Input::Shutdown => Queued::Shutdown,
            Input::Wait(span) => return self.run_until(self.now.saturating_add(span)),
            Input::NextOpen(answer) => return self.answers.push_back(answer),
            Input::NextWriteFails => {
                self.write_fails = true;
                return;
            }
            Input::Release(opened) => {
                if self.channel != Channel::Held {
                    return;
                }
                self.answer_open(if opened { Answer::Open } else { Answer::Refuse });
                return self.settle();
            }
            Input::PeerCloses => {
                if self.channel != Channel::Open {
                    return;
                }
                self.channel = Channel::Free;
                Queued::Ended(End::Closed)
            }
        };
        self.queue.push_back(queued);
        self.settle();
    }

    /// Delivers what is queued, then ticks while a deadline has already passed, as the driver does.
    fn settle(&mut self) {
        self.drain();
        self.run_until(self.now);
    }

    /// Jumps the clock to each deadline up to `until` in turn and ticks there, then to `until`.
    pub(super) fn run_until(&mut self, until: Duration) {
        for _ in 0..TICKS_MAX {
            match self.machine.deadline() {
                Some(at) if at <= until => {
                    self.now = self.now.max(at);
                    self.queue.push_back(Queued::Tick);
                    self.drain();
                }
                Some(_) | None => {
                    self.now = self.now.max(until);
                    return;
                }
            }
        }
        self.violations
            .push(format!("the deadline did not move after {TICKS_MAX} ticks"));
    }

    /// Brings `device` up through the whole handshake.
    pub(super) fn bring_up(&mut self, device: usize) {
        self.feed(Input::NextOpen(Answer::Open));
        self.feed(Input::Up(device));
        self.feed(Input::Packet(pods_samples::handshake_ack()));
        self.feed(Input::Packet(pods_samples::features_ack()));
        assert_eq!(
            self.machine.phase(),
            Phase::Up {
                rerequest: self.machine.deadline()
            }
        );
    }

    /// The actions of the last `count` steps, in order.
    pub(super) fn since(&self, count: usize) -> Vec<String> {
        let first = self.steps.len().saturating_sub(count);
        self.steps
            .iter()
            .skip(first)
            .flat_map(|step| step.actions.clone())
            .collect()
    }

    /// The actions of the last step.
    pub(super) fn last(&self) -> Vec<String> {
        self.steps
            .last()
            .map_or_else(Vec::new, |step| step.actions.clone())
    }

    /// The last step whose event is `event`.
    pub(super) fn step_of(&self, event: &str) -> &Step {
        self.steps
            .iter()
            .rev()
            .find(|step| step.event == event)
            .unwrap()
    }

    /// Every action with the time it was returned, in order.
    pub(super) fn actions(&self) -> Vec<(u64, String)> {
        self.steps
            .iter()
            .flat_map(|step| {
                step.actions
                    .iter()
                    .map(|action| (millis(step.at), action.clone()))
            })
            .collect()
    }

    /// The times of every action named `name`.
    pub(super) fn times_of(&self, name: &str) -> Vec<u64> {
        self.actions()
            .into_iter()
            .filter(|(_, action)| action == name)
            .map(|(at, _)| at)
            .collect()
    }

    /// Asserts every invariant held at every step so far.
    pub(super) fn check(&self) {
        assert!(self.violations.is_empty(), "{:#?}", self.violations);
    }

    /// Asserts every ticket handed in was resolved exactly once.
    pub(super) fn check_all_tickets_resolved_once(&self) {
        self.check();
        let resolved: Vec<u64> = self.resolves.keys().copied().collect();
        let handed_in: Vec<u64> = (0..self.next_ticket).collect();
        assert_eq!(resolved, handed_in);
        for (ticket, outcomes) in &self.resolves {
            assert_eq!(outcomes.len(), 1, "ticket {ticket} resolved {outcomes:?}");
        }
    }

    pub(super) fn outcomes(&self) -> Vec<Outcome> {
        self.resolves.values().flatten().copied().collect()
    }

    /// Every outcome ticket `ticket` resolved with, in order.
    pub(super) fn outcome(&self, ticket: u64) -> Vec<Outcome> {
        self.resolves.get(&ticket).cloned().unwrap_or_default()
    }

    fn drain(&mut self) {
        for _ in 0..CASCADE_MAX {
            let Some(queued) = self.queue.pop_front() else {
                return;
            };
            self.deliver(&queued);
        }
        self.violations
            .push(format!("more than {CASCADE_MAX} steps from one input"));
        self.queue.clear();
    }

    fn deliver(&mut self, queued: &Queued) {
        let label = format!("{queued:?}");
        let event = match *queued {
            Queued::Up(device) => self.device_event(device, Seen::Up, Event::DeviceUp),
            Queued::Down(device) => self.device_event(device, Seen::Down, Event::DeviceDown),
            Queued::Gone(device) => self.device_event(device, Seen::Gone, Event::DeviceGone),
            Queued::BluezLost => {
                self.seen = self
                    .seen
                    .map(|seen| if seen == Seen::Up { Seen::Lost } else { seen });
                Event::BluezLost
            }
            Queued::Opened => Event::ChannelOpened,
            Queued::Ended(end) => Event::ChannelEnded(end),
            Queued::Packet(ref packet) => Event::Packet(packet),
            Queued::Command(command) => {
                let ticket = Ticket(self.next_ticket);
                self.next_ticket = self.next_ticket.saturating_add(1);
                Event::Command { ticket, command }
            }
            Queued::CallFailed => Event::CallFailed(crate::link::DeviceAction::Connect),
            Queued::SendFailed => Event::SendFailed,
            Queued::Tick => Event::Tick,
            Queued::Shutdown => Event::Shutdown,
        };
        let actions = self.machine.step(&mut self.state, event, self.now);
        if actions.len() > ACTIONS_MAX {
            self.violations
                .push(format!("{label}: {} actions", actions.len()));
        }
        let described = actions.into_iter().map(|action| self.act(action)).collect();
        self.steps.push(Step {
            at: self.now,
            event: label.clone(),
            actions: described,
        });
        self.check_step(&label);
    }

    fn device_event(
        &mut self,
        device: usize,
        seen: Seen,
        event: fn(BdAddr) -> Event<'static>,
    ) -> Event<'static> {
        self.seen[device] = seen;
        event(addr(device))
    }

    /// Plays the shell's part for one action and names it for the log.
    fn act(&mut self, action: Action) -> String {
        match action {
            Action::OpenChannel(peer) => {
                let device = index(peer);
                if self.channel != Channel::Free {
                    self.violations
                        .push("OpenChannel while one is unanswered".into());
                }
                if self.seen[device] != Seen::Up {
                    self.violations.push(format!(
                        "OpenChannel({device}) after {:?}",
                        self.seen[device]
                    ));
                }
                let answer = self.answers.pop_front().unwrap_or(self.default_answer);
                self.answer_open(answer);
                format!("Open({device})")
            }
            Action::CloseChannel => {
                if self.channel == Channel::Open {
                    self.channel = Channel::Free;
                    self.queue.push_back(Queued::Ended(End::Closed));
                } else {
                    self.violations
                        .push(format!("CloseChannel on a {:?} channel", self.channel));
                }
                "Close".into()
            }
            Action::Send(bytes) => {
                if self.write_fails {
                    self.write_fails = false;
                    // The driver hands a failed write in before anything already waiting.
                    self.queue.push_front(Queued::SendFailed);
                }
                send_label(&bytes)
            }
            Action::Sweep => {
                for (device, seen) in self.seen.iter().enumerate() {
                    if *seen == Seen::Up {
                        self.queue.push_back(Queued::Up(device));
                    }
                }
                "Sweep".into()
            }
            Action::Call { device, action } => format!("Call({}, {action:?})", index(device)),
            Action::Resolve { ticket, outcome } => {
                self.resolves.entry(ticket.0).or_default().push(outcome);
                format!("Resolve({})", ticket.0)
            }
        }
    }

    fn answer_open(&mut self, answer: Answer) {
        self.channel = match answer {
            Answer::Open => {
                self.queue.push_back(Queued::Opened);
                Channel::Open
            }
            Answer::Refuse => {
                self.queue.push_back(Queued::Ended(End::ConnectFailed));
                Channel::Free
            }
            Answer::Hold => Channel::Held,
        };
    }

    fn check_step(&mut self, label: &str) {
        let phase = self.machine.phase();
        let up = matches!(phase, Phase::Up { .. });
        if self.state.connected != up {
            self.violations.push(format!(
                "{label}: connected {} in {phase:?}",
                self.state.connected
            ));
        }
        let holds_deadline = matches!(
            phase,
            Phase::AwaitHandshakeAck { .. }
                | Phase::AwaitFeaturesAck { .. }
                | Phase::Backoff { .. }
                | Phase::Parked { .. }
                | Phase::Grace { .. }
        );
        if holds_deadline && self.machine.deadline().is_none() {
            self.violations
                .push(format!("{label}: no deadline in {phase:?}"));
        }
        if self.machine.active.is_some() && phase == Phase::Idle {
            self.violations
                .push(format!("{label}: Idle with an active device"));
        }
        let pending = self.machine.pending.len();
        if pending > 0 && !up {
            self.violations
                .push(format!("{label}: {pending} commands pending in {phase:?}"));
        }
        if pending > 0 && self.machine.deadline().is_none() {
            self.violations.push(format!(
                "{label}: {pending} commands pending and no deadline"
            ));
        }
        if pending > PENDING_MAX {
            self.violations
                .push(format!("{label}: {pending} commands pending"));
        }
    }
}

/// The synthetic address of device `device`.
pub(super) fn addr(device: usize) -> BdAddr {
    DEVICES[device].parse().unwrap()
}

fn index(peer: BdAddr) -> usize {
    (0..DEVICES.len())
        .find(|device| addr(*device) == peer)
        .unwrap()
}

pub(super) fn millis(at: Duration) -> u64 {
    u64::try_from(at.as_millis()).unwrap()
}

pub(super) fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

/// A sent frame as the log names it.
fn send_label(bytes: &[u8]) -> String {
    match bytes {
        b if b == HANDSHAKE => "Send(handshake)".into(),
        b if b == SET_SPECIFIC_FEATURES => "Send(features)".into(),
        b if b == REQUEST_NOTIFICATIONS => "Send(notifications)".into(),
        other => format!("Send({other:02x?})"),
    }
}
