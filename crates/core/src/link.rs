//! The control channel's lifecycle as a pure machine: events and the time go in, actions come out.

mod commands;
mod devices;
pub mod ladder;
mod reports;
mod wear;

use std::time::Duration;

use pods_proto::addr::BdAddr;
use pods_proto::awareness::Awareness;
use pods_proto::ear::EarReport;
use pods_proto::handshake::Handshake;
use pods_proto::packets::{HANDSHAKE, REQUEST_NOTIFICATIONS};
use pods_proto::uplink::UplinkState;

pub use commands::REPORT_HOLD_MS;
pub use reports::SavedKeys;

use crate::command::{self, Command, CommandError, Outcome};
use crate::state::State;
use ladder::{
    ABSENT_GRACE_MS, Attempt, FIRST_RETRY_MS, HANDSHAKE_STEP_TIMEOUT_MS, KNOWN_MAX,
    NOTIFY_REREQUEST_MS, held_delay, parked,
};

/// Names one client command so its answer finds the client again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ticket(pub u64);

/// How a channel ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The connect never opened the channel.
    ConnectFailed,
    /// An open channel closed, from either side.
    Closed,
}

/// A BlueZ method call on a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceAction {
    Connect,
    Disconnect,
    Forget,
}

/// One BlueZ method call the machine asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Call {
    pub action: DeviceAction,
    /// When the machine asked, so a failure that comes after the device left releases nothing.
    pub session: Session,
}

/// The stretch in which user disconnects count together, ended by a device reset or by BlueZ holding the device again after it stopped holding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Session(u32);

/// Something that happened, handed to `step` by the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event<'a> {
    /// BlueZ reports this AirPods device connected.
    DeviceUp(BdAddr),
    /// BlueZ reports this paired AirPods device disconnected.
    DeviceDown(BdAddr),
    /// BlueZ removed this device.
    DeviceGone(BdAddr),
    /// BlueZ left the system bus, so nothing it said still holds.
    BluezLost,
    ChannelOpened,
    /// Delivered exactly once for every `OpenChannel`, however the channel ended.
    ChannelEnded(End),
    /// One SDU from the control channel.
    Packet(&'a [u8]),
    Command {
        ticket: Ticket,
        command: Command,
    },
    CallFailed(Call),
    /// A `Send` could not be written; the driver hands this in before any other event.
    SendFailed,
    /// The clock reached `deadline()`, or any time a driver wakes.
    Tick,
    /// The system woke from a suspend and its settle has passed.
    Resumed,
    Shutdown,
}

/// Something the driver must do, in the order `step` returns them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Connect the control channel; answered by `ChannelOpened` or `ChannelEnded`.
    OpenChannel(BdAddr),
    /// Close the channel; answered by its `ChannelEnded`.
    CloseChannel,
    Send(Vec<u8>),
    /// Read every device from BlueZ again and report each one as an event.
    Sweep,
    Call {
        device: BdAddr,
        call: Call,
    },
    Resolve {
        ticket: Ticket,
        outcome: Outcome,
    },
    /// For the policy, which the driver hands it before it publishes the state.
    Signal(Signal),
}

/// A moment of the link that other parts of the daemon act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// The link entered `Up`.
    Up { device: BdAddr },
    /// The link left `Up` for any phase but `Stopped`; a later `Lost` carries this step's time as its `down_at`.
    Down,
    /// The device was reset; `down_at` is when its link last left `Up` since the reset before, `None` if it never came up.
    Lost {
        device: BdAddr,
        down_at: Option<Duration>,
        worn_on_link: bool,
    },
    /// The pods answered the proximity keys request.
    Keys(SavedKeys),
    /// The pods said whether their uplink runs, which they also do before the link is up.
    Uplink(UplinkState),
    /// The pods reported a Conversation Awareness level.
    Awareness(Awareness),
}

