//! The pods on real channels: one thread per channel, every thread sharing one model behind one lock.

use std::any::Any;
use std::collections::VecDeque;
use std::mem;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use pods_samples::{Cell, Placed};
use pods_sys::l2cap::{self, Channel, ChannelError, Received};
use rustix::io::Errno;

use crate::model::{self, Change, Model};
use crate::uplink::{Machine, Uplink};
use crate::{Deadline, Fault, Heard, Profile};

/// The longest wait of a channel thread, so what the test queues goes out within it.
const POLL: Duration = Duration::from_millis(20);
/// How long `finish` waits for every thread to end.
const FINISH_WITHIN: Duration = Duration::from_secs(2);
/// Larger than any control channel SDU; a longer one is heard cut to this length.
const RECV_BUFFER_LEN: usize = 65_536;
/// Twice the 2000 reports the largest planned case queues on one channel; more means the host stopped reading.
const OUTBOX_LIMIT: usize = 4_096;
/// Far above the few dozen frames a daemon sends in a test; more means the host floods the pods.
const HEARD_LIMIT: usize = 4_096;
const THREAD_NAME: &str = "fake-pods";

/// One pair of pods; every clone is the same pods.
#[derive(Debug, Clone)]
pub struct Pods {
    shared: Arc<Shared>,
}

#[derive(Debug)]
pub(crate) struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Debug)]
struct State {
    model: Model,
    uplink: Machine,
    fault: Fault,
    lines: Vec<Line>,
    next_line: usize,
    heard: Vec<Heard>,
    closed: Vec<Instant>,
    most_open: usize,
    threads: Vec<(usize, JoinHandle<()>)>,
    failures: Vec<String>,
    finished: bool,
}

/// An open channel as the threads that do not own it see it.
#[derive(Debug)]
struct Line {
    id: usize,
    fault: Fault,
    outbox: VecDeque<Vec<u8>>,
    hang_up: bool,
    deaf: bool,
}

/// A bound the host made the pods pass; the pods hang up and `finish` names it.
#[derive(Debug, Clone, Copy)]
enum Overflow {
    Outbox,
    Heard,
}

impl Overflow {
    fn message(self) -> String {
        match self {
            Self::Outbox => format!(
                "{THREAD_NAME}: a channel queued more than OUTBOX_LIMIT ({OUTBOX_LIMIT}) packets the host did not read"
            ),
            Self::Heard => {
                format!("{THREAD_NAME}: the host sent more than HEARD_LIMIT ({HEARD_LIMIT}) SDUs")
            }
        }
    }
}

