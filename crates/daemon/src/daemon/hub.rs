//! The one owner of the daemon state: the socket and the state file read it here and ask it to change here.

use std::future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use pods_core::command::{Command, Outcome, apply};
use pods_core::state::State;
use tokio::sync::{mpsc, watch};

use super::socket::MAX_CONNECTIONS;

/// Changes a subscriber may leave untaken before the hub drops it; each client makes at most one change between two turns of a subscriber's task.
const SUBSCRIBER_BACKLOG: usize = MAX_CONNECTIONS;

/// A handle on the state; every clone shares it, and only `submit` changes it.
#[derive(Debug, Clone)]
pub(crate) struct Hub {
    state: watch::Sender<State>,
    /// One sender per subscriber, each getting every change in order; only taken inside the state's lock.
    subscribers: Arc<Mutex<Vec<mpsc::Sender<State>>>>,
}

impl Hub {
    /// A hub holding the state of a daemon with no pods connected.
    pub(crate) fn new() -> Self {
        Self {
            state: watch::Sender::new(State::default()),
            subscribers: Arc::default(),
        }
    }

    /// A copy of the current state.
    pub(crate) fn state(&self) -> State {
        self.state.borrow().clone()
    }

    /// A receiver that wakes on every change after this call, coalescing changes it has not seen yet.
    pub(crate) fn subscribe(&self) -> watch::Receiver<State> {
        self.state.subscribe()
    }

    /// The current state and a receiver of every change after it, none skipped or repeated; it closes if it falls `SUBSCRIBER_BACKLOG` behind.
    pub(crate) fn follow(&self) -> (State, mpsc::Receiver<State>) {
        // The read lock on the state is held until the sender is listed, so no change lands between the copy and the listing.
        let current = self.state.borrow();
        let (sender, receiver) = mpsc::channel(SUBSCRIBER_BACKLOG);
        let mut subscribers = self.subscribers();
        // Departed subscribers leave here too, so the list stays at the live ones while the state stands still.
        subscribers.retain(|subscriber| !subscriber.is_closed());
        subscribers.push(sender);
        drop(subscribers);
        (current.clone(), receiver)
    }

    /// Applies `command` and tells subscribers when the state changed; with no link the outcome is ready at once.
    pub(crate) fn submit(&self, command: Command) -> impl Future<Output = Outcome> + use<> {
        let mut next = self.state();
        let outcome = apply(&mut next, &command);
        self.state.send_if_modified(|state| {
            if *state == next {
                return false;
            }
            // Inside the write lock, so each subscriber gets the changes in the order they land.
            let mut subscribers = self.subscribers();
            // A subscriber that is full or gone is dropped, and its connection closes rather than skip a change.
            subscribers.retain(|subscriber| subscriber.try_send(next.clone()).is_ok());
            *state = next;
            true
        });
        future::ready(outcome)
    }

    /// The subscriber list; a panic while it was held left it whole, since every change to it is one call.
    fn subscribers(&self) -> MutexGuard<'_, Vec<mpsc::Sender<State>>> {
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Replaces the state as a link would, which no command can do while there is none.
    #[cfg(test)]
    pub(crate) fn replace(&self, state: State) {
        self.state.send_replace(state);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use pods_core::command::CommandError;
    use pods_core::legacy::{LegacyRequest, parse_verb};
    use pods_core::state::EarDetectionBehavior;
    use tokio::runtime::Builder;
    use tokio::sync::mpsc::error::TryRecvError;

    use super::*;

    /// Subscriptions the ordering test takes while another thread changes the state.
    const FOLLOWS: usize = 2000;

    /// The ear detection command that is a change from the one before it, alternating by `turn`.
    fn ear_change(turn: usize) -> Command {
        if turn.is_multiple_of(2) {
            Command::SetEarDetection(EarDetectionBehavior::PauseWhenBothRemoved)
        } else {
            Command::SetEarDetection(EarDetectionBehavior::PauseWhenOneRemoved)
        }
    }

    /// A change that has not arrived after this long never will, and the test fails instead of hanging.
    const HANG_LIMIT: Duration = Duration::from_secs(10);

    /// The next change, failing the test once `HANG_LIMIT` passes without one.
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

    /// Raises the flag when dropped, so the changing thread stops even when the test panics.
    struct StopOnDrop<'flag>(&'flag AtomicBool);

    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

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

    #[test]
    fn follow_gets_every_change_after_its_copy_exactly_once() {
        let hub = Hub::new();
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
        let hub = Hub::new();
        let (copy, mut changes) = hub.follow();
        let (_, departed) = hub.follow();
        drop(departed);
        block_on(hub.submit(ear_change(0)));
        assert_eq!(hub.subscribers().len(), 1, "the departed subscriber");
        for turn in 1..=SUBSCRIBER_BACKLOG {
            block_on(hub.submit(ear_change(turn)));
        }
        let mut expected = copy.counters.ear_detection_changes;
        for _ in 0..SUBSCRIBER_BACKLOG {
            expected += 1;
            let change = changes.try_recv().unwrap();
            assert_eq!(change.counters.ear_detection_changes, expected);
        }
        assert_eq!(changes.try_recv(), Err(TryRecvError::Disconnected));
        assert!(hub.subscribers().is_empty(), "the full subscriber");
    }

    #[test]
    fn subscribers_that_left_are_not_kept_while_the_state_stands_still() {
        let hub = Hub::new();
        for _ in 0..FOLLOWS {
            drop(hub.follow());
        }
        let (_, _live) = hub.follow();
        assert_eq!(hub.subscribers().len(), 1);
    }
}
