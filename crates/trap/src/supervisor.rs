//! The trap's thread: takes each trapped call off the listener, answers it and records it.

use std::collections::HashMap;
use std::fs::{File, metadata};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{FileExt, MetadataExt};
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

/// From include/net/bluetooth/l2cap.h, for the classic channel the trap serves.
const L2CAP_OPTIONS: u32 = 0x01;
const L2CAP_OPTIONS_LEN: usize = 12;
const DEFAULT_MTU: u16 = 672;
const DEFAULT_FLUSH_TIMEOUT: u16 = 0xffff;
const BASIC_MODE: u8 = 0;
const CRC16_FCS: u8 = 1;
const DEFAULT_MAX_TX: u8 = 3;
const DEFAULT_TX_WINDOW: u16 = 63;
const OPTIONS_PADDING: u8 = 0;

#[derive(Debug, Default)]
pub(crate) struct State {
    pub(crate) routes: Vec<Route>,
    pub(crate) dials: Vec<Dial>,
    pub(crate) sockets: usize,
    pub(crate) refuse_l2cap_option_writes: bool,
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
    owned: HashMap<u32, TrappedSocket>,
}

struct TrappedSocket {
    /// The identity survives handing off the far end and distinguishes a reused descriptor number.
    near_identity: (u64, u64),
    far: Option<OwnedFd>,
    imtu: Option<u16>,
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
            libc::SYS_getsockopt => self.socket_option(call, "getsockopt"),
            libc::SYS_setsockopt => self.socket_option(call, "setsockopt"),
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
                let near_identity = socket_identity("self", near.as_raw_fd())
                    .map_err(|error| format!("reading the socket pair identity failed: {error}"))?;
                match notify::answer_with_descriptor(self.listener.as_fd(), call.id, near.as_fd()) {
                    Ok(number) => {
                        self.owned.insert(
                            number,
                            TrappedSocket {
                                near_identity,
                                far: Some(far),
                                imtu: None,
                            },
                        );
                    }
                    Err(Errno::NOENT) => {}
                    Err(errno) => return Err(failed("SECCOMP_IOCTL_NOTIF_ADDFD", errno)),
                }
            }
        }
        Ok(())
    }

    fn socket_option(&mut self, call: Call, name: &'static str) -> Result<Flow, String> {
        let [fd, _, option, ..] = call.args;
        let fd = verdict::low_half(fd);
        let option = verdict::low_half(option);
        let socket = match self.owned.get_mut(&fd) {
            Some(socket) => match socket_identity(call.tid, fd) {
                Ok(identity) if identity == socket.near_identity => Some(socket),
                Ok(_) => None,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    self.owned.remove(&fd);
                    None
                }
                Err(error) => {
                    return Err(format!("checking {name} descriptor {fd} failed: {error}"));
                }
            },
            None => None,
        };
        let reply = match socket {
            None => Reply::Continue,
            Some(_) if option != L2CAP_OPTIONS => {
                let violation = Violation::UnsupportedOption { call: name, option };
                self.shared.update(|state| state.violations.push(violation));
                Reply::Error(Errno::NOPROTOOPT)
            }
            Some(_)
                if call.number == libc::SYS_setsockopt
                    && self.shared.lock().refuse_l2cap_option_writes =>
            {
                Reply::Error(Errno::ACCESS)
            }
            Some(socket) => match answer_option(self.listener.as_fd(), call, socket) {
                Ok(reply) => reply,
                Err(_) if !notify::still_valid(self.listener.as_fd(), call.id) => {
                    return Ok(Flow::Go);
                }
                Err(error)
                    if error.raw_os_error() == Some(libc::EIO)
                        || error.kind() == io::ErrorKind::UnexpectedEof =>
                {
                    Reply::Error(Errno::FAULT)
                }
                Err(error) => {
                    return Err(format!("answering {name}(L2CAP_OPTIONS) failed: {error}"));
                }
            },
        };
        self.reply(call.id, reply)?;
        Ok(Flow::Go)
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
        let socket = self.owned.get(&fd);
        let pending_socket = socket.is_some_and(|socket| socket.far.is_some());
        let imtu = socket.and_then(|socket| socket.imtu);
        let (violation, errno) = match verdict::connect(&address, pending_socket, route.as_ref()) {
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
                    imtu,
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
                    imtu,
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
                if let Some(far) = self.owned.get_mut(&fd).and_then(|socket| socket.far.take()) {
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

fn socket_identity(
    tid: impl std::fmt::Display,
    fd: impl std::fmt::Display,
) -> io::Result<(u64, u64)> {
    let stat = metadata(format!("/proc/{tid}/fd/{fd}"))?;
    Ok((stat.dev(), stat.ino()))
}

fn answer_option(
    listener: BorrowedFd<'_>,
    call: Call,
    socket: &mut TrappedSocket,
) -> io::Result<Reply> {
    let [_, _, _, value_at, len, ..] = call.args;
    let memory = File::options()
        .read(true)
        .write(true)
        .open(format!("/proc/{}/mem", call.tid))?;
    // The open file pins this memory even if the task exits after this check.
    if !notify::still_valid(listener, call.id) {
        return Ok(Reply::Value(0));
    }
    if call.number == libc::SYS_getsockopt {
        write_options(&memory, value_at, len, socket.imtu.unwrap_or(DEFAULT_MTU))?;
    } else {
        let supplied_len = usize::try_from(verdict::low_half(len)).map_err(io::Error::other)?;
        if socket.far.is_none() || supplied_len < L2CAP_OPTIONS_LEN {
            return Ok(Reply::Error(Errno::INVAL));
        }
        let mut bytes = [0; L2CAP_OPTIONS_LEN];
        memory.read_exact_at(&mut bytes, value_at)?;
        // Sample input: a0 02 00 08 ff ff 00 01 03 00 3f 00, with imtu 2048 in bytes 2 and 3.
        let [_, _, low, high, ..] = bytes;
        socket.imtu = Some(u16::from_le_bytes([low, high]));
    }
    Ok(Reply::Value(0))
}

fn write_options(memory: &File, value_at: u64, len_at: u64, imtu: u16) -> io::Result<()> {
    let [omtu_low, omtu_high] = DEFAULT_MTU.to_le_bytes();
    let [imtu_low, imtu_high] = imtu.to_le_bytes();
    let [flush_low, flush_high] = DEFAULT_FLUSH_TIMEOUT.to_le_bytes();
    let [window_low, window_high] = DEFAULT_TX_WINDOW.to_le_bytes();
    let options = [
        omtu_low,
        omtu_high,
        imtu_low,
        imtu_high,
        flush_low,
        flush_high,
        BASIC_MODE,
        CRC16_FCS,
        DEFAULT_MAX_TX,
        OPTIONS_PADDING,
        window_low,
        window_high,
    ];
    let mut supplied_len = [0; size_of::<u32>()];
    memory.read_exact_at(&mut supplied_len, len_at)?;
    // Sample input: 0c 00 00 00 is the caller's socklen_t capacity in bytes.
    let len = usize::try_from(u32::from_le_bytes(supplied_len)).map_err(io::Error::other)?;
    let count = len.min(options.len());
    let bytes = options.get(..count).ok_or(Errno::INVAL)?;
    let actual_len = u32::try_from(count).map_err(io::Error::other)?;
    memory.write_all_at(bytes, value_at)?;
    memory.write_all_at(&actual_len.to_le_bytes(), len_at)
}

/// Reads the caller's `struct sockaddr_l2` sized address out of its memory.
fn read_address(tid: u32, address_at: u64) -> io::Result<[u8; 14]> {
    let memory = File::open(format!("/proc/{tid}/mem"))?;
    let mut bytes = [0; 14];
    memory.read_exact_at(&mut bytes, address_at)?;
    Ok(bytes)
}