impl Pods {
    #[must_use]
    pub fn new(profile: Profile) -> Self {
        let state = State {
            model: Model::new(profile),
            uplink: Machine::new(),
            fault: Fault::None,
            lines: Vec::new(),
            next_line: 0,
            heard: Vec::new(),
            closed: Vec::new(),
            most_open: 0,
            threads: Vec::new(),
            failures: Vec::new(),
            finished: false,
        };
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(state),
                changed: Condvar::new(),
            }),
        }
    }

    /// Sets the fault of channels opened after this call.
    pub fn fault(&self, fault: Fault) {
        self.shared.lock().fault = fault;
    }

    /// Answers on `channel` from one thread until the channel closes; after `finish` it closes `channel` at once.
    pub fn serve(&self, channel: Channel) {
        let mut state = self.shared.lock();
        if state.finished {
            return;
        }
        let id = state.next_line;
        state.next_line = id.wrapping_add(1);
        let fault = state.fault;
        let outbox = state.model.on_open(fault).into();
        state.lines.push(Line {
            id,
            fault,
            outbox,
            hang_up: false,
            deaf: false,
        });
        state.most_open = state.most_open.max(state.lines.len());
        let shared = Arc::clone(&self.shared);
        state.start(id, move || shared.run(id, fault, channel));
    }

    /// Moves the buds and reports it on every open channel.
    pub fn set_ear(&self, primary: Placed, secondary: Placed) {
        self.change(Change::Ear { primary, secondary });
    }

    pub fn set_battery(&self, left: Cell, right: Cell, case: Cell) {
        self.change(Change::Battery { left, right, case });
    }

    /// Changes a setting as the stem or another host would.
    pub fn set_setting(&self, identifier: u8, value: u8) {
        self.change(Change::Setting { identifier, value });
    }

    /// Sends raw bytes on every open channel.
    pub fn send(&self, sdu: &[u8]) {
        self.shared.lock().queue_everywhere(|_| Some(sdu.to_vec()));
    }

    /// Closes every open channel within one poll.
    pub fn hang_up(&self) {
        for line in &mut self.shared.lock().lines {
            line.hang_up = true;
        }
    }

    /// Returns once every channel `hang_up` marked has ended; one opened since is not waited for.
    pub fn wait_hung_up(&self, deadline: Duration) -> Result<(), Deadline> {
        self.shared.wait_until(deadline, |state| {
            state.lines.iter().all(|line| !line.hang_up).then_some(())
        })
    }

    /// Stops reading on every open channel, so the host's writes fill the socket until one fails.
    pub fn stop_reading(&self) {
        for line in &mut self.shared.lock().lines {
            line.deaf = true;
        }
    }

    /// The test's hand on the microphone uplink.
    #[must_use]
    pub fn uplink(&self) -> Uplink {
        Uplink::new(Arc::clone(&self.shared))
    }

    /// Everything the host sent, oldest first.
    #[must_use]
    pub fn heard(&self) -> Vec<Heard> {
        self.shared.lock().heard.clone()
    }

    /// Every SDU heard equal to `sdu`, once there are at least `count`.
    pub fn wait_heard(
        &self,
        sdu: &[u8],
        count: usize,
        deadline: Duration,
    ) -> Result<Vec<Heard>, Deadline> {
        self.shared.wait_until(deadline, |state| {
            let matching: Vec<Heard> = state
                .heard
                .iter()
                .filter(|heard| heard.sdu == sdu)
                .cloned()
                .collect();
            (matching.len() >= count).then_some(matching)
        })
    }

    /// When each channel ended, once at least `count` have.
    pub fn wait_closed(&self, count: usize, deadline: Duration) -> Result<Vec<Instant>, Deadline> {
        self.shared.wait_until(deadline, |state| {
            (state.closed.len() >= count).then(|| state.closed.clone())
        })
    }

    /// Returns once every open channel has sent all it queued.
    pub fn wait_sent(&self, deadline: Duration) -> Result<(), Deadline> {
        self.shared.wait_until(deadline, |state| {
            state
                .lines
                .iter()
                .all(|line| line.outbox.is_empty())
                .then_some(())
        })
    }

    #[must_use]
    pub fn most_open_at_once(&self) -> usize {
        self.shared.lock().most_open
    }

    /// Hangs up and joins every thread within 2 s; the error names each thread that panicked, failed or outlived that, and each bound passed.
    pub fn finish(self) -> Result<(), String> {
        self.shared.lock().finished = true;
        self.hang_up();
        let started = Instant::now();
        let mut state = self.shared.lock();
        while !state.threads.iter().all(|(_, handle)| handle.is_finished()) {
            let Some(left) = FINISH_WITHIN.checked_sub(started.elapsed()) else {
                break;
            };
            // A thread notifies as it ends but is finished only just after, so the wait is cut to one poll.
            state = self.shared.wait(state, left.min(POLL));
        }
        let threads = mem::take(&mut state.threads);
        let mut failures = mem::take(&mut state.failures);
        drop(state);
        for (id, handle) in threads {
            if !handle.is_finished() {
                failures.push(format!(
                    "{THREAD_NAME} thread {id} did not end within {FINISH_WITHIN:?}"
                ));
            } else if let Err(payload) = handle.join() {
                failures.push(format!(
                    "{THREAD_NAME} thread {id} panicked: {}",
                    panic_message(payload.as_ref())
                ));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    fn change(&self, change: Change) {
        let mut state = self.shared.lock();
        state.model.apply(change);
        state.queue_everywhere(|fault| change.report(fault));
    }
}

impl Line {
    fn queue(&mut self, sdus: impl IntoIterator<Item = Vec<u8>>) -> Result<(), Overflow> {
        for sdu in sdus {
            if self.outbox.len() >= OUTBOX_LIMIT {
                return Err(Overflow::Outbox);
            }
            self.outbox.push_back(sdu);
        }
        Ok(())
    }
}

impl State {
    /// Runs `body` on a thread `finish` joins; called under the lock so `finish` sees either no channel or its thread.
    fn start(&mut self, id: usize, body: impl FnOnce() + Send + 'static) {
        let spawned = thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn(body);
        match spawned {
            Ok(handle) => self.threads.push((id, handle)),
            Err(error) => {
                self.lines.retain(|line| line.id != id);
                self.failures.push(format!(
                    "could not start {THREAD_NAME} thread {id}: {error}"
                ));
            }
        }
    }

    /// Queues on every open channel what `packet` gives for the fault it was opened with.
    fn queue_everywhere(&mut self, packet: impl Fn(Fault) -> Option<Vec<u8>>) {
        let mut passed = false;
        for line in &mut self.lines {
            if line.queue(packet(line.fault)).is_err() {
                passed = true;
            }
        }
        if passed {
            self.overflow(Overflow::Outbox);
        }
    }

    /// Records the bound once and hangs up every channel, so no queue grows past it.
    fn overflow(&mut self, bound: Overflow) {
        let failure = bound.message();
        if !self.failures.contains(&failure) {
            self.failures.push(failure);
        }
        for line in &mut self.lines {
            line.hang_up = true;
        }
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `change` on the uplink under the lock every channel thread takes.
    pub(crate) fn uplink<T>(&self, change: impl FnOnce(&mut Machine) -> T) -> T {
        change(&mut self.lock().uplink)
    }

    fn wait<'a>(&self, guard: MutexGuard<'a, State>, timeout: Duration) -> MutexGuard<'a, State> {
        let (guard, _) = self
            .changed
            .wait_timeout(guard, timeout)
            .unwrap_or_else(PoisonError::into_inner);
        guard
    }

    fn wait_until<T>(
        &self,
        deadline: Duration,
        mut ready: impl FnMut(&State) -> Option<T>,
    ) -> Result<T, Deadline> {
        let started = Instant::now();
        let mut state = self.lock();
        loop {
            if let Some(found) = ready(&state) {
                return Ok(found);
            }
            let waited = started.elapsed();
            let Some(left) = deadline.checked_sub(waited) else {
                return Err(Deadline { waited });
            };
            // A drained outbox notifies nobody, so the wait is cut to one poll.
            state = self.wait(state, left.min(POLL));
        }
    }

    /// One channel's thread: serves it, closes it, then marks it closed whatever ended it.
    fn run(&self, id: usize, fault: Fault, channel: Channel) {
        let ended = self.serve_line(id, fault, &channel);
        // Stamped before the drop, so the host never sees the close before its stamp.
        let closed = Instant::now();
        drop(channel);
        let mut state = self.lock();
        state.lines.retain(|line| line.id != id);
        state.uplink.closed(id);
        state.closed.push(closed);
        if let Err(error) = ended {
            state
                .failures
                .push(format!("{THREAD_NAME} thread {id}: {error}"));
        }
        self.changed.notify_all();
    }

    /// Ends on `Closed`, on a hang up or where the fault closes; a receive or send error the host did not cause is returned.
    fn serve_line(&self, id: usize, fault: Fault, channel: &Channel) -> Result<(), ChannelError> {
        let mut buffer = vec![0; RECV_BUFFER_LEN];
        let mut sent: usize = 0;
        loop {
            if !self.send_queued(id, fault, channel, &mut sent)? {
                return Ok(());
            }
            if model::closes(fault, sent) {
                return Ok(());
            }
            let pause = self.pause(id);
            if self.deaf(id) {
                drop(self.wait(self.lock(), pause));
                continue;
            }
            if l2cap::wait_any(&[channel], pause)?.is_none() {
                continue;
            }
            let len = match channel.try_recv(&mut buffer) {
                Ok(Received::Sdu(len)) => len,
                Ok(Received::Truncated { .. }) => RECV_BUFFER_LEN,
                Ok(Received::TimedOut) => continue,
                Ok(Received::Closed) => return Ok(()),
                Err(error) if host_gone(error) => return Ok(()),
                Err(error) => return Err(error),
            };
            self.on_host(id, fault, buffer.get(..len).unwrap_or_default());
        }
    }

    /// Queues what the uplink has due on channel `id`, then sends what the channel has queued until the socket is full; `false` once the channel is to end.
    fn send_queued(
        &self,
        id: usize,
        fault: Fault,
        channel: &Channel,
        sent: &mut usize,
    ) -> Result<bool, ChannelError> {
        let mut guard = self.lock();
        let state = &mut *guard;
        let worn = state.model.either_bud_in_an_ear();
        let due = state.uplink.due(id, worn, Instant::now());
        let Some(line) = state.lines.iter_mut().find(|line| line.id == id) else {
            return Ok(false);
        };
        if line.hang_up {
            return Ok(false);
        }
        if let Err(bound) = line.queue(due) {
            state.overflow(bound);
            return Ok(false);
        }
        while let Some(sdu) = line.outbox.front() {
            if model::closes(fault, *sent) {
                return Ok(false);
            }
            match channel.try_send(sdu) {
                Ok(()) => {
                    line.outbox.pop_front();
                    *sent = sent.saturating_add(1);
                }
                Err(ChannelError::Send(Errno::AGAIN)) => break,
                Err(error) if host_gone(error) => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        Ok(true)
    }

    /// How long channel `id` may wait: one poll, or less when the uplink has a packet due on it sooner.
    fn pause(&self, id: usize) -> Duration {
        let due = self.lock().uplink.wait(id, Instant::now());
        due.map_or(POLL, |wait| wait.min(POLL))
    }

    fn deaf(&self, id: usize) -> bool {
        self.lock()
            .lines
            .iter()
            .any(|line| line.id == id && line.deaf)
    }

    fn on_host(&self, id: usize, fault: Fault, sdu: &[u8]) {
        let mut guard = self.lock();
        let state = &mut *guard;
        if state.heard.len() >= HEARD_LIMIT {
            state.overflow(Overflow::Heard);
            return;
        }
        let at = Instant::now();
        state.heard.push(Heard {
            at,
            sdu: sdu.to_vec(),
        });
        let replies = state.model.on_host(fault, sdu);
        if model::answers(fault) {
            state.uplink.on_host(id, sdu, at);
        }
        let queued = match state.lines.iter_mut().find(|line| line.id == id) {
            Some(line) => line.queue(replies),
            None => Ok(()),
        };
        if let Err(bound) = queued {
            state.overflow(bound);
        }
        self.changed.notify_all();
    }
}

/// The host closed its end: a reset when it left packets unread (`unix_release_sock` in `net/unix/af_unix.c`), a broken pipe on a later send.
fn host_gone(error: ChannelError) -> bool {
    matches!(
        error,
        ChannelError::Recv(Errno::CONNRESET) | ChannelError::Send(Errno::CONNRESET | Errno::PIPE)
    )
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<&str>() {
        message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message
    } else {
        "a payload that is not text"
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[test]
    fn a_thread_that_panicked_fails_the_finish() {
        let pods = Pods::new(Profile::default());
        let (host, end) = crate::pair();
        pods.serve(end);
        pods.shared.lock().start(7, || panic!("on purpose"));
        let failure = pods.finish().unwrap_err();
        assert_eq!(failure, "fake-pods thread 7 panicked: on purpose");
        drop(host);
    }

    #[test]
    fn a_host_that_sends_past_the_heard_limit_fails_the_finish() {
        const JUNK: [u8; 1] = [0xee];
        const DEADLINE: Duration = Duration::from_secs(5);
        let pods = Pods::new(Profile::default());
        let (host, end) = crate::pair();
        pods.serve(end);
        for _ in 0..HEARD_LIMIT {
            host.send(&JUNK).unwrap();
        }
        assert_eq!(
            pods.wait_heard(&JUNK, HEARD_LIMIT, DEADLINE).unwrap().len(),
            HEARD_LIMIT
        );
        assert_eq!(pods.shared.lock().failures, Vec::<String>::new());
        host.send(&JUNK).unwrap();
        pods.wait_closed(1, DEADLINE).unwrap();
        assert_eq!(pods.heard().len(), HEARD_LIMIT);
        assert_eq!(pods.finish().unwrap_err(), Overflow::Heard.message());
        drop(host);
    }

    /// Pods with one channel no thread drains, its outbox at `OUTBOX_LIMIT` and nothing failed yet.
    fn pods_with_a_full_outbox() -> Pods {
        let pods = Pods::new(Profile::default());
        let unread = Line {
            id: 0,
            fault: Fault::None,
            outbox: VecDeque::new(),
            hang_up: false,
            deaf: false,
        };
        pods.shared.lock().lines.push(unread);
        for _ in 0..OUTBOX_LIMIT {
            pods.send(&[0xee]);
        }
        assert_eq!(pods.shared.lock().failures, Vec::<String>::new());
        pods
    }

    fn assert_hung_up_on_the_outbox_limit(pods: Pods) {
        let state = pods.shared.lock();
        assert_eq!(state.lines[0].outbox.len(), OUTBOX_LIMIT);
        assert!(state.lines[0].hang_up);
        drop(state);
        assert_eq!(pods.finish().unwrap_err(), Overflow::Outbox.message());
    }

    #[test]
    fn wait_sent_returns_once_the_host_can_read_the_change() {
        const DEADLINE: Duration = Duration::from_secs(5);
        let pods = Pods::new(Profile::default());
        let (host, end) = crate::pair();
        pods.serve(end);
        pods.wait_sent(DEADLINE).unwrap();
        let mut buffer = vec![0; RECV_BUFFER_LEN];
        while matches!(host.try_recv(&mut buffer), Ok(Received::Sdu(_))) {}
        pods.set_ear(Placed::InEar, Placed::OutOfEar);
        pods.wait_sent(DEADLINE).unwrap();
        assert!(matches!(host.try_recv(&mut buffer), Ok(Received::Sdu(_))));
        pods.finish().unwrap();
        drop(host);
    }

    #[test]
    fn wait_sent_fails_at_the_deadline_while_a_packet_is_queued() {
        let pods = pods_with_a_full_outbox();
        let waited = pods.wait_sent(Duration::from_millis(50)).unwrap_err();
        assert!(waited.waited >= Duration::from_millis(50), "{waited:?}");
    }

    #[test]
    fn a_waiter_sees_an_outbox_drained_without_a_notify_within_a_poll() {
        const DEADLINE: Duration = Duration::from_secs(10);
        const WAKE_SLACK: Duration = Duration::from_secs(1);
        let pods = pods_with_a_full_outbox();
        let checked = AtomicBool::new(false);
        let started = Instant::now();
        thread::scope(|scope| {
            scope.spawn(|| {
                while !checked.load(Ordering::Acquire) {
                    thread::yield_now();
                }
                // The waiter holds the lock until it sleeps, so this drain lands while it waits.
                pods.shared.lock().lines[0].outbox.clear();
            });
            let seen = pods.shared.wait_until(DEADLINE, |state| {
                checked.store(true, Ordering::Release);
                state.lines[0].outbox.is_empty().then_some(())
            });
            assert_eq!(seen, Ok(()));
        });
        // Not after one poll plus the slack a loaded runner takes to wake a thread.
        assert!(
            started.elapsed() < POLL + WAKE_SLACK,
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_change_past_the_outbox_limit_fails_the_finish() {
        let pods = pods_with_a_full_outbox();
        pods.set_setting(pods_samples::NOISE, 2);
        assert_hung_up_on_the_outbox_limit(pods);
    }

    #[test]
    fn a_reply_past_the_outbox_limit_fails_the_finish() {
        let pods = pods_with_a_full_outbox();
        pods.shared
            .on_host(0, Fault::None, &pods_samples::host::handshake());
        assert_hung_up_on_the_outbox_limit(pods);
    }

    #[test]
    fn a_send_to_a_host_that_closed_ends_the_channel_quietly() {
        for unread in [false, true] {
            let pods = pods_with_a_full_outbox();
            let (host, end) = crate::pair();
            if unread {
                end.send(&[0x01]).unwrap();
            }
            drop(host);
            let mut sent = 0;
            assert_eq!(
                pods.shared.send_queued(0, Fault::None, &end, &mut sent),
                Ok(false),
                "unread: {unread}"
            );
        }
    }

    #[test]
    fn an_open_channel_is_not_counted_closed() {
        /// Five polls: a thread that ends by itself has ended by then.
        const OPEN_FOR: Duration = Duration::from_millis(100);
        let pods = Pods::new(Profile::default());
        let (host, end) = crate::pair();
        pods.serve(end);
        let waited = pods.wait_closed(1, OPEN_FOR);
        assert!(
            matches!(waited, Err(Deadline { waited }) if waited >= OPEN_FOR),
            "{waited:?}"
        );
        drop(host);
        pods.finish().unwrap();
    }
}
