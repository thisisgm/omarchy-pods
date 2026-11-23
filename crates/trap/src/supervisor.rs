//! The trap's thread: takes each trapped call off the listener, answers it and records it.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::FileExt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use pods_proto::addr::BdAddr;
use pods_sys::l2cap;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::{Errno, ioctl_fionbio};
use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};

use crate::notify::{self, Call, Reply};
use crate::record::{Answer, Dial, Outcome, Route, Violation};
use crate::verdict::{self, Address, ConnectVerdict, SocketVerdict};

#[derive(Debug, Default)]
pub(crate) struct State {
    pub(crate) routes: Vec<Route>,
    pub(crate) dials: Vec<Dial>,
    pub(crate) sockets: usize,
    pub(crate) violations: Vec<Violation>,
    /// Set when the thread ended early, naming the system call that failed.
    pub(crate) failure: Option<String>,
}

impl State {
    fn answer_for(&self, device: BdAddr, psm: u16) -> Option<Answer> {
        let route = self
            .routes
            .iter()
            .find(|route| route.device == device && route.psm == psm);
        route.map(|route| route.answer.clone())
    }
}

/// The state the test and the thread share; `changed` wakes a test waiting for a dial.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    state: Mutex<State>,
    pub(crate) changed: Condvar,
}

impl Shared {
    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn update(&self, change: impl FnOnce(&mut State)) {
        change(&mut self.lock());
        self.changed.notify_all();
    }
}

enum Flow {
    Go,
    Stop,
}

struct Supervisor {
    listener: OwnedFd,
    stop: Arc<OwnedFd>,
    shared: Arc<Shared>,
    /// The test's end of every socket the child got, under the number the child got.
    owned: HashMap<u32, OwnedFd>,
}

/// Runs until `stop` is written or every task of the child is gone.
pub(crate) fn run(listener: OwnedFd, stop: Arc<OwnedFd>, shared: &Arc<Shared>) {
    let mut supervisor = Supervisor {
        listener,
        stop,
        shared: Arc::clone(shared),
        owned: HashMap::new(),
    };
    if let Err(failure) = supervisor.serve() {
        shared.update(|state| state.failure = Some(failure));
    }
}

fn failed(call: &str, errno: Errno) -> String {
    format!("{call} failed: {errno}")
}

// corner: one answer at a time; a delay holds the child's next trapped call
impl Supervisor {
    fn serve(&mut self) -> Result<(), String> {
        loop {
            let mut fds = [
                PollFd::new(&self.listener, PollFlags::IN),
                PollFd::new(&*self.stop, PollFlags::IN),
            ];
            match poll(&mut fds, None) {
                Ok(_) => {}
                Err(Errno::INTR) => continue,
                Err(errno) => return Err(failed("poll", errno)),
            }
            let [listener, stop] = fds.map(|fd| fd.revents());
            if !stop.is_empty() {
                return Ok(());
            }
            if !listener.contains(PollFlags::IN) {
                // A hang up: the filter has no task left.
                return Ok(());
            }
            match notify::receive(self.listener.as_fd()) {
                Ok(call) => {
                    if let Flow::Stop = self.handle(call)? {
                        return Ok(());
                    }
                }
                // The caller was killed between the poll and the receive.
                Err(Errno::NOENT | Errno::INTR) => {}
                Err(errno) => return Err(failed("SECCOMP_IOCTL_NOTIF_RECV", errno)),
            }
        }
    }

    fn handle(&mut self, call: Call) -> Result<Flow, String> {
        match call.number {
            libc::SYS_socket => self.socket(call).map(|()| Flow::Go),
            libc::SYS_connect => self.connect(call),
            _ => self.reply(call.id, Reply::Continue).map(|_| Flow::Go),
        }
    }

