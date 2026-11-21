//! The control channel's lifecycle as a pure machine: events and the time go in, actions come out.

mod commands;
pub mod ladder;
mod reports;

use std::time::Duration;

use pods_proto::addr::BdAddr;
use pods_proto::handshake::Handshake;
use pods_proto::packets::REQUEST_NOTIFICATIONS;

use crate::command::{self, Command, CommandError, Outcome};
use crate::state::State;
use ladder::{
    ABSENT_GRACE_MS, Attempt, FIRST_RETRY_MS, HANDSHAKE_STEP_TIMEOUT_MS, HELD_DELAYS_MS, KNOWN_MAX,
    NOTIFY_REREQUEST_MS, PARKED_RETRY_MS,
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
    CallFailed(DeviceAction),
    /// The clock reached `deadline()`, or any time a driver wakes.
    Tick,
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
        action: DeviceAction,
    },
    Resolve {
        ticket: Ticket,
        outcome: Outcome,
    },
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

/// The link session: which device has it, what BlueZ last said, and the phase.
#[derive(Debug, Default)]
pub struct Machine {
    phase: Phase,
    /// The device that has the session.
    active: Option<BdAddr>,
    /// BlueZ's last word on the active device: connected.
    held: bool,
    /// The link reached `Up` since the last device reset.
    populated: bool,
    /// Paired AirPods seen, first seen first, at most `KNOWN_MAX`; once full, a new address is not kept.
    known: Vec<BdAddr>,
    last_linked: Option<BdAddr>,
    handshake: Handshake,
    /// Commands written to the pods and waiting for their report, oldest first, at most `PENDING_MAX`.
    pending: Vec<commands::Pending>,
    /// A battery report came on this channel, so the notification request needs no repeat.
    battery_seen: bool,
    /// The settings this channel reported, or stored for the adaptive level, the only ones a command may answer without a write.
    known_settings: commands::KnownSettings,
}

impl Machine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes one event at `now` and changes `state` in one go; the driver carries out the actions in order.
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive match over every phase and event, so no pair is left undecided"
    )]
    pub fn step(&mut self, state: &mut State, event: Event<'_>, now: Duration) -> Vec<Action> {
        use Event as E;
        use Phase as P;
        let mut actions = match (self.phase, event) {
            (P::Stopped, E::Command { ticket, .. }) => {
                // corner: every ticket resolves exactly once, so a stopped machine still refuses one.
                vec![resolve(
                    ticket,
                    Outcome::Refused(CommandError::NotConnected),
                )]
            }
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
                    self.held = true;
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
                    self.forget(device);
                    self.idle(state)
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
                    self.known_settings = commands::KnownSettings::default();
                    let until = after(now, HANDSHAKE_STEP_TIMEOUT_MS);
                    self.enter(state, P::AwaitHandshakeAck { attempt, until });
                    vec![Action::Send(Handshake::first().to_vec())]
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
                    return self.report(state, sdu);
                };
                let until = after(now, HANDSHAKE_STEP_TIMEOUT_MS);
                self.enter(state, P::AwaitFeaturesAck { attempt, until });
                vec![Action::Send(reply.to_vec())]
            }
            (P::AwaitFeaturesAck { .. }, E::Packet(sdu)) => {
                let Some(reply) = self.handshake.on_packet(sdu) else {
                    return self.report(state, sdu);
                };
                let rerequest = (!self.battery_seen).then(|| after(now, NOTIFY_REREQUEST_MS));
                self.enter(state, P::Up { rerequest });
                self.populated = true;
                self.last_linked = self.active;
                vec![Action::Send(reply.to_vec())]
            }
            (P::Up { .. }, E::Packet(sdu)) => self.report(state, sdu),
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
            // Nothing to do: stopped, calls not handled yet, packets outside the handshake and `Up`, events the shell rules out, ticks with no deadline.
            (
                P::Stopped,
                E::DeviceUp(_)
                | E::DeviceDown(_)
                | E::DeviceGone(_)
                | E::BluezLost
                | E::ChannelEnded(_)
                | E::Packet(_)
                | E::CallFailed(_)
                | E::Tick
                | E::Shutdown,
            )
            | (
                P::Idle
                | P::Opening { .. }
                | P::AwaitHandshakeAck { .. }
                | P::AwaitFeaturesAck { .. }
                | P::Up { .. }
                | P::Closing { .. }
                | P::Backoff { .. }
                | P::Parked { .. }
                | P::Grace { .. },
                E::CallFailed(_),
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
            | (P::Idle | P::Opening { .. } | P::Closing { .. }, E::Tick) => Vec::new(),
        };
        self.settle(state, now, &mut actions);
        actions
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
            Phase::Up { rerequest } => rerequest.into_iter().chain(self.confirm_deadline()).min(),
            Phase::Idle | Phase::Opening { .. } | Phase::Closing { .. } | Phase::Stopped => None,
        }
    }

    #[must_use]
    pub fn phase(&self) -> Phase {
        self.phase
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
        self.forget(device);
        self.reset(state);
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

    /// BlueZ says the device left; keep its state for a grace if the link had been up.
    fn absent(&mut self, state: &mut State, now: Duration) -> Vec<Action> {
        self.held = false;
        if !self.populated {
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
        self.reset(state);
        self.enter(state, Phase::Idle);
        vec![Action::Sweep]
    }

    fn reset(&mut self, state: &mut State) {
        state.reset_device();
        self.active = None;
        self.held = false;
        self.populated = false;
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

/// The wait after failed held attempt `attempt`, `None` once the held ladder is spent.
fn held_delay(attempt: u32) -> Option<u64> {
    let index = usize::try_from(attempt).ok()?.checked_sub(1)?;
    HELD_DELAYS_MS.get(index).copied()
}

/// The held ladder is spent or a parked attempt failed: count the failure and park.
fn parked(state: &mut State, now: Duration) -> Phase {
    raise(&mut state.counters.reconnect_failures);
    Phase::Parked {
        until: after(now, PARKED_RETRY_MS),
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
    mod cpp_ladder;
    mod invariants;
    mod known;
    mod ladders;
    mod lifecycle;
    mod reports;
    mod sim;
}
