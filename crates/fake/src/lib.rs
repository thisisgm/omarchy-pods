//! Fake pods for tests: they answer the control channel and the dictation service from sample bytes alone.
#![cfg_attr(
    test,
    expect(
        dead_code_pub_in_binary,
        reason = "the unit tests drive the machines of the uplink and of the dictation service; the tests under tests/ drive their handles over socket pairs"
    )
)]

pub mod doap;
mod model;
mod serve;
pub mod uplink;

use std::fs::File;
use std::time::{Duration, Instant};

use pods_samples::{CASE, Cell, LEFT, MODEL_NUMBER, NAME, Placed, RIGHT};
use pods_sys::l2cap::{Channel, from_fd};
use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};

pub use serve::Pods;

/// The wire value of `pods_samples::NOISE` for off, the listening mode of the first burst.
const NOISE_OFF: u8 = 0x01;

/// What the pods report about themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub model_number: String,
    pub left: Cell,
    pub right: Cell,
    pub case: Cell,
    pub primary: Placed,
    pub secondary: Placed,
    /// The wire value of `pods_samples::NOISE`.
    pub noise: u8,
    /// The identity and payload keys of the proximity keys reply; `None` never answers the request.
    pub keys: Option<([u8; 16], [u8; 16])>,
}

impl Default for Profile {
    /// The values of `pods_samples::first_burst()`.
    fn default() -> Self {
        Self {
            name: NAME.to_owned(),
            model_number: MODEL_NUMBER.to_owned(),
            left: LEFT,
            right: RIGHT,
            case: CASE,
            primary: Placed::InEar,
            secondary: Placed::InEar,
            noise: NOISE_OFF,
            keys: None,
        }
    }
}

/// A way the pods misbehave, for channels opened after it is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    None,
    /// Reads everything, sends nothing.
    NeverAcknowledge,
    /// Closes the channel after sending this many packets; 0 closes at once.
    CloseAfter(usize),
    /// Sends the battery and the ear packet as the channel opens, then acknowledges nothing.
    ReportsBeforeAck,
    /// The burst has no battery packet, however often it is asked for.
    WithholdBattery,
    /// The burst's battery packet arrives one byte short, so it does not parse.
    CutBattery,
    /// The burst ends with one more packet, cut inside its opcode.
    TrailingFragment,
    /// A setting from the host changes nothing and is not reported back.
    NeverConfirm,
}

/// One SDU the host sent, and when the fake read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heard {
    pub at: Instant,
    pub sdu: Vec<u8>,
}

/// A wait that ran out, and how long it waited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadline {
    pub waited: Duration,
}

/// An `AF_UNIX` `SOCK_SEQPACKET` pair as `(host, pods)`; panics when the kernel cannot make one.
#[must_use]
#[expect(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "a test tool: with no socket pair there is no test to run, and the one line says so"
)]
#[expect(
    clippy::disallowed_methods,
    reason = "a test wraps both ends of a socket pair as channels"
)]
pub fn pair() -> (Channel, Channel) {
    let (host, pods) = socketpair(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .expect("the kernel makes an AF_UNIX SOCK_SEQPACKET socket pair");
    (from_fd(host), from_fd(pods))
}

/// A channel around a regular file, which epoll refuses to watch; panics when the file cannot be opened.
#[must_use]
#[expect(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "a test tool: a process that cannot open its own program has no test to run, and the one line says so"
)]
#[expect(
    clippy::disallowed_methods,
    reason = "a test wraps a file as a channel"
)]
pub fn unwatchable() -> Channel {
    let file = File::open("/proc/self/exe").expect("the running program opens as a file");
    from_fd(file.into())
}

#[cfg(test)]
mod tests {
    use pods_sys::l2cap::ChannelError;
    use rustix::io::Errno;

    use super::unwatchable;

    #[test]
    fn an_unwatchable_channel_is_no_socket() {
        let error = unwatchable().try_recv(&mut [0]).unwrap_err();
        assert_eq!(error, ChannelError::Recv(Errno::NOTSOCK));
    }
}
