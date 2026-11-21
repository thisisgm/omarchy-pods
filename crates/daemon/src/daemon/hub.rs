//! The one owner of the daemon state: the socket and the state file read it here and ask it to change here.

use std::future;

use pods_core::command::{Command, Outcome, apply};
use pods_core::state::State;
use tokio::sync::watch;

/// A handle on the state; every clone shares it, and only `submit` changes it.
#[derive(Debug, Clone)]
pub(crate) struct Hub {
    state: watch::Sender<State>,
}

impl Hub {
    /// A hub holding the state of a daemon with no pods connected.
    pub(crate) fn new() -> Self {
        Self {
            state: watch::Sender::new(State::default()),
        }
    }

    /// A copy of the current state.
    pub(crate) fn state(&self) -> State {
        self.state.borrow().clone()
    }

    /// A receiver that wakes on every change after this call.
    pub(crate) fn subscribe(&self) -> watch::Receiver<State> {
        self.state.subscribe()
    }

    /// Applies `command` and tells subscribers when the state changed; with no link the outcome is ready at once.
    pub(crate) fn submit(&self, command: Command) -> impl Future<Output = Outcome> + use<> {
        let mut next = self.state();
        let outcome = apply(&mut next, &command);
        self.state.send_if_modified(|state| {
            if *state == next {
                return false;
            }
            *state = next;
            true
        });
        future::ready(outcome)
    }
}

#[cfg(test)]
mod tests {
    use pods_core::command::CommandError;
    use pods_core::legacy::{LegacyRequest, parse_verb};
    use pods_core::state::EarDetectionBehavior;
    use tokio::runtime::Builder;

    use super::*;

    fn block_on<F: Future>(future: F) -> F::Output {
        Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn a_change_reaches_subscribers_and_the_state() {
        let hub = Hub::new();
        let mut updates = hub.subscribe();
        let ear_off = Command::SetEarDetection(EarDetectionBehavior::Disabled);
        assert_eq!(block_on(hub.submit(ear_off)), Outcome::Confirmed);
        assert!(updates.has_changed().unwrap());
        let seen = updates.borrow_and_update().ear_detection_behavior;
        assert_eq!(seen, EarDetectionBehavior::Disabled);
        assert_eq!(hub.state().ear_detection_behavior, seen);
    }

    #[test]
    fn a_command_that_changes_nothing_wakes_nobody() {
        let hub = Hub::new();
        let updates = hub.subscribe();
        let Some(LegacyRequest::Command(noise_off)) = parse_verb(b"noise:off") else {
            panic!("noise:off is not a command");
        };
        let outcome = block_on(hub.submit(noise_off));
        assert_eq!(outcome, Outcome::Refused(CommandError::NotConnected));
        assert!(!updates.has_changed().unwrap());
        assert_eq!(hub.state(), State::default());
    }
}