/// Where the session is, with the data each state needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    /// No active device.
    #[default]
    Idle,
    /// `OpenChannel` issued, no answer yet.
    Opening { attempt: Attempt },
    /// The handshake request sent.
    AwaitHandshakeAck { attempt: Attempt, until: Duration },
    /// The features request sent.
    AwaitFeaturesAck { attempt: Attempt, until: Duration },
    /// The notification request sent; `state.connected` is true exactly here.
    Up { rerequest: Option<Duration> },
    /// `CloseChannel` issued or due, waiting for the end; `attempt` is `None` once the link was up.
    Closing { attempt: Option<Attempt> },
    /// Held by BlueZ, waiting for the next attempt.
    Backoff { attempt: Attempt, until: Duration },
    /// The held ladder spent, still held by BlueZ.
    Parked { until: Duration },
    /// The channel gone and BlueZ says disconnected; the device state is kept until `until`.
    Grace { until: Duration },
    /// After `Shutdown`; every event is ignored except two: a `Command` is refused and a `ChannelOpened` is closed.
    Stopped,
}

impl Phase {
    /// The phase as fixed text for the log, without the data it carries.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Opening { .. } => "opening",
            Self::AwaitHandshakeAck { .. } => "handshake",
            Self::AwaitFeaturesAck { .. } => "features",
            Self::Up { .. } => "up",
            Self::Closing { .. } => "closing",
            Self::Backoff { .. } => "backoff",
            Self::Parked { .. } => "parked",
            Self::Grace { .. } => "grace",
            Self::Stopped => "stopped",
        }
    }
}

/// The log line for a change of phase, read by the hardware tests: `link: idle to opening`.
#[must_use]
pub fn phase_change_line(from: Phase, to: Phase) -> String {
    format!("link: {} to {}", from.name(), to.name())
}

/// The log line once the link is up with a name, a model and a battery, read by the hardware tests.
pub const STATE_POPULATED_LINE: &str = "link: state populated";

/// The link session: which device has it, what BlueZ last said, and the phase.
#[derive(Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag remembers a separate link or channel observation"
)]
pub struct Machine {
    phase: Phase,
    /// The device that has the session.
    active: Option<BdAddr>,
    /// BlueZ's last word on the active device: connected.
    held: bool,
    /// The link reached `Up` since the last device reset.
    populated: bool,
    /// Wear reported on this connection, or a model without wear detection.
    worn_on_link: bool,
    /// The last step that began in `Up`, which is when the link last left it.
    down_at: Duration,
    /// Wear at channel loss, kept while BlueZ still reports the same device connected.
    ear_at_link_down: EarReport,
    session: Session,
    /// User disconnects called in this session and not failed; while any, the device leaving skips the grace and no dial goes out.
    user_disconnects: u32,
    /// Paired AirPods seen, first seen first, at most `KNOWN_MAX`; once full, a new address is not kept.
    known: Vec<BdAddr>,
    last_linked: Option<BdAddr>,
    handshake: Handshake,
    /// A battery report came on this channel, so the notification request needs no repeat.
    battery_seen: bool,
    /// Whether this channel knows the noise mode, and the last value written for each setting.
    channel_settings: commands::ChannelSettings,
    /// A setting written in the last step, answered by the next event.
    written: Option<commands::Written>,
}

