//! Connect, disconnect and forget: the device each acts on, and the BlueZ call it asks for.

use pods_proto::addr::BdAddr;

use super::ladder::Attempt;
use super::{Action, DeviceAction, Machine, Phase, Ticket, raise, resolve};
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
        call(state, ticket, device, DeviceAction::Connect)
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
        call(state, ticket, device, DeviceAction::Disconnect)
    }

    /// The device's `DeviceGone` does the rest once BlueZ removed it.
    pub(super) fn forget_device(&mut self, state: &mut State, ticket: Ticket) -> Vec<Action> {
        let Some(device) = self.target() else {
            return no_device(ticket);
        };
        call(state, ticket, device, DeviceAction::Forget)
    }

    pub(super) fn call_failed(&mut self, state: &mut State, action: DeviceAction) -> Vec<Action> {
        if let Some(failures) = action.failures(&mut state.counters) {
            raise(failures);
        }
        if action == DeviceAction::Disconnect {
            self.user_disconnects = self.user_disconnects.saturating_sub(1);
        }
        Vec::new()
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

fn call(state: &mut State, ticket: Ticket, device: BdAddr, action: DeviceAction) -> Vec<Action> {
    raise(action.calls(&mut state.counters));
    vec![
        Action::Call { device, action },
        resolve(ticket, Outcome::Sent),
    ]
}

fn no_device(ticket: Ticket) -> Vec<Action> {
    vec![resolve(ticket, Outcome::Refused(CommandError::NoDevice))]
}
