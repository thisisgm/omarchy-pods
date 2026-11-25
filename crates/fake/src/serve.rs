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

use crate::doap::{self, Doap};
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
    doap: doap::Machine,
    fault: Fault,
    lines: Vec<Line>,
    next_line: usize,
    heard: Vec<Heard>,
    /// The packets heard on attribute channels, which are counted and not kept.
    att_heard: usize,
    closed: Vec<Instant>,
    most_open: usize,
    threads: Vec<(usize, JoinHandle<()>)>,
    failures: Vec<String>,
    finished: bool,
}

/// What a channel carries: the control channel with the fault it was opened with, or the attribute channel of the dictation service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Control(Fault),
    Att,
}

impl Side {
    /// The fault of a control channel; an attribute channel has none.
    fn fault(self) -> Fault {
        match self {
            Self::Control(fault) => fault,
            Self::Att => Fault::None,
        }
    }
}

/// An open channel as the threads that do not own it see it.
#[derive(Debug)]
struct Line {
    id: usize,
    side: Side,
    outbox: VecDeque<Vec<u8>>,
    hang_up: bool,
    deaf: bool,
}

/// A bound the host made the pods pass; the pods hang up and `finish` names it.
#[derive(Debug, Clone, Copy)]
enum Overflow {
    Outbox,
    Heard,
    AttHeard,
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
            Self::AttHeard => format!(
                "{THREAD_NAME}: the host sent more than HEARD_LIMIT ({HEARD_LIMIT}) packets on its attribute channels"
            ),
        }
    }
}

