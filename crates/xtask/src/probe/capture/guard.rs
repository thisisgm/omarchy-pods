//! The guards of a run: what must be sent, and how long the channel must stay open, on every way out.

use std::cell::Cell;

use pods_proto::control::ControlCommand;
use pods_proto::uplink;

use super::record::Sent;
use super::{Link, RECV_BUFFER_LEN, Wake, receive};

/// What the guards did as they dropped, kept here because a drop returns nothing.
#[derive(Default)]
pub(super) struct Dropped {
    /// A STOP the log has not recorded yet.
    pub(super) stop: Cell<Option<Sent>>,
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

/// Keeps the channel open until the tail after the last packet of a guard has passed; packets that arrive meanwhile are read and not logged.
pub(super) struct Tail<'a> {
    pub(super) link: Link<'a>,
    pub(super) length_ns: u64,
    pub(super) dropped: &'a Dropped,
}

impl Drop for Tail<'_> {
    fn drop(&mut self) {
        let Some(from_ns) = self.dropped.tail_from_ns.get() else {
            return;
        };
        let until_ns = from_ns.saturating_add(self.length_ns);
        let mut buf = [0_u8; RECV_BUFFER_LEN];
        while self.link.now() < until_ns {
            if matches!(
                receive(self.link, &mut buf).1,
                Wake::Closed | Wake::Failed(_)
            ) {
                // The channel is gone, so there is nothing left to keep open.
                return;
            }
        }
    }
}
