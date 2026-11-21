//! Commands to bytes while the link is up, and the commands that wait for the pods' own report.

use std::time::Duration;

use pods_proto::control::{ControlCommand, ControlId};
use pods_proto::model::Capabilities;
use pods_proto::noise::NoiseMode;

use super::ladder::{CONFIRM_TIMEOUT_MS, PENDING_MAX};
use super::{Action, Event, Machine, Phase, Ticket, after, raise, resolve};
use crate::command::{self, AdaptiveLevel, Command, CommandError, NoiseTarget, Outcome};
use crate::state::{Counters, State};

/// A command written to the pods, waiting for the report that shows it carried out.
#[derive(Debug, Clone, Copy)]
pub(super) struct Pending {
    ticket: Ticket,
    /// The report that confirms it: the same identifier and first value byte.
    expect: ControlCommand,
    deadline: Duration,
}

/// An adaptive level whose write the driver has not yet answered for.
#[derive(Debug, Clone, Copy)]
pub(super) struct Stored {
    ticket: Ticket,
    level: AdaptiveLevel,
    /// When it was written; the deadline that makes the driver step again at once.
    at: Duration,
}

/// The settings the pods reported on the current channel, and whether the adaptive level was stored on it.
#[derive(Debug, Clone, Copy, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per setting, all cleared together when a channel opens"
)]
pub(super) struct KnownSettings {
    pub(super) noise: bool,
    pub(super) conversational_awareness: bool,
    pub(super) one_bud_anc: bool,
    pub(super) adaptive_level: bool,
}

/// The setting a written command changes, for the counter it raises.
#[derive(Debug, Clone, Copy)]
enum Setting {
    Noise,
    ConversationalAwareness,
    OneBudAnc,
}

impl Setting {
    fn changes(self, counters: &mut Counters) -> &mut u64 {
        match self {
            Self::Noise => &mut counters.noise_control_changes,
            Self::ConversationalAwareness => &mut counters.conversational_awareness_changes,
            Self::OneBudAnc => &mut counters.one_bud_anc_changes,
        }
    }
}

/// What a command takes while the link is up.
#[derive(Debug, Clone, Copy)]
enum Handling {
    /// Answered from the state, with no bytes written.
    Answer(Outcome),
    /// Not a setting of the pods: answered as it is with no link.
    AsWithoutLink,
    /// Written, then answered by the pods' report or by the deadline.
    Wait(Setting, ControlCommand),
    /// Written and stored at once, since the pods never report it back.
    Store(AdaptiveLevel),
}

impl Machine {
    /// A command in `Up`: answered at once, or written and left waiting for its report.
    pub(super) fn command(
        &mut self,
        state: &mut State,
        ticket: Ticket,
        command: Command,
        now: Duration,
    ) -> Vec<Action> {
        match handling(state, self.known_settings, &self.pending, command) {
            Handling::Answer(outcome) => vec![resolve(ticket, outcome)],
            Handling::AsWithoutLink => vec![resolve(ticket, command::apply(state, &command))],
            Handling::Wait(setting, expect) => {
                if self.pending.len() >= PENDING_MAX {
                    return vec![resolve(ticket, Outcome::Refused(CommandError::Timeout))];
                }
                raise(setting.changes(&mut state.counters));
                self.pending.push(Pending {
                    ticket,
                    expect,
                    deadline: after(now, CONFIRM_TIMEOUT_MS),
                });
                vec![Action::Send(expect.encode())]
            }
            Handling::Store(level) => {
                self.stored = Some(Stored {
                    ticket,
                    level,
                    at: now,
                });
                let frame = ControlCommand {
                    id: ControlId::ADAPTIVE_NOISE_LEVEL,
                    value: [level.percent(), 0, 0, 0],
                };
                vec![Action::Send(frame.encode())]
            }
        }
    }

    /// Answers the level written in the step before `event`: refused when its write failed, else stored and sent.
    pub(super) fn written(&mut self, state: &mut State, event: Event<'_>) -> Vec<Action> {
        let Some(stored) = self.stored.take() else {
            return Vec::new();
        };
        if event == Event::SendFailed {
            let refused = Outcome::Refused(CommandError::NotConnected);
            return vec![resolve(stored.ticket, refused)];
        }
        state.adaptive_noise_level = stored.level;
        self.known_settings.adaptive_level = true;
        raise(&mut state.counters.adaptive_level_changes);
        vec![resolve(stored.ticket, Outcome::Sent)]
    }

    pub(super) fn stored_deadline(&self) -> Option<Duration> {
        self.stored.map(|stored| stored.at)
    }

    /// Confirms every waiting command that `report` shows carried out.
    pub(super) fn confirm(&mut self, report: ControlCommand) -> Vec<Action> {
        self.pending
            .extract_if(.., |pending| shows(report, pending.expect))
            .map(|pending| resolve(pending.ticket, Outcome::Confirmed))
            .collect()
    }

    /// The earliest deadline of a waiting command.
    pub(super) fn confirm_deadline(&self) -> Option<Duration> {
        self.pending.iter().map(|pending| pending.deadline).min()
    }