    /// False when the caller was killed before the answer reached it.
    fn reply(&self, id: u64, reply: Reply) -> Result<bool, String> {
        match notify::answer(self.listener.as_fd(), id, reply) {
            Ok(()) => Ok(true),
            Err(Errno::NOENT) => Ok(false),
            Err(errno) => Err(failed("SECCOMP_IOCTL_NOTIF_SEND", errno)),
        }
    }

    fn socket(&mut self, call: Call) -> Result<(), String> {
        self.shared
            .update(|state| state.sockets = state.sockets.saturating_add(1));
        match verdict::socket(&call.args) {
            SocketVerdict::Foreign { kind, protocol } => {
                let violation = Violation::ForeignSocket { kind, protocol };
                self.shared.update(|state| state.violations.push(violation));
                self.reply(call.id, Reply::Error(Errno::PROTONOSUPPORT))?;
            }
            SocketVerdict::Serve => {
                let (near, far) = socketpair(
                    AddressFamily::UNIX,
                    SocketType::SEQPACKET,
                    SocketFlags::CLOEXEC,
                    None,
                )
                .map_err(|errno| failed("socketpair", errno))?;
                // The child's copy shares this end's open file, so it carries the flag too.
                if verdict::nonblocking(&call.args) {
                    ioctl_fionbio(&near, true).map_err(|errno| failed("FIONBIO", errno))?;
                }
                match notify::answer_with_descriptor(self.listener.as_fd(), call.id, near.as_fd()) {
                    Ok(number) => {
                        self.owned.insert(number, far);
                    }
                    Err(Errno::NOENT) => {}
                    Err(errno) => return Err(failed("SECCOMP_IOCTL_NOTIF_ADDFD", errno)),
                }
            }
        }
        Ok(())
    }

    fn connect(&mut self, call: Call) -> Result<Flow, String> {
        let at = Instant::now();
        let [fd, address_at, ..] = call.args;
        let fd = verdict::low_half(fd);
        let bytes = match read_address(call.tid, address_at) {
            Ok(bytes) => bytes,
            // The caller was killed before the trap could read its address.
            Err(_) if !notify::still_valid(self.listener.as_fd(), call.id) => return Ok(Flow::Go),
            // `EIO` from /proc/<tid>/mem is an unmapped address, which the kernel refuses with `EFAULT` too.
            Err(error) if error.raw_os_error() == Some(libc::EIO) => {
                self.reply(call.id, Reply::Continue)?;
                return Ok(Flow::Go);
            }
            Err(error) => {
                return Err(format!(
                    "reading the connect address at {address_at:#x} in /proc/{}/mem failed: {error}",
                    call.tid
                ));
            }
        };
        if !notify::still_valid(self.listener.as_fd(), call.id) {
            return Ok(Flow::Go);
        }
        let address = verdict::address(&bytes);
        let route = match address {
            Address::L2cap { device, psm, .. } => self.shared.lock().answer_for(device, psm),
            Address::Other => None,
        };
        let (violation, errno) =
            match verdict::connect(&address, self.owned.contains_key(&fd), route.as_ref()) {
                ConnectVerdict::Continue => {
                    self.reply(call.id, Reply::Continue)?;
                    return Ok(Flow::Go);
                }
                ConnectVerdict::Routed {
                    device,
                    psm,
                    answer,
                } => {
                    let dial = Dial {
                        at,
                        device,
                        psm,
                        outcome: Outcome::Pending,
                    };
                    return self.dial(call.id, fd, dial, answer);
                }
                ConnectVerdict::Stranger => (Violation::Stranger { fd }, Errno::BADF),
                ConnectVerdict::BadAddress { cid, address_type } => {
                    (Violation::BadAddress { cid, address_type }, Errno::INVAL)
                }
                ConnectVerdict::Unrouted { device, psm } => {
                    let dial = Dial {
                        at,
                        device,
                        psm,
                        outcome: Outcome::Unrouted,
                    };
                    self.shared.update(|state| state.dials.push(dial));
                    (Violation::Unrouted { device, psm }, Errno::HOSTDOWN)
                }
            };
        self.shared.update(|state| state.violations.push(violation));
        self.reply(call.id, Reply::Error(errno))?;
        Ok(Flow::Go)
    }

