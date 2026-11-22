//! The guards of a run: what must be sent, and how long the channel must stay open, on every way out.

use std::cell::Cell;

use pods_proto::control::ControlCommand;
use pods_proto::{dictation, uplink};

use super::RECV_BUFFER_LEN;
use super::record::{Bearer, Sent};
use super::wires::{Link, Wake, Wires, receive};

/// How many packets a tail reads at most: a stream that ignored its stop sends about 50 a second, so this is several of the seconds a tail lasts and still ends one a flood would fill.
pub(super) const TAIL_MAX_PACKETS: u32 = 256;

/// Kept here because a drop returns nothing.
#[derive(Default)]
pub(super) struct Dropped {
    /// A STOP the log has not recorded yet.
    pub(super) stop: Cell<Option<Sent>>,
    /// A dictation stop write the log has not recorded yet.
    pub(super) dictation_stop: Cell<Option<Sent>>,
    /// When the last dictation stop write left, which the delay of the pods' reply counts from.
    pub(super) dictation_stop_ns: Cell<Option<u64>>,
    /// An idle session marker the log has not recorded yet.
    pub(super) idle: Cell<Option<Sent>>,
    /// A side restore the log has not recorded yet, with the command it sent.
    pub(super) restore: Cell<Option<(ControlCommand, Sent)>>,
    /// When the last packet of a guard left, which is where the tail counts from.
    pub(super) tail_from_ns: Cell<Option<u64>>,
}

/// Exists exactly while the uplink may be running; dropping it is the only way a STOP is sent.
pub(super) struct Uplink<'a> {
    pub(super) link: Link<'a>,
    pub(super) dropped: &'a Dropped,
}

impl Drop for Uplink<'_> {
    fn drop(&mut self) {
        let sent = self.link.send(&uplink::STOP);
        if sent.result.is_ok() {
            self.dropped.tail_from_ns.set(Some(sent.at_ns));
        }
        self.dropped.stop.set(Some(sent));
    }
}

/// Exists exactly while the dictation stream may be running; dropping it is the only way its stop write is sent.
pub(super) struct Dictation<'a> {
    pub(super) link: Link<'a>,
    pub(super) dropped: &'a Dropped,
}

impl Drop for Dictation<'_> {
    fn drop(&mut self) {
        let sent = self.link.send(&dictation::STOP);
        if sent.result.is_ok() {
            self.dropped.tail_from_ns.set(Some(sent.at_ns));
            self.dropped.dictation_stop_ns.set(Some(sent.at_ns));
        }
        self.dropped.dictation_stop.set(Some(sent));
    }
}

/// Exists exactly while the pods may hold a session marked active; dropping it is the only way the idle marker is sent.
pub(super) struct Session<'a> {
    pub(super) link: Link<'a>,
    pub(super) dropped: &'a Dropped,
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        let sent = self.link.send(&dictation::SESSION_IDLE);
        if sent.result.is_ok() {
            self.dropped.tail_from_ns.set(Some(sent.at_ns));
        }
        self.dropped.idle.set(Some(sent));
    }
}

/// Exists exactly while the microphone side may differ from what the pods reported; dropping it is the only way it is put back.
pub(super) struct SideChange<'a> {
    pub(super) link: Link<'a>,
    pub(super) original: ControlCommand,
    pub(super) dropped: &'a Dropped,
}

impl Drop for SideChange<'_> {
    fn drop(&mut self) {
        let sent = self.link.send(&self.original.encode());
        if sent.result.is_ok() {
            self.dropped.tail_from_ns.set(Some(sent.at_ns));
        }
        self.dropped.restore.set(Some((self.original, sent)));
    }
}

/// Keeps the channels open until the tail after the last packet of a guard has passed or `TAIL_MAX_PACKETS` have arrived; dropped unread, as a panic drops it, it reads what arrives and keeps none of it.
pub(super) struct Tail<'a> {
    pub(super) wires: Wires<'a>,
    pub(super) length_ns: u64,
    pub(super) dropped: &'a Dropped,
}

impl Tail<'_> {
    /// A second call finds nothing left to wait for.
    pub(super) fn read(&mut self, keep: &mut dyn FnMut(u64, Wake<'_>)) {
        let Some(from_ns) = self.dropped.tail_from_ns.take() else {
            return;
        };
        let until_ns = from_ns.saturating_add(self.length_ns);
        let mut buf = [0_u8; RECV_BUFFER_LEN];
        let mut last = Bearer::Att;
        let mut packets = 0_u32;
        while self.wires.any_open() && packets < TAIL_MAX_PACKETS && self.wires.now() < until_ns {
            let (at_ns, wake) = receive(&self.wires, last, &mut buf);
            match wake {
                // A channel that is gone has nothing left to keep open; the other one still has its tail.
                Wake::Closed(bearer) | Wake::Failed(bearer, _) => self.wires.forget(bearer),
                Wake::WaitFailed(_) => return,
                Wake::Sdu(bearer, _) | Wake::TooLong { bearer, .. } => {
                    last = bearer;
                    packets = packets.saturating_add(1);
                    keep(at_ns, wake);
                }
                Wake::Idle => {}
            }
        }
    }
}

impl Drop for Tail<'_> {
    fn drop(&mut self) {
        self.read(&mut |_, _| {});
    }
}
