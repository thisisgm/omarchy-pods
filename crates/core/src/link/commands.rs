//! Setting commands while the link is up: each is written, answered and stored at the write, and held against late reports.

use std::time::Duration;

use pods_proto::control::{ControlCommand, ControlId};
use pods_proto::model::Capabilities;
use pods_proto::noise::NoiseMode;

use super::{Action, Event, Machine, Ticket, after, raise, resolve};
use crate::command::{self, AdaptiveLevel, Command, CommandError, NoiseTarget, Outcome};
use crate::state::{Counters, State};

/// Covers the measured lag of the pods' reports behind queued noise writes, about one to two seconds each.
pub const REPORT_HOLD_MS: u64 = 3000;

/// A setting value a command writes to the pods, or a valid report of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Setting {
    Noise(NoiseMode),
    ConversationalAwareness(bool),
    OneBudAnc(bool),
    AdaptiveLevel(AdaptiveLevel),
}

impl Setting {
    /// Reads `04 00 04 00 09 00 28 01 00 00 00` as conversational awareness on; `None` for another identifier or a value out of range.
    pub(super) fn read(report: ControlCommand) -> Option<Self> {
        match report.id {
            ControlId::NOISE_CONTROL => NoiseMode::from_command(&report).ok().map(Self::Noise),
            ControlId::CONVERSATIONAL_AWARENESS => {
                report.state().map(Self::ConversationalAwareness)
            }
            ControlId::ONE_BUD_ANC => report.state().map(Self::OneBudAnc),
            ControlId::ADAPTIVE_NOISE_LEVEL => {
                let [percent, ..] = report.value;
                AdaptiveLevel::new(percent).ok().map(Self::AdaptiveLevel)
            }
            _ => None,
        }
    }

    fn frame(self) -> ControlCommand {
        match self {
            Self::Noise(mode) => mode.command(),
            Self::ConversationalAwareness(enabled) => {
                ControlCommand::switch(ControlId::CONVERSATIONAL_AWARENESS, enabled)
            }
            Self::OneBudAnc(enabled) => ControlCommand::switch(ControlId::ONE_BUD_ANC, enabled),
            Self::AdaptiveLevel(level) => ControlCommand {
                id: ControlId::ADAPTIVE_NOISE_LEVEL,
                value: [level.percent(), 0, 0, 0],
            },
        }
    }

    fn show(self, state: &mut State) {
        match self {
            Self::Noise(mode) => state.noise_mode = mode,
            Self::ConversationalAwareness(enabled) => state.conversational_awareness = enabled,
            Self::OneBudAnc(enabled) => state.one_bud_anc_mode = enabled,
            Self::AdaptiveLevel(level) => state.adaptive_noise_level = level,
        }
    }

    fn count_write(self, counters: &mut Counters) {
        let counter = match self {
            Self::Noise(_) => &mut counters.noise_control_changes,
            Self::ConversationalAwareness(_) => &mut counters.conversational_awareness_changes,
            Self::OneBudAnc(_) => &mut counters.one_bud_anc_changes,
            Self::AdaptiveLevel(_) => &mut counters.adaptive_level_changes,
        };
        raise(counter);
    }

    fn id(self) -> ControlId {
        self.frame().id
    }
}

/// A setting written in the last step, answered by the next event: refused on `SendFailed`, else stored and sent.
#[derive(Debug, Clone, Copy)]
pub(super) struct Written {
    ticket: Ticket,
    setting: Setting,
    /// When it was written; the deadline that makes the driver step again at once.
    at: Duration,
}

/// The value last written for one setting; until `until`, a report of another value is an earlier write's and is not taken.
#[derive(Debug, Clone, Copy)]
struct Hold {
    value: Setting,
    until: Duration,
    /// The last report of another value, taken when the hold passes since the pods report only on change.
    withheld: Option<Setting>,
}

/// The settings a write holds against late reports, one hold slot each.
const HELD: [ControlId; 4] = [
    ControlId::NOISE_CONTROL,
    ControlId::CONVERSATIONAL_AWARENESS,
    ControlId::ONE_BUD_ANC,
    ControlId::ADAPTIVE_NOISE_LEVEL,
];

/// What the current channel holds of the settings, cleared when a channel opens.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ChannelSettings {
    /// The noise mode was reported or written on this channel, which an adaptive level needs.
    pub(super) noise_known: bool,
    /// One per entry of `HELD`, in its order.
    holds: [Option<Hold>; HELD.len()],
}

impl ChannelSettings {
    fn slot(&mut self, id: ControlId) -> Option<&mut Option<Hold>> {
        let index = HELD.iter().position(|held| *held == id)?;
        self.holds.get_mut(index)
    }

    fn start(&mut self, setting: Setting, until: Duration) {
        if let Some(slot) = self.slot(setting.id()) {
            // A later write keeps what an earlier hold held back, since the pods report only on change.
            let withheld = slot.and_then(|hold| hold.withheld);
            *slot = Some(Hold {
                value: setting,
                until,
                withheld,
            });
        }
    }