    /// Records the dial as pending, waits its delay, answers it and sets its outcome.
    fn dial(&mut self, id: u64, fd: u32, dial: Dial, answer: Answer) -> Result<Flow, String> {
        let mut index = 0;
        self.shared.update(|state| {
            index = state.dials.len();
            state.dials.push(dial);
        });
        let stopped = match &answer {
            Answer::Accept { after, .. } | Answer::Refuse { after, .. } => {
                self.stopped_within(*after)?
            }
            Answer::Held { release, .. } => self.stopped_before(release)?,
        };
        if stopped {
            if !notify::still_valid(self.listener.as_fd(), id) {
                self.set_outcome(index, Outcome::Abandoned);
            }
            return Ok(Flow::Stop);
        }
        match answer {
            Answer::Refuse { errno, .. } => {
                let delivered = self.reply(id, Reply::Error(errno))?;
                let outcome = if delivered {
                    Outcome::Refused(errno)
                } else {
                    Outcome::Abandoned
                };
                self.set_outcome(index, outcome);
            }
            Answer::Accept { serve, .. } | Answer::Held { serve, .. } => {
                if !self.reply(id, Reply::Value(0))? {
                    self.set_outcome(index, Outcome::Abandoned);
                    return Ok(Flow::Go);
                }
                self.set_outcome(index, Outcome::Accepted);
                if let Some(far) = self.owned.remove(&fd) {
                    #[expect(
                        clippy::disallowed_methods,
                        reason = "the trap hands the test the far end of the socket the child connected"
                    )]
                    serve(l2cap::from_fd(far));
                }
            }
        }
        Ok(Flow::Go)
    }

    fn set_outcome(&self, index: usize, outcome: Outcome) {
        self.shared.update(|state| {
            if let Some(dial) = state.dials.get_mut(index) {
                dial.outcome = outcome;
            }
        });
    }

    /// Waits until `release` reads ready or the trap is stopped; true when stopped.
    fn stopped_before(&self, release: &OwnedFd) -> Result<bool, String> {
        loop {
            let mut fds = [
                PollFd::new(&*self.stop, PollFlags::IN),
                PollFd::new(release, PollFlags::IN),
            ];
            match poll(&mut fds, None) {
                Ok(_) => {}
                Err(Errno::INTR) => continue,
                Err(errno) => return Err(failed("poll", errno)),
            }
            let [stop, _] = fds.map(|fd| fd.revents());
            return Ok(!stop.is_empty());
        }
    }

    /// Waits `delay` on the stop descriptor; true when the trap is being stopped.
    fn stopped_within(&self, delay: Duration) -> Result<bool, String> {
        let deadline = Instant::now()
            .checked_add(delay)
            .ok_or_else(|| "the answer's delay passes the end of the clock".to_owned())?;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let timeout = Timespec::try_from(remaining)
                .ok()
                .ok_or_else(|| "the answer's delay passes the poll timeout".to_owned())?;
            match poll(
                &mut [PollFd::new(&*self.stop, PollFlags::IN)],
                Some(&timeout),
            ) {
                Ok(0) => return Ok(false),
                Ok(_) => return Ok(true),
                Err(Errno::INTR) => {}
                Err(errno) => return Err(failed("poll", errno)),
            }
        }
    }
}

/// Reads the caller's `struct sockaddr_l2` sized address out of its memory.
fn read_address(tid: u32, address_at: u64) -> io::Result<[u8; 14]> {
    let memory = File::open(format!("/proc/{tid}/mem"))?;
    let mut bytes = [0; 14];
    memory.read_exact_at(&mut bytes, address_at)?;
    Ok(bytes)
}