    /// Ends each waiting command whose time has run out, then, outside `Up`, every one that is left.
    pub(super) fn settle(&mut self, state: &State, now: Duration, actions: &mut Vec<Action>) {
        let expired = self
            .pending
            .extract_if(.., |pending| pending.deadline <= now)
            .map(|pending| {
                resolve(
                    pending.ticket,
                    Outcome::Refused(unanswered(state, pending.expect)),
                )
            });
        actions.extend(expired);
        if matches!(self.phase, Phase::Up { .. }) {
            return;
        }
        // Nothing can report back once the link has left `Up`.
        let refused = self
            .pending
            .drain(..)
            .map(|pending| resolve(pending.ticket, Outcome::Refused(CommandError::NotConnected)));
        actions.extend(refused);
    }
}

/// Whether `report` carries the value `expect` asked for.
fn shows(report: ControlCommand, expect: ControlCommand) -> bool {
    report.id == expect.id && report.value[0] == expect.value[0]
}

/// The refusal for a command no report answered in time: a noise change on pods out of every ear is `not_worn`.
fn unanswered(state: &State, expect: ControlCommand) -> CommandError {
    let worn = state.ear.any_in_ear() || state.model.capabilities().is_headset;
    if expect.id == ControlId::NOISE_CONTROL && !worn {
        CommandError::NotWorn
    } else {
        CommandError::Timeout
    }
}

/// What `command` takes in `Up`, read from the state, what this channel reported, the commands still waiting and the model's features.
fn handling(
    state: &State,
    known: KnownSettings,
    pending: &[Pending],
    command: Command,
) -> Handling {
    let features = state.model.capabilities();
    match command {
        Command::SetNoise(target) => noise(state, known, pending, features, target),
        Command::SetConversationalAwareness(enabled) => switch(
            features.conversational_awareness,
            known
                .conversational_awareness
                .then_some(state.conversational_awareness),
            enabled,
            Setting::ConversationalAwareness,
            ControlId::CONVERSATIONAL_AWARENESS,
            pending,
        ),
        Command::SetOneBudAnc(enabled) => switch(
            features.one_bud_anc,
            known.one_bud_anc.then_some(state.one_bud_anc_mode),
            enabled,
            Setting::OneBudAnc,
            ControlId::ONE_BUD_ANC,
            pending,
        ),
        Command::SetAdaptiveLevel(level) => adaptive_level(state, known, features, level),
        // Never reached for connect, disconnect and forget, which `step` answers in every phase.
        Command::SetEarDetection(_)
        | Command::Reopen
        | Command::Connect
        | Command::Disconnect
        | Command::Forget => Handling::AsWithoutLink,
    }
}

/// The newest write still waiting for a report under `id`: the value the pods are on their way to.
fn on_its_way(pending: &[Pending], id: ControlId) -> Option<ControlCommand> {
    pending
        .iter()
        .rev()
        .map(|entry| entry.expect)
        .find(|expect| expect.id == id)
}

fn noise(
    state: &State,
    known: KnownSettings,
    pending: &[Pending],
    features: Capabilities,
    target: NoiseTarget,
) -> Handling {
    if !features.noise_control {
        return Handling::Answer(Outcome::Refused(CommandError::NotSupported));
    }
    let on_its_way = on_its_way(pending, ControlId::NOISE_CONTROL);
    let mode = match target {
        NoiseTarget::Mode(mode) => mode,
        NoiseTarget::Cycle => {
            let from = on_its_way.and_then(|expect| NoiseMode::from_command(&expect).ok());
            next_offered(from.unwrap_or(state.noise_mode), features)
        }
    };
    if !offers(features, mode) {
        return Handling::Answer(Outcome::Refused(CommandError::NotSupported));
    }
    if on_its_way.is_none() && known.noise.then_some(state.noise_mode) == Some(mode) {
        return Handling::Answer(Outcome::Confirmed);
    }
    Handling::Wait(Setting::Noise, mode.command())
}

fn next_offered(current: NoiseMode, features: Capabilities) -> NoiseMode {
    let mut mode = current.next();
    // Ends within three steps: every model offers Anc and Transparency.
    while !offers(features, mode) {
        mode = mode.next();
    }
    mode
}

fn offers(features: Capabilities, mode: NoiseMode) -> bool {
    match mode {
        NoiseMode::Off => features.noise_off,
        NoiseMode::Adaptive => features.adaptive,
        NoiseMode::Anc | NoiseMode::Transparency => true,
    }
}

/// `current` is the value reported on this channel, if any.
fn switch(
    supported: bool,
    current: Option<bool>,
    enabled: bool,
    setting: Setting,
    id: ControlId,
    pending: &[Pending],
) -> Handling {
    if !supported {
        return Handling::Answer(Outcome::Refused(CommandError::NotSupported));
    }
    if on_its_way(pending, id).is_none() && current == Some(enabled) {
        return Handling::Answer(Outcome::Confirmed);
    }
    Handling::Wait(setting, ControlCommand::switch(id, enabled))
}

fn adaptive_level(
    state: &State,
    known: KnownSettings,
    features: Capabilities,
    level: AdaptiveLevel,
) -> Handling {
    if !features.adaptive || known.noise.then_some(state.noise_mode) != Some(NoiseMode::Adaptive) {
        return Handling::Answer(Outcome::Refused(CommandError::NotSupported));
    }
    if known.adaptive_level.then_some(state.adaptive_noise_level) == Some(level) {
        return Handling::Answer(Outcome::Confirmed);
    }
    Handling::Store(level)
}