impl Machine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes one event at `now` and changes `state` in one go; the driver carries out the actions in order.
    #[must_use]
    pub fn step(&mut self, state: &mut State, event: Event<'_>, now: Duration) -> Vec<Action> {
        let was_up = matches!(self.phase, Phase::Up { .. });
        let ear = state.ear;
        if was_up {
            self.down_at = now;
            self.release_held(state, now);
        }
        let mut actions = self.answer_written(state, event);
        let reacted = self.react(state, event, now);
        let down = !matches!(self.phase, Phase::Up { .. } | Phase::Stopped);
        self.record_wear_at_link_down(was_up, down, ear);
        // A shutdown signals nothing, as it signals no `Lost` either.
        if was_up && down {
            actions.push(Action::Signal(Signal::Down));
        }
        actions.extend(reacted);
        actions
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive match over every phase and event, so no pair is left undecided"
    )]
    fn react(&mut self, state: &mut State, event: Event<'_>, now: Duration) -> Vec<Action> {
        use Event as E;
        use Phase as P;
        match (self.phase, event) {
            (P::Stopped, E::Command { ticket, .. }) => {
                // corner: every ticket resolves exactly once, so a stopped machine still refuses one.
                vec![resolve(
                    ticket,
                    Outcome::Refused(CommandError::NotConnected),
                )]
            }
            (
                _,
                E::Command {
                    ticket,
                    command: Command::Connect,
                },
            ) => self.connect(state, ticket),
            (
                _,
                E::Command {
                    ticket,
                    command: Command::Disconnect,
                },
            ) => self.disconnect(state, ticket),
            (
                _,
                E::Command {
                    ticket,
                    command: Command::Forget,
                },
            ) => self.forget_device(state, ticket),
            (_, E::Resumed) => self.resumed(state, now),
            (
                P::Idle
                | P::Opening { .. }
                | P::AwaitHandshakeAck { .. }
                | P::AwaitFeaturesAck { .. }
                | P::Closing { .. }
                | P::Backoff { .. }
                | P::Parked { .. }
                | P::Grace { .. },
                E::Command { ticket, command },
            ) => vec![resolve(ticket, command::apply(state, &command))],
            (P::Up { .. }, E::Command { ticket, command }) => {
                self.command(state, ticket, command, now)
            }
            (
                P::AwaitHandshakeAck { .. } | P::AwaitFeaturesAck { .. } | P::Up { .. },
                E::Shutdown,
            ) => {
                self.enter(state, P::Stopped);
                vec![Action::CloseChannel]
            }
            (
                P::Idle
                | P::Opening { .. }
                | P::Closing { .. }
                | P::Backoff { .. }
                | P::Parked { .. }
                | P::Grace { .. },
                E::Shutdown,
            ) => {
                // An opening channel is not open yet; the driver drops it when it stops.
                self.enter(state, P::Stopped);
                Vec::new()
            }
            (P::Idle, E::DeviceUp(device)) => {
                if state.advert_device != Some(device) {
                    state.reset_device();
                }
                self.remember(device);
                self.active = Some(device);
                self.open(state, device, Attempt::Held(1))
            }
            (
                P::Opening { .. }
                | P::AwaitHandshakeAck { .. }
                | P::AwaitFeaturesAck { .. }
                | P::Up { .. }
                | P::Closing { .. },
                E::DeviceUp(device),
            ) => {
                if self.is_active(device) {
                    self.held_again();
                } else {
                    self.remember(device);
                }
                Vec::new()
            }
            (P::Backoff { .. } | P::Parked { .. } | P::Grace { .. }, E::DeviceUp(device)) => {
                match (self.is_active(device), self.held) {
                    (true, true) => Vec::new(),
                    (true, false) => self.open(state, device, Attempt::Held(1)),
                    (false, _) => {
                        self.remember(device);
                        Vec::new()
                    }
                }
            }
            (P::Idle, E::DeviceDown(device)) => {
                // corner: a paired device seen with no session is remembered like any other device.
                self.remember(device);
                Vec::new()
            }
            (P::Opening { .. } | P::Closing { .. }, E::DeviceDown(device)) => {
                if self.is_active(device) {
                    self.held = false;
                } else {
                    self.remember(device);
                }
                Vec::new()
            }
            (
                P::AwaitHandshakeAck { attempt, .. } | P::AwaitFeaturesAck { attempt, .. },
                E::DeviceDown(device),
            ) => self.device_down_on_channel(state, device, Some(attempt)),
            (P::Up { .. }, E::DeviceDown(device)) => {
                self.device_down_on_channel(state, device, None)
            }
            (P::Backoff { .. } | P::Parked { .. }, E::DeviceDown(device)) => {
                if self.is_active(device) {
                    self.absent(state, now)
                } else {
                    self.remember(device);
                    Vec::new()
                }
            }
            (P::Grace { .. }, E::DeviceDown(device)) => {
                // corner: the active device is already absent here, so only another device is news.
                if !self.is_active(device) {
                    self.remember(device);
                }
                Vec::new()
            }
            (P::Idle, E::DeviceGone(device)) => {
                self.forget(device);
                Vec::new()
            }
            (P::Opening { .. }, E::DeviceGone(device)) => {
                self.device_gone_on_channel(state, device, Vec::new())
            }
            (
                P::AwaitHandshakeAck { .. } | P::AwaitFeaturesAck { .. } | P::Up { .. },
                E::DeviceGone(device),
            ) => self.device_gone_on_channel(state, device, vec![Action::CloseChannel]),
            (P::Closing { .. }, E::DeviceGone(device)) => {
                // A close is already on its way, so only the reset and the sweep remain.
                self.device_gone_on_channel(state, device, Vec::new())
            }
            (P::Backoff { .. } | P::Parked { .. } | P::Grace { .. }, E::DeviceGone(device)) => {
                if self.is_active(device) {
                    let actions = self.idle(state);
                    self.forget(device);
                    actions
                } else {
                    self.forget(device);
                    Vec::new()
                }
            }
            (
                P::Idle
                | P::Opening { .. }
                | P::AwaitHandshakeAck { .. }
                | P::AwaitFeaturesAck { .. }
                | P::Up { .. }
                | P::Closing { .. }
                | P::Grace { .. },
                E::BluezLost,
            ) => {
                // corner: no close here, the socket is the truth while it lives; a channel still opening is closed once it opens.
                self.held = false;
                Vec::new()
            }
            (P::Backoff { .. } | P::Parked { .. }, E::BluezLost) => self.absent(state, now),
            (P::Opening { attempt }, E::ChannelOpened) => {
                if self.held {
                    self.handshake = Handshake::default();
                    self.battery_seen = false;
                    self.channel_settings = commands::ChannelSettings::default();
                    let until = after(now, HANDSHAKE_STEP_TIMEOUT_MS);
                    self.enter(state, P::AwaitHandshakeAck { attempt, until });
                    vec![Action::Send(HANDSHAKE.to_vec())]
                } else {
                    self.enter(
                        state,
                        P::Closing {
                            attempt: Some(attempt),
                        },
                    );
                    vec![Action::CloseChannel]
                }
            }
            (P::Closing { .. }, E::ChannelOpened) => {
                // Only a device gone while opening reaches this: its channel opened after all.
                vec![Action::CloseChannel]
            }
            (P::Stopped, E::ChannelOpened) => {
                // A connect in flight at shutdown opened after all; no channel stays open once stopped.
                vec![Action::CloseChannel]
            }
            (
                P::Opening { attempt }
                | P::AwaitHandshakeAck { attempt, .. }
                | P::AwaitFeaturesAck { attempt, .. },
                E::ChannelEnded(_),
            ) => self.ended(state, Some(attempt), now),
            (P::Up { .. }, E::ChannelEnded(_)) => self.ended(state, None, now),
            (P::Closing { attempt }, E::ChannelEnded(_)) => self.ended(state, attempt, now),
            (P::AwaitHandshakeAck { attempt, .. }, E::Packet(sdu)) => {
                let Some(reply) = self.handshake.on_packet(sdu) else {
                    return self.report(state, sdu, now);
                };
                let until = after(now, HANDSHAKE_STEP_TIMEOUT_MS);
                self.enter(state, P::AwaitFeaturesAck { attempt, until });
                vec![Action::Send(reply.to_vec())]
            }
            (P::AwaitFeaturesAck { .. }, E::Packet(sdu)) => {
                let Some(reply) = self.handshake.on_packet(sdu) else {
                    return self.report(state, sdu, now);
                };
                let rerequest = (!self.battery_seen).then(|| after(now, NOTIFY_REREQUEST_MS));
                self.enter(state, P::Up { rerequest });
                self.populated = true;
                self.last_linked = self.active;
                let mut actions = vec![Action::Send(reply.to_vec())];
                actions.extend(
                    self.active
                        .map(|device| Action::Signal(Signal::Up { device })),
                );
                actions
            }
            (P::Up { .. }, E::Packet(sdu)) => self.report(state, sdu, now),
            (
                P::AwaitHandshakeAck { attempt, .. } | P::AwaitFeaturesAck { attempt, .. },
                E::SendFailed,
            ) => {
                self.enter(
                    state,
                    P::Closing {
                        attempt: Some(attempt),
                    },
                );
                vec![Action::CloseChannel]
            }
            (P::Up { .. }, E::SendFailed) => {
                self.enter(state, P::Closing { attempt: None });
                vec![Action::CloseChannel]
            }
            (
                P::AwaitHandshakeAck { attempt, until } | P::AwaitFeaturesAck { attempt, until },
                E::Tick,
            ) => {
                if now < until {
                    return Vec::new();
                }
                self.enter(
                    state,
                    P::Closing {
                        attempt: Some(attempt),
                    },
                );
                vec![Action::CloseChannel]
            }
            (P::Up { rerequest }, E::Tick) => match rerequest {
                Some(at) if now >= at => {
                    self.enter(state, P::Up { rerequest: None });
                    vec![Action::Send(REQUEST_NOTIFICATIONS.to_vec())]
                }
                Some(_) | None => Vec::new(),
            },
            (P::Backoff { attempt, until }, E::Tick) => self.retry(state, attempt, until, now),
            (P::Parked { until }, E::Tick) => self.retry(state, Attempt::Parked, until, now),
            (P::Grace { until }, E::Tick) => {
                if now < until {
                    return Vec::new();
                }
                raise(&mut state.counters.reconnect_failures);
                self.idle(state)
            }
            (
                P::Idle
                | P::Opening { .. }
                | P::AwaitHandshakeAck { .. }
                | P::AwaitFeaturesAck { .. }
                | P::Up { .. }
                | P::Closing { .. }
                | P::Backoff { .. }
                | P::Parked { .. }
                | P::Grace { .. },
                E::CallFailed(call),
            ) => self.call_failed(state, call),
            // Nothing to do: stopped, packets outside the handshake and `Up`, events the shell rules out, ticks with no deadline.
            (
                P::Stopped,
                E::DeviceUp(_)
                | E::DeviceDown(_)
                | E::DeviceGone(_)
                | E::BluezLost
                | E::ChannelEnded(_)
                | E::Packet(_)
                | E::CallFailed(_)
                | E::SendFailed
                | E::Tick
                | E::Shutdown,
            )
            | (
                P::Idle
                | P::AwaitHandshakeAck { .. }
                | P::AwaitFeaturesAck { .. }
                | P::Up { .. }
                | P::Backoff { .. }
                | P::Parked { .. }
                | P::Grace { .. },
                E::ChannelOpened,
            )
            | (
                P::Idle | P::Backoff { .. } | P::Parked { .. } | P::Grace { .. },
                E::ChannelEnded(_),
            )
            | (
                P::Idle
                | P::Opening { .. }
                | P::Closing { .. }
                | P::Backoff { .. }
                | P::Parked { .. }
                | P::Grace { .. },
                E::Packet(_),
            )
            | (
                P::Idle
                | P::Opening { .. }
                | P::Closing { .. }
                | P::Backoff { .. }
                | P::Parked { .. }
                | P::Grace { .. },
                E::SendFailed,
            )
            | (P::Idle | P::Opening { .. } | P::Closing { .. }, E::Tick) => Vec::new(),
        }
    }

    /// When the driver must send `Tick` next, if anything is waiting on the clock.
    #[must_use]
    pub fn deadline(&self) -> Option<Duration> {
        match self.phase {
            Phase::AwaitHandshakeAck { until, .. }
            | Phase::AwaitFeaturesAck { until, .. }
            | Phase::Backoff { until, .. }
            | Phase::Parked { until }
            | Phase::Grace { until } => Some(until),
            Phase::Up { rerequest } => rerequest
                .into_iter()
                .chain(self.written_deadline())
                .chain(self.channel_settings.release_deadline())
                .min(),
            Phase::Idle | Phase::Opening { .. } | Phase::Closing { .. } | Phase::Stopped => None,
        }
    }

    #[must_use]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    #[must_use]
    pub fn connected_device(&self) -> Option<BdAddr> {
        if matches!(self.phase, Phase::Up { .. }) {
            self.active
        } else {
            None
        }
    }

    /// Every phase change goes through here, so `connected` is true exactly in `Up`.
    fn enter(&mut self, state: &mut State, phase: Phase) {
        self.phase = phase;
        state.connected = matches!(phase, Phase::Up { .. });
    }

    fn open(&mut self, state: &mut State, device: BdAddr, attempt: Attempt) -> Vec<Action> {
        self.held = true;
        self.enter(state, Phase::Opening { attempt });
        vec![Action::OpenChannel(device)]
    }

    /// The next attempt of a ladder, once its time has come.
    fn retry(
        &mut self,
        state: &mut State,
        attempt: Attempt,
        until: Duration,
        now: Duration,
    ) -> Vec<Action> {
        if now < until {
            return Vec::new();
        }
        // corner: Backoff and Parked always have an active device; without one there is nothing to retry.
        let Some(device) = self.active else {
            return self.idle(state);
        };
        if self.user_disconnects > 0 {
            // corner: the user's disconnect ended the channel before BlueZ said the device left, and a dial now would page it back.
            let until = after(now, FIRST_RETRY_MS);
            self.enter(state, Phase::Backoff { attempt, until });
            return Vec::new();
        }
        raise(&mut state.counters.reconnect_attempts);
        self.open(state, device, attempt)
    }

    /// BlueZ says the active device left while its channel is open.
    fn device_down_on_channel(
        &mut self,
        state: &mut State,
        device: BdAddr,
        attempt: Option<Attempt>,
    ) -> Vec<Action> {
        if !self.is_active(device) {
            self.remember(device);
            return Vec::new();
        }
        self.held = false;
        self.enter(state, Phase::Closing { attempt });
        vec![Action::CloseChannel]
    }

    /// The active device was removed while a channel is open or opening: reset at once, then wait for the end.
    fn device_gone_on_channel(
        &mut self,
        state: &mut State,
        device: BdAddr,
        mut actions: Vec<Action>,
    ) -> Vec<Action> {
        if !self.is_active(device) {
            self.forget(device);
            return Vec::new();
        }
        actions.extend(self.reset(state));
        self.forget(device);
        self.enter(state, Phase::Closing { attempt: None });
        actions.push(Action::Sweep);
        actions
    }

    /// The channel ended; `failed` is the attempt that never reached `Up`, `None` after a link that was up.
    fn ended(&mut self, state: &mut State, failed: Option<Attempt>, now: Duration) -> Vec<Action> {
        if self.active.is_none() {
            return self.idle(state);
        }
        if !self.held {
            return self.absent(state, now);
        }
        let next = match failed {
            None => Phase::Backoff {
                attempt: Attempt::Held(1),
                until: after(now, FIRST_RETRY_MS),
            },
            Some(Attempt::Held(attempt)) => match held_delay(attempt) {
                Some(delay) => Phase::Backoff {
                    attempt: Attempt::Held(attempt.saturating_add(1)),
                    until: after(now, delay),
                },
                None => parked(state, now),
            },
            Some(Attempt::Parked) => parked(state, now),
        };
        self.enter(state, next);
        Vec::new()
    }

    /// BlueZ says the device left; keep its state for a grace if the link had been up and the user did not disconnect it.
    fn absent(&mut self, state: &mut State, now: Duration) -> Vec<Action> {
        self.held = false;
        if !self.populated || self.user_disconnects > 0 {
            return self.idle(state);
        }
        self.enter(
            state,
            Phase::Grace {
                until: after(now, ABSENT_GRACE_MS),
            },
        );
        Vec::new()
    }

    /// Resets the device and the session, then sweeps for the next device.
    fn idle(&mut self, state: &mut State) -> Vec<Action> {
        let mut actions = Vec::from_iter(self.reset(state));
        self.enter(state, Phase::Idle);
        actions.push(Action::Sweep);
        actions
    }

    fn is_active(&self, device: BdAddr) -> bool {
        self.active == Some(device)
    }

    fn remember(&mut self, device: BdAddr) {
        // A stranger never pushes out a known device, the active one or the last one linked.
        if self.known.contains(&device) || self.known.len() >= KNOWN_MAX {
            return;
        }
        self.known.push(device);
    }

    /// Forgets a removed device, and its link history so no later command picks it.
    fn forget(&mut self, device: BdAddr) {
        self.known.retain(|known| *known != device);
        if self.last_linked == Some(device) {
            self.last_linked = None;
        }
        if self.is_active(device) {
            self.active = None;
        }
    }
}

fn after(now: Duration, millis: u64) -> Duration {
    now.saturating_add(Duration::from_millis(millis))
}

fn resolve(ticket: Ticket, outcome: Outcome) -> Action {
    Action::Resolve { ticket, outcome }
}

fn raise(counter: &mut u64) {
    *counter = counter.saturating_add(1);
}

#[cfg(test)]
mod tests {
    mod commands;
    mod devices;
    mod invariants;
    mod known;
    mod ladder_timing;
    mod ladders;
    mod lifecycle;
    mod log_lines;
    mod reports;
    mod resumed;
    mod signals;
    mod sim;
}
