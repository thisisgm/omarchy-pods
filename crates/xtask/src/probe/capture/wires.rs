//! The channels of a run and its clock: the control channel its caller opened, and the attribute channel the run opens and may close itself.

use std::cell::RefCell;
use std::time::Duration;

use pods_sys::l2cap::{self, Channel, ChannelError, Received};
use rustix::io::Errno;

use super::record::{Bearer, IoFailure, Sent};

/// Bounds how late a timed action fires and how late a signal is seen; chosen here.
pub(super) const RECV_TIMEOUT_MS: u64 = 10;

/// Opens the attribute channel, which a run does itself, when its script says to, because it may also have to close it.
pub(crate) type OpenAtt<'a> = &'a dyn Fn() -> Result<Channel, ChannelError>;

#[derive(Clone, Copy)]
pub(super) struct Wires<'a> {
    pub(super) accessory: Option<&'a Channel>,
    /// The attribute channel while it is open; taking it out closes it.
    pub(super) att: &'a RefCell<Option<Channel>>,
    pub(super) now_ns: &'a dyn Fn() -> u64,
}

impl<'a> Wires<'a> {
    pub(super) fn now(&self) -> u64 {
        (self.now_ns)()
    }

    fn is_open(&self, bearer: Bearer) -> bool {
        match bearer {
            Bearer::Accessory => self.accessory.is_some(),
            Bearer::Att => self.att.borrow().is_some(),
        }
    }

    /// The channel a sender needs with the clock; a script that needs one that is not open ends the run here.
    pub(super) fn link(&self, bearer: Bearer) -> Result<Link<'a>, IoFailure> {
        if self.is_open(bearer) {
            Ok(Link {
                bearer,
                wires: *self,
            })
        } else {
            let what = bearer.name();
            Err(IoFailure::NotGiven { what })
        }
    }

    pub(super) fn any_open(&self) -> bool {
        self.is_open(Bearer::Accessory) || self.is_open(Bearer::Att)
    }

    /// Stops waiting on a channel that is gone; the attribute channel is closed with it, the control channel stays with its owner.
    pub(super) fn forget(&mut self, bearer: Bearer) {
        match bearer {
            Bearer::Accessory => self.accessory = None,
            Bearer::Att => drop(self.att.take()),
        }
    }
}

/// One channel of a run, for whoever sends on it.
#[derive(Clone, Copy)]
pub(super) struct Link<'a> {
    bearer: Bearer,
    wires: Wires<'a>,
}

impl Link<'_> {
    /// Sends one packet; the stamp is taken just before the send, so it is when the host asked, and a channel closed since then refuses the packet as the kernel would.
    pub(super) fn send(&self, sdu: &[u8]) -> Sent {
        let at_ns = self.wires.now();
        let result = match self.bearer {
            Bearer::Accessory => self.wires.accessory.map(|channel| channel.send(sdu)),
            Bearer::Att => {
                let att = self.wires.att.borrow();
                att.as_ref().map(|channel| channel.send(sdu))
            }
        };
        Sent {
            bearer: self.bearer,
            at_ns,
            result: result.unwrap_or(Err(ChannelError::Send(Errno::NOTCONN))),
        }
    }
}

/// What one wait on the channels ended with.
#[derive(Clone, Copy)]
pub(super) enum Wake<'b> {
    Sdu(Bearer, &'b [u8]),
    /// A packet of this many bytes that the buffer could not hold.
    TooLong {
        bearer: Bearer,
        full_len: usize,
    },
    /// The receive timeout or a signal: nothing arrived.
    Idle,
    Closed(Bearer),
    Failed(Bearer, ChannelError),
    /// The wait itself failed, so no channel is to blame.
    WaitFailed(ChannelError),
}

/// One wait over the open channels, which asks the one read `last` after the other so that neither waits behind a busy one; the stamp is the first statement after the receive returns.
pub(super) fn receive<'b>(wires: &Wires<'_>, last: Bearer, buf: &'b mut [u8]) -> (u64, Wake<'b>) {
    let att = wires.att.borrow();
    let in_turn = [last.other(), last];
    let open: Vec<(Bearer, &Channel)> = in_turn
        .into_iter()
        .filter_map(|bearer| {
            let channel = match bearer {
                Bearer::Accessory => wires.accessory,
                Bearer::Att => att.as_ref(),
            };
            Some((bearer, channel?))
        })
        .collect();
    let channels: Vec<&Channel> = open.iter().map(|(_, channel)| *channel).collect();
    let ready = match l2cap::wait_any(&channels, Duration::from_millis(RECV_TIMEOUT_MS)) {
        Ok(ready) => ready,
        Err(ChannelError::Wait(Errno::INTR)) => None,
        Err(error) => return (wires.now(), Wake::WaitFailed(error)),
    };
    let Some((bearer, channel)) = ready.and_then(|index| open.get(index).copied()) else {
        return (wires.now(), Wake::Idle);
    };
    let received = channel.recv(buf);
    let at_ns = wires.now();
    (at_ns, wake_of(bearer, received, buf))
}

/// Reads what a receive on `bearer` returned; a timeout and a signal both mean that nothing arrived.
pub(super) fn wake_of(
    bearer: Bearer,
    received: Result<Received, ChannelError>,
    buf: &[u8],
) -> Wake<'_> {
    match received {
        Ok(Received::Sdu(len)) => match buf.get(..len) {
            Some(sdu) => Wake::Sdu(bearer, sdu),
            None => Wake::TooLong {
                bearer,
                full_len: len,
            },
        },
        Ok(Received::Truncated { full_len }) => Wake::TooLong { bearer, full_len },
        Ok(Received::TimedOut) | Err(ChannelError::Recv(Errno::INTR)) => Wake::Idle,
        Ok(Received::Closed) => Wake::Closed(bearer),
        Err(error) => Wake::Failed(bearer, error),
    }
}