    /// Whether `report` is taken: its setting has no hold, it carries the written value, or the hold has passed; a taken report ends the hold.
    pub(super) fn admit(&mut self, report: Setting, now: Duration) -> bool {
        let Some(slot) = self.slot(report.id()) else {
            return true;
        };
        if let Some(hold) = slot
            && now < hold.until
            && report != hold.value
        {
            hold.withheld = Some(report);
            return false;
        }
        *slot = None;
        true
    }

    /// Ends every hold that has passed, returning the reports it held back.
    pub(super) fn release(&mut self, now: Duration) -> Vec<Setting> {
        let mut released = Vec::new();
        for slot in &mut self.holds {
            if let Some(hold) = *slot
                && hold.until <= now
            {
                released.extend(hold.withheld);
                *slot = None;
            }
        }
        released
    }

    /// When the earliest hold that holds back a report passes.
    pub(super) fn release_deadline(&self) -> Option<Duration> {
        self.holds
            .into_iter()
            .flatten()
            .filter(|hold| hold.withheld.is_some())
            .map(|hold| hold.until)
            .min()
    }

    /// Shows a reported or written setting; a noise mode also makes the mode known on this channel.
    pub(super) fn show(&mut self, state: &mut State, setting: Setting) {
        setting.show(state);
        self.noise_known |= matches!(setting, Setting::Noise(_));
    }
}

/// What a command takes while the link is up.
#[derive(Debug, Clone, Copy)]
enum Handling {
    Refuse(CommandError),
    /// Not a setting of the pods: answered as it is with no link.
    AsWithoutLink,
    Write(Setting),
}

impl Machine {
    /// A command in `Up`: refused, answered as without a link, or written and answered in the next step.
    pub(super) fn command(
        &mut self,
        state: &mut State,
        ticket: Ticket,
        command: Command,
        now: Duration,
    ) -> Vec<Action> {
        match handling(state, self.channel_settings.noise_known, command) {
            Handling::Refuse(error) => vec![resolve(ticket, Outcome::Refused(error))],
            Handling::AsWithoutLink => vec![resolve(ticket, command::apply(state, &command))],
            Handling::Write(setting) => {
                self.written = Some(Written {
                    ticket,
                    setting,
                    at: now,
                });
                vec![Action::Send(setting.frame().encode())]
            }
        }
    }

    /// Answers the setting written in the step before `event`: refused when its write failed, else stored, held and sent.
    pub(super) fn answer_written(&mut self, state: &mut State, event: Event<'_>) -> Vec<Action> {
        let Some(written) = self.written.take() else {
            return Vec::new();
        };
        if event == Event::SendFailed {
            let refused = Outcome::Refused(CommandError::NotConnected);
            return vec![resolve(written.ticket, refused)];
        }
        written.setting.count_write(&mut state.counters);
        let settings = &mut self.channel_settings;
        settings.show(state, written.setting);
        settings.start(written.setting, after(written.at, REPORT_HOLD_MS));
        vec![resolve(written.ticket, Outcome::Sent)]
    }

    pub(super) fn written_deadline(&self) -> Option<Duration> {
        self.written.map(|written| written.at)
    }
}

/// What `command` takes in `Up`, read from the state, whether this channel reported or wrote the noise mode, and the model's features.
fn handling(state: &State, noise_known: bool, command: Command) -> Handling {
    let features = state.capabilities();
    match command {
        Command::SetNoise(target) => noise(state, features, target),
        Command::SetConversationalAwareness(enabled) => switch(
            features.conversational_awareness,
            Setting::ConversationalAwareness(enabled),
        ),
        Command::SetOneBudAnc(enabled) => switch(features.one_bud_anc, Setting::OneBudAnc(enabled)),
        Command::SetAdaptiveLevel(level) => {
            let adaptive = noise_known && state.noise_mode == NoiseMode::Adaptive;
            if features.adaptive && adaptive {
                Handling::Write(Setting::AdaptiveLevel(level))
            } else {
                Handling::Refuse(CommandError::NotSupported)
            }
        }
        // Never reached for connect, disconnect and forget, which `step` answers in every phase.
        Command::SetEarDetection(_)
        | Command::SetNotifications(_)
        | Command::Reopen
        | Command::Connect
        | Command::Disconnect
        | Command::Forget => Handling::AsWithoutLink,
    }
}

fn noise(state: &State, features: Capabilities, target: NoiseTarget) -> Handling {
    if !features.noise_control {
        return Handling::Refuse(CommandError::NotSupported);
    }
    let mode = match target {
        NoiseTarget::Mode(mode) => mode,
        NoiseTarget::Cycle => next_offered(state.noise_mode, features),
    };
    if !offers(features, mode) {
        return Handling::Refuse(CommandError::NotSupported);
    }
    // The pods answer no noise write while no bud is in an ear, so none is sent.
    if !state.ear.any_in_ear() && !state.model.capabilities().is_headset {
        return Handling::Refuse(CommandError::NotWorn);
    }
    Handling::Write(Setting::Noise(mode))
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

fn switch(supported: bool, setting: Setting) -> Handling {
    if supported {
        Handling::Write(setting)
    } else {
        Handling::Refuse(CommandError::NotSupported)
    }
}
