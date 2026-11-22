//! The one owner of the daemon state; every read and every change goes through it.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use pods_core::command::{Command, CommandError, Outcome};
use pods_core::state::State;
use tokio::sync::{mpsc, oneshot, watch};

use super::SUBSCRIBER_BACKLOG;
use super::link::Input;

/// Clones share one state; only the link driver changes it, through `publish`.
#[derive(Debug, Clone)]
pub(crate) struct Hub {
    state: watch::Sender<State>,
    /// Only taken inside the state's lock, so each subscriber gets every change in order.
    subscribers: Arc<Mutex<Vec<mpsc::Sender<State>>>>,
    link: mpsc::Sender<Input>,
}

impl Hub {
    pub(super) fn new(link: mpsc::Sender<Input>) -> Self {
        Self {
            state: watch::Sender::new(State::default()),
            subscribers: Arc::default(),
            link,
        }
    }

    pub(crate) fn state(&self) -> State {
        self.state.borrow().clone()
    }

    /// Coalesces changes the receiver has not seen yet.
    pub(crate) fn subscribe(&self) -> watch::Receiver<State> {
        self.state.subscribe()
    }

    /// None skipped or repeated; the receiver closes if it falls `SUBSCRIBER_BACKLOG` behind.
    pub(crate) fn follow(&self) -> (State, mpsc::Receiver<State>) {
        // The read lock is held until the sender is listed, so no change falls between the two.
        let current = self.state.borrow();
        let (sender, receiver) = mpsc::channel(SUBSCRIBER_BACKLOG);
        let mut subscribers = self.subscribers();
        // Gone subscribers leave here too, so the list stays live while the state stands still.
        subscribers.retain(|subscriber| !subscriber.is_closed());
        subscribers.push(sender);
        drop(subscribers);
        (current.clone(), receiver)
    }

    /// A full queue or a driver that is gone is `Timeout`.
    pub(crate) fn submit(&self, command: Command) -> impl Future<Output = Outcome> + use<> {
        let (reply, outcome) = oneshot::channel();
        // Queued before the first poll, so commands reach the driver in submission order.
        let queued = self
            .link
            .try_send(Input::Command { command, reply })
            .is_ok();
        async move {
            let timed_out = Outcome::Refused(CommandError::Timeout);
            if !queued {
                return timed_out;
            }
            outcome.await.unwrap_or(timed_out)
        }
    }

    /// Two calls at once would lose a change; the one driver is the only caller.
    pub(super) fn publish(&self, next: State) {
        self.state.send_if_modified(|state| {
            if *state == next {
                return false;
            }
            // Inside the write lock, so each subscriber gets the changes in the order they land.
            let mut subscribers = self.subscribers();
            // A full or gone subscriber is dropped: its connection closes, never skips a change.
            subscribers.retain(|subscriber| subscriber.try_send(next.clone()).is_ok());
            *state = next;
            true
        });
    }

    /// A panic while held left the list whole, since every change to it is one call.
    fn subscribers(&self) -> MutexGuard<'_, Vec<mpsc::Sender<State>>> {
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Replaces the state as a link would; no command can while there is none.
    #[cfg(test)]
    pub(crate) fn replace(&self, state: State) {
        self.state.send_replace(state);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Instant;

    use pods_core::command::CommandError;
    use pods_core::legacy::{LegacyRequest, parse_verb};
    use pods_core::state::EarDetectionBehavior;
    use tokio::sync::mpsc::error::TryRecvError;

    use super::*;
    use crate::daemon::link::tests::{HANG_LIMIT, Running, block_on};

    const FOLLOWS: usize = 2000;

    fn ear_change(turn: usize) -> Command {
        if turn.is_multiple_of(2) {
            Command::SetEarDetection(EarDetectionBehavior::PauseWhenBothRemoved)
        } else {
            Command::SetEarDetection(EarDetectionBehavior::PauseWhenOneRemoved)
        }
    }

    fn next_change(changes: &mut mpsc::Receiver<State>) -> State {
        let started = Instant::now();
        loop {
            match changes.try_recv() {
                Ok(change) => return change,
                Err(TryRecvError::Empty) if started.elapsed() < HANG_LIMIT => thread::yield_now(),
                Err(error) => panic!("no change after {:?}: {error}", started.elapsed()),
            }
        }
    }

    /// Raised on drop, so the changing thread stops even when the test panics.
    struct StopOnDrop<'flag>(&'flag AtomicBool);

    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    #[test]
    fn a_change_reaches_subscribers_and_the_state() {
        let link = Running::new();
        let hub = link.hub();
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
        let link = Running::new();
        let hub = link.hub();
        let updates = hub.subscribe();
        let Some(LegacyRequest::Command(noise_off)) = parse_verb(b"noise:off") else {
            panic!("noise:off is not a command");
        };
        let outcome = block_on(hub.submit(noise_off));
        assert_eq!(outcome, Outcome::Refused(CommandError::NotConnected));
        assert!(!updates.has_changed().unwrap());
        assert_eq!(hub.state(), State::default());
    }

    #[test]
    fn follow_gets_every_change_after_its_copy_exactly_once() {
        let link = Running::new();
        let hub = link.hub();
        let done = AtomicBool::new(false);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut turn = 0;
                while !done.load(Ordering::Relaxed) {
                    assert_eq!(block_on(hub.submit(ear_change(turn))), Outcome::Confirmed);
                    turn += 1;
                }
            });
            let _stop = StopOnDrop(&done);
            for _ in 0..FOLLOWS {
                let (copy, mut changes) = hub.follow();
                let first = next_change(&mut changes);
                let copied = copy.counters.ear_detection_changes;
                assert_eq!(first.counters.ear_detection_changes, copied + 1);
            }
        });
    }

    #[test]
    fn a_subscriber_that_falls_behind_is_closed_after_its_backlog() {
        let link = Running::new();
        let hub = link.hub();
        let (copy, mut changes) = hub.follow();
        let (_, departed) = hub.follow();
        drop(departed);
        block_on(hub.submit(ear_change(0)));
        assert_eq!(hub.subscribers().len(), 1, "the departed subscriber");
        let backlog = 64;
        for turn in 1..=backlog {
            block_on(hub.submit(ear_change(turn)));
        }
        let mut expected = copy.counters.ear_detection_changes;
        for _ in 0..backlog {
            expected += 1;
            let change = changes.try_recv().unwrap();
            assert_eq!(change.counters.ear_detection_changes, expected);
        }
        assert_eq!(changes.try_recv(), Err(TryRecvError::Disconnected));
        assert!(hub.subscribers().is_empty(), "the full subscriber");
    }

    #[test]
    fn subscribers_that_left_are_not_kept_while_the_state_stands_still() {
        let link = Running::new();
        let hub = link.hub();
        for _ in 0..FOLLOWS {
            drop(hub.follow());
        }
        let (_, _live) = hub.follow();
        assert_eq!(hub.subscribers().len(), 1);
    }
}