impl Pods {
    #[must_use]
    pub fn new(profile: Profile) -> Self {
        let state = State {
            model: Model::new(profile),
            uplink: Machine::new(),
            doap: doap::Machine::new(),
            fault: Fault::None,
            lines: Vec::new(),
            next_line: 0,
            heard: Vec::new(),
            att_heard: 0,
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

    /// Answers the control channel on `channel` from one thread until the channel closes; after `finish` it closes `channel` at once.
    pub fn serve(&self, channel: Channel) {
        let side = Side::Control(self.shared.lock().fault);
        self.open(side, channel);
    }

    /// Answers the attribute channel of the dictation service on `channel`, as `serve` answers the control channel.
    pub fn serve_att(&self, channel: Channel) {
        self.open(Side::Att, channel);
    }

    fn open(&self, side: Side, channel: Channel) {
        let mut state = self.shared.lock();
        if state.finished {
            return;
        }
        let id = state.next_line;
        state.next_line = id.wrapping_add(1);
        let outbox = match side {
            Side::Control(fault) => state.model.on_open(fault).into(),
            Side::Att => {
                state.doap.opened(id);
                VecDeque::new()
            }
        };
        state.lines.push(Line {
            id,
            side,
            outbox,
            hang_up: false,
            deaf: false,
        });
        state.most_open = state.most_open.max(state.control_lines());
        let shared = Arc::clone(&self.shared);
        state.start(id, move || shared.run(id, side, channel));
    }

    /// Moves the buds and reports it on every open control channel.
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

    /// Reports the given Conversation Awareness level on every open control channel.
    pub fn speak(&self, level: u8) {
        self.change(Change::Awareness { level });
    }

    /// Sends raw bytes on every open control channel.
    pub fn send(&self, sdu: &[u8]) {
        self.shared.lock().queue_everywhere(|_| Some(sdu.to_vec()));
    }

    /// Closes every open channel within one poll.
    pub fn hang_up(&self) {
        for line in &mut self.shared.lock().lines {
            line.hang_up = true;
        }
    }

    /// Closes the attribute channels while the control link stays up.
    pub fn hang_up_att(&self) {
        for line in &mut self.shared.lock().lines {
            if line.side == Side::Att {
                line.hang_up = true;
            }
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

    /// The test's hand on the dictation service.
    #[must_use]
    pub fn doap(&self) -> Doap {
        Doap::new(Arc::clone(&self.shared))
    }

    /// Everything the host sent on a control channel, oldest first.
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

    /// When each control channel ended, once at least `count` have.
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

    /// The most control channels that were open at the same time.
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

    fn control_lines(&self) -> usize {
        let control = |line: &&Line| matches!(line.side, Side::Control(_));
        self.lines.iter().filter(control).count()
    }

    /// Queues on every open control channel what `packet` gives for the fault it was opened with.
    fn queue_everywhere(&mut self, packet: impl Fn(Fault) -> Option<Vec<u8>>) {
        let mut passed = false;
        for line in &mut self.lines {
            let Side::Control(fault) = line.side else {
                continue;
            };
            if line.queue(packet(fault)).is_err() {
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

    /// Runs `change` on the dictation service under the same lock.
    pub(crate) fn doap<T>(&self, change: impl FnOnce(&mut doap::Machine) -> T) -> T {
        change(&mut self.lock().doap)
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
    fn run(&self, id: usize, side: Side, channel: Channel) {
        let ended = self.serve_line(id, side, &channel);
        // Stamped before the drop, so the host never sees the close before its stamp.
        let closed = Instant::now();
        drop(channel);
        let mut state = self.lock();
        state.lines.retain(|line| line.id != id);
        state.uplink.closed(id);
        state.doap.closed(id);
        if side != Side::Att {
            state.closed.push(closed);
        }
        if let Err(error) = ended {
            state
                .failures
                .push(format!("{THREAD_NAME} thread {id}: {error}"));
        }
        self.changed.notify_all();
    }

    /// Ends on `Closed`, on a hang up or where the fault closes; a receive or send error the host did not cause is returned.
    fn serve_line(&self, id: usize, side: Side, channel: &Channel) -> Result<(), ChannelError> {
        let mut buffer = vec![0; RECV_BUFFER_LEN];
        let mut sent: usize = 0;
        loop {
            if !self.send_queued(id, side, channel, &mut sent)? {
                return Ok(());
            }
            if model::closes(side.fault(), sent) {
                return Ok(());
            }
            let pause = self.pause(id, side);
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
            self.on_host(id, side, buffer.get(..len).unwrap_or_default());
        }
    }

    /// Queues what the uplink or the dictation service has due on channel `id`, then sends what the channel has queued until the socket is full; `false` once the channel is to end.
    fn send_queued(
        &self,
        id: usize,
        side: Side,
        channel: &Channel,
        sent: &mut usize,
    ) -> Result<bool, ChannelError> {
        let mut guard = self.lock();
        let state = &mut *guard;
        let worn = state.model.either_bud_in_an_ear();
        let due = match side {
            Side::Control(_) => state.uplink.due(id, worn, Instant::now()),
            Side::Att => state.doap.due(id, Instant::now()),
        };
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
            if model::closes(side.fault(), *sent) {
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

    /// How long channel `id` may wait: one poll, or less when the uplink or the dictation service has a packet due on it sooner.
    fn pause(&self, id: usize, side: Side) -> Duration {
        let state = self.lock();
        let due = match side {
            Side::Control(_) => state.uplink.wait(id, Instant::now()),
            Side::Att => state.doap.wait(id, Instant::now()),
        };
        due.map_or(POLL, |wait| wait.min(POLL))
    }

    fn deaf(&self, id: usize) -> bool {
        self.lock()
            .lines
            .iter()
            .any(|line| line.id == id && line.deaf)
    }

    fn on_host(&self, id: usize, side: Side, sdu: &[u8]) {
        let mut guard = self.lock();
        let state = &mut *guard;
        let at = Instant::now();
        let replies = match side {
            Side::Control(fault) => {
                if state.heard.len() >= HEARD_LIMIT {
                    state.overflow(Overflow::Heard);
                    return;
                }
                state.heard.push(Heard {
                    at,
                    sdu: sdu.to_vec(),
                });
                if model::answers(fault) {
                    state.uplink.on_host(id, sdu, at);
                    state.doap.on_control(id, sdu, at);
                }
                state.model.on_host(fault, sdu)
            }
            Side::Att => {
                if state.att_heard >= HEARD_LIMIT {
                    state.overflow(Overflow::AttHeard);
                    return;
                }
                state.att_heard = state.att_heard.saturating_add(1);
                state.doap.on_att(id, sdu, at)
            }
        };
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
mod tests;
