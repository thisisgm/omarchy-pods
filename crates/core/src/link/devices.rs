//! Connect, disconnect and forget: the device each acts on, and the BlueZ call it asks for.

use pods_proto::addr::BdAddr;

use super::ladder::Attempt;
use super::{Action, Call, DeviceAction, Machine, Phase, Session, Signal, Ticket, raise, resolve};
use crate::command::{CommandError, Outcome};
use crate::state::{Counters, State};

impl DeviceAction {
    fn calls(self, counters: &mut Counters) -> &mut u64 {
        match self {
            Self::Connect => &mut counters.connect_calls,
            Self::Disconnect => &mut counters.disconnect_calls,
            Self::Forget => &mut counters.forget_calls,
        }
    }

    /// The counter a failed call raises; a failed forget has none.
    fn failures(self, counters: &mut Counters) -> Option<&mut u64> {
        match self {
            Self::Connect => Some(&mut counters.connect_failures),
            Self::Disconnect => Some(&mut counters.disconnect_failures),
            Self::Forget => None,
        }
    }
}

impl Machine {
    #[must_use]
    pub fn active_device(&self) -> Option<BdAddr> {
        self.active
    }

    /// Signals `Lost` when a device had the session; the caller forgets a removed device after this.
    pub(super) fn reset(&mut self, state: &mut State) -> Option<Action> {
        let lost = self.active.map(|device| {
            Action::Signal(Signal::Lost {
                device,
                down_at: self.populated.then_some(self.down_at),
                worn_on_link: self.worn_on_link,
            })
        });
        state.reset_device();
        self.active = None;
        self.held = false;
        self.populated = false;
        self.worn_on_link = false;
        self.spend_disconnects();
        lost
    }

    /// Connects only if the tracked device supplied the keys that resolved the advert.
    #[must_use]
    pub fn connect_when_worn(&self, state: &mut State, device: BdAddr) -> Option<(BdAddr, Call)> {
        if self.held || matches!(self.phase, Phase::Up { .. } | Phase::Stopped) {
            return None;
        }
        if self.target() != Some(device) {
            return None;
        }
        Some((device, self.device_call(state, DeviceAction::Connect)))
    }

    pub(super) fn connect(&mut self, state: &mut State, ticket: Ticket) -> Vec<Action> {
        let Some(device) = self.target() else {
            return no_device(ticket);
        };
        if matches!(self.phase, Phase::Up { .. }) {
            return vec![resolve(ticket, Outcome::Confirmed)];
        }
        if let Phase::Backoff { .. } | Phase::Parked { .. } = self.phase {
            let mut actions = self.open(state, device, Attempt::Held(1));
            actions.push(resolve(ticket, Outcome::Sent));
            return actions;
        }
        self.call(state, ticket, device, DeviceAction::Connect)
    }

    pub(super) fn disconnect(&mut self, state: &mut State, ticket: Ticket) -> Vec<Action> {
        let Some(device) = self.target() else {
            return no_device(ticket);
        };
        if !(self.is_active(device) && self.held) {
            return vec![resolve(
                ticket,
                Outcome::Refused(CommandError::NotConnected),
            )];
        }
        self.user_disconnects = self.user_disconnects.saturating_add(1);
        self.call(state, ticket, device, DeviceAction::Disconnect)
    }

    /// The device's `DeviceGone` does the rest once BlueZ removed it.
    pub(super) fn forget_device(&mut self, state: &mut State, ticket: Ticket) -> Vec<Action> {
        let Some(device) = self.target() else {
            return no_device(ticket);
        };
        self.call(state, ticket, device, DeviceAction::Forget)
    }

    pub(super) fn call_failed(&mut self, state: &mut State, call: Call) -> Vec<Action> {
        if let Some(failures) = call.action.failures(&mut state.counters) {
            raise(failures);
        }
        if call.action == DeviceAction::Disconnect && call.session == self.session {
            self.user_disconnects = self.user_disconnects.saturating_sub(1);
        }
        Vec::new()
    }

    /// BlueZ says the active device is connected; if it had stopped holding it (the device left, or BlueZ did), the disconnects called before are spent.
    pub(super) fn held_again(&mut self) {
        if !self.held {
            self.spend_disconnects();
        }
        self.held = true;
    }

    /// No user disconnect counts any more, and a late failure of one releases nothing.
    pub(super) fn spend_disconnects(&mut self) {
        self.session = Session(self.session.0.wrapping_add(1));
        self.user_disconnects = 0;
    }

    fn call(
        &self,
        state: &mut State,
        ticket: Ticket,
        device: BdAddr,
        action: DeviceAction,
    ) -> Vec<Action> {
        let call = self.device_call(state, action);
        vec![
            Action::Call { device, call },
            resolve(ticket, Outcome::Sent),
        ]
    }

    fn device_call(&self, state: &mut State, action: DeviceAction) -> Call {
        raise(action.calls(&mut state.counters));
        Call {
            action,
            session: self.session,
        }
    }

    /// The active device, else the last one linked, else the only one known.
    fn target(&self) -> Option<BdAddr> {
        let only = match self.known.as_slice() {
            [only] => Some(*only),
            _ => None,
        };
        self.active.or(self.last_linked).or(only)
    }
}

fn no_device(ticket: Ticket) -> Vec<Action> {
    vec![resolve(ticket, Outcome::Refused(CommandError::NoDevice))]
}
