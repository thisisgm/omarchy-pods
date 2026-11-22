//! L2CAP channels in a trace: ACL fragments joined, identifiers mapped to PSMs by signalling or content.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use pods_proto::sdulog::{Direction, Record};

use super::content::{Carried, Unopened};
use super::{HANDLE_MASK, HciRecord, Kind, sender, take};

/// The packet boundary flag sits above the 12 bit handle (`BlueZ` lib/bluetooth/hci.h, `acl_flags`).
const BOUNDARY_SHIFT: u32 = 12;
/// The packet boundary flag is two bits; the two above it are the broadcast flag (Core 6.1 Vol 4 Part E 5.4.2).
const BOUNDARY_MASK: u16 = 0b11;
/// Packet boundary flag of a continuing fragment; every other value starts a PDU (`BlueZ` monitor/l2cap.c, `l2cap_packet`).
const CONTINUING_FRAGMENT: u16 = 0b01;
/// The signalling channel of a classic link (Core 6.1 Vol 3 Part A 2.1, Table 2.1).
const SIGNALLING_CID: u16 = 0x0001;
/// The first dynamically allocated channel identifier (Core 6.1 Vol 3 Part A 2.1, Table 2.1).
const FIRST_DYNAMIC_CID: u16 = 0x0040;
/// `L2CAP_CONNECTION_REQ` (Core 6.1 Vol 3 Part A 4.2).
const CONNECTION_REQUEST: u8 = 0x02;
/// `L2CAP_CONNECTION_RSP` (Core 6.1 Vol 3 Part A 4.3).
const CONNECTION_RESPONSE: u8 = 0x03;
/// `L2CAP_CONFIGURATION_REQ` (Core 6.1 Vol 3 Part A 4.4).
const CONFIGURATION_REQUEST: u8 = 0x04;
/// `L2CAP_DISCONNECTION_REQ` (Core 6.1 Vol 3 Part A 4.6).
const DISCONNECTION_REQUEST: u8 = 0x06;
/// The MTU configuration option (Core 6.1 Vol 3 Part A 5.1).
const OPTION_MTU: u8 = 0x01;
/// The retransmission and flow control option, whose first byte is the mode (Core 6.1 Vol 3 Part A 5.4).
const OPTION_RETRANSMISSION_AND_FLOW_CONTROL: u8 = 0x04;
/// The size of that option's data; one of another size is not that option (`BlueZ` monitor/l2cap.c, `options_table`).
const RETRANSMISSION_OPTION_LEN: usize = 9;
/// Basic mode, the only one whose PDU payload is one whole packet (`BlueZ` lib/bluetooth/l2cap.h, `L2CAP_MODE_BASIC`).
const BASIC_MODE: u8 = 0x00;
/// The top bit of an option type marks a hint and is not part of the type (Core 6.1 Vol 3 Part A 5).
const OPTION_TYPE_MASK: u8 = 0x7f;

/// The result of a connection response that opened the channel (Core 6.1 Vol 3 Part A 4.3).
pub(crate) const RESULT_SUCCESS: u16 = 0x0000;
/// The result of a connection response that says a final one follows (Core 6.1 Vol 3 Part A 4.3).
const RESULT_PENDING: u16 = 0x0001;
/// The most connection requests listed, and so channels tracked, in one trace; the rest are counted.
pub(crate) const MAX_REQUESTS: usize = 1024;
/// The most unmapped channel identifiers listed in one trace; PDUs on the rest are counted.
pub(crate) const MAX_UNMAPPED: usize = 1024;

/// One L2CAP channel a connection response in the trace opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Channel {
    pub handle: u16,
    /// Which link of the trace it is on: links count from 0 as each one's first packet arrives, and a handle given out again after its link ended is a new link.
    pub link: usize,
    pub psm: u16,
    /// The identifier the host chose for its end.
    pub host_cid: u16,
    /// The identifier the pods chose for theirs.
    pub pods_cid: u16,
    /// The MTU option of the host's last configuration request, the largest SDU it takes.
    pub host_mtu: Option<u16>,
    /// The MTU option of the pods' last configuration request.
    pub pods_mtu: Option<u16>,
    /// The mode in the retransmission and flow control option of the host's last configuration request that held one.
    pub host_mode: Option<u8>,
    /// The same for the pods.
    pub pods_mode: Option<u8>,
    /// PDUs not kept because a side had asked for a mode other than basic, where a payload opens with control fields.
    pub not_basic_pdus: u64,
    /// The position of the record that opened it, among the records `channels` was given.
    pub opened: usize,
    /// The position of the record that closed it; `None` while it is open at the end of the trace.
    pub closed: Option<usize>,
    /// Every SDU on it, in trace order, stamped when its last fragment was seen.
    pub sdus: Vec<Record>,
}

impl Channel {
    /// Whether no side's last configuration request asked for a mode other than basic; only then are its PDUs kept.
    pub(crate) fn is_basic(&self) -> bool {
        let asked = [self.host_mode, self.pods_mode];
        asked.iter().flatten().all(|mode| *mode == BASIC_MODE)
    }
}

/// One connection request and how the trace shows it was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Request {
    pub at_ns: u64,
    pub handle: u16,
    /// Who sent it: `ToPods` is the host, `FromPods` the pods.
    pub direction: Direction,
    pub psm: u16,
    /// The identifier the sender chose for its end.
    pub source_cid: u16,
    /// The result of the last connection response that answered it; `None` when the trace holds none.
    pub result: Option<u16>,
    /// The channel it opened, as a position in `Channels::list`.
    pub channel: Option<usize>,
}

/// A dynamic channel identifier that carried PDUs and that no connection response in the trace opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Unmapped {
    pub handle: u16,
    /// The identifier in the PDUs, which is the receiving side's.
    pub cid: u16,
    pub direction: Direction,
    pub pdus: u64,
}

/// Nothing is dropped without landing in one of these.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub acl_packets: u64,
    /// ACL packets shorter than their header, or whose length field disagrees with their size.
    pub malformed_acl_packets: u64,
    pub continuations_without_start: u64,
    /// PDUs whose start arrived and whose last fragment never did.
    pub unfinished_pdus: u64,
    /// PDUs that hold more bytes than their length field says.
    pub malformed_pdus: u64,
    /// Signalling PDUs with a command or an option cut short, or a command of the wrong size.
    pub malformed_signals: u64,
    /// Connection responses that answer no request in the trace.
    pub responses_without_request: u64,
    /// PDUs on fixed channels other than signalling, which are not read.
    pub fixed_channel_pdus: u64,
    /// Connection requests past `MAX_REQUESTS`.
    pub requests_over_limit: u64,
    /// PDUs on unmapped identifiers past `MAX_UNMAPPED`.
    pub unmapped_over_limit: u64,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Channels {
    /// The channels in the order they opened.
    pub list: Vec<Channel>,
    /// Every connection request in trace order, at most `MAX_REQUESTS`.
    pub requests: Vec<Request>,
    /// At most `MAX_UNMAPPED` identifiers.
    pub unmapped: Vec<Unmapped>,
    /// What content mapped of the identifiers no response opened; only `channels_by_content` fills it.
    pub by_content: Vec<Carried>,
    pub counts: Counts,
}

#[derive(Debug, Default)]
struct Link {
    /// The value its channels carry in `Channel::link`.
    number: usize,
    /// What has arrived of a PDU the host is sending, its header included.
    from_host: Option<Vec<u8>>,
    /// What has arrived of a PDU the pods are sending, its header included.
    from_pods: Option<Vec<u8>>,
    /// Open channels by the identifier the host chose, as positions in `Channels::list`.
    host_cids: BTreeMap<u16, usize>,
    /// The same channels by the identifier the pods chose.
    pods_cids: BTreeMap<u16, usize>,
    /// Requests on this link still waiting for a final response, as positions in `Channels::requests`.
    waiting: Vec<usize>,
}

impl Link {
    /// The identifier in a PDU is the receiving side's.
    fn receiving(&self, sender: Direction, cid: u16) -> Option<usize> {
        let cids = match sender {
            Direction::ToPods => &self.pods_cids,
            Direction::FromPods => &self.host_cids,
        };
        cids.get(&cid).copied()
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Seen {
    /// The position of the record that completed it.
    position: usize,
    pub(super) at_ns: u64,
    pub(super) handle: u16,
    /// The number of the link on that handle.
    pub(super) link: usize,
    pub(super) direction: Direction,
}

/// The walk over the records; links are at most 4096, one per 12 bit handle.
#[derive(Debug, Default)]
struct Reader {
    links: BTreeMap<u16, Link>,
    /// How many links have carried a packet so far, which is the number the next one takes.
    links_started: usize,
    /// Where each listed unmapped identifier sits in `found.unmapped`, by handle, identifier and whether the pods sent.
    unmapped_at: BTreeMap<(u16, u16, bool), usize>,
    /// Set when identifiers no response opened are to be mapped by content, which takes their PDUs and not only a count.
    unopened: Option<Unopened>,
    found: Channels,
}

pub(super) fn bump(count: &mut u64) {
    *count = count.saturating_add(1);
}

/// The link on `handle`, which takes the next number when this is its first packet.
fn link_on<'a>(
    links: &'a mut BTreeMap<u16, Link>,
    started: &mut usize,
    handle: u16,
) -> &'a mut Link {
    links.entry(handle).or_insert_with(|| {
        let number = *started;
        *started = started.saturating_add(1);
        Link {
            number,
            ..Link::default()
        }
    })
}

fn other(direction: Direction) -> Direction {
    match direction {
        Direction::ToPods => Direction::FromPods,
        Direction::FromPods => Direction::ToPods,
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Options {
    mtu: Option<u16>,
    mode: Option<u8>,
}

// Sample input: 01 02 a0 02, then 04 09 03 3f 03 d0 07 e0 2e f0 03: the MTU option with 672 in little endian, then retransmission and flow control with mode 3.
fn options(mut rest: &[u8]) -> (Options, bool) {
    let mut found = Options::default();
    while !rest.is_empty() {
        let Some([kind, len]) = take(&mut rest) else {
            return (found, false);
        };
        let Some((data, after)) = rest.split_at_checked(usize::from(len)) else {
            return (found, false);
        };
        rest = after;
        match kind & OPTION_TYPE_MASK {
            OPTION_MTU => {
                if let Ok(value) = <[u8; 2]>::try_from(data) {
                    found.mtu = Some(u16::from_le_bytes(value));
                }
            }
            OPTION_RETRANSMISSION_AND_FLOW_CONTROL => {
                if let Ok([mode, ..]) = <[u8; RETRANSMISSION_OPTION_LEN]>::try_from(data) {
                    found.mode = Some(mode);
                }
            }
            _ => {}
        }
    }
    (found, true)
}

impl Reader {
    // Sample input: 0b 20 05 00 01 00 40 00 aa, handle 0x00b with boundary flag 0b10, 5 data bytes: a whole PDU of 1 byte for channel 0x0040.
    fn acl(&mut self, position: usize, record: &HciRecord, direction: Direction) {
        let counts = &mut self.found.counts;
        bump(&mut counts.acl_packets);
        let mut rest = record.data.as_slice();
        let (Some(field), Some(len)) = (take::<2>(&mut rest), take::<2>(&mut rest)) else {
            return bump(&mut counts.malformed_acl_packets);
        };
        if usize::from(u16::from_le_bytes(len)) != rest.len() {
            return bump(&mut counts.malformed_acl_packets);
        }
        let field = u16::from_le_bytes(field);
        let handle = field & HANDLE_MASK;
        let link = link_on(&mut self.links, &mut self.links_started, handle);
        let seen = Seen {
            position,
            at_ns: record.at_ns,
            handle,
            link: link.number,
            direction,
        };
        let slot = match direction {
            Direction::ToPods => &mut link.from_host,
            Direction::FromPods => &mut link.from_pods,
        };
        // A buffer grows only by bytes that arrived, never by what a length field claims.
        let pdu = if (field >> BOUNDARY_SHIFT) & BOUNDARY_MASK == CONTINUING_FRAGMENT {
            let Some(mut joined) = slot.take() else {
                return bump(&mut counts.continuations_without_start);
            };
            joined.extend_from_slice(rest);
            joined
        } else {
            if slot.take().is_some() {
                bump(&mut counts.unfinished_pdus);
            }
            rest.to_vec()
        };
        let mut payload = pdu.as_slice();
        // A first fragment may end inside the 4 byte header; the rest follows in a continuation (Linux net/bluetooth/l2cap_core.c, `l2cap_recv_acldata`).
        let (Some(len), Some(cid)) = (take::<2>(&mut payload), take::<2>(&mut payload)) else {
            *slot = Some(pdu);
            return;
        };
        match payload.len().cmp(&usize::from(u16::from_le_bytes(len))) {
            Ordering::Less => *slot = Some(pdu),
            Ordering::Greater => bump(&mut counts.malformed_pdus),
            Ordering::Equal => self.pdu(seen, u16::from_le_bytes(cid), payload),
        }
    }

    // Sample input: channel 0x0001 carries signalling commands, 0x0002 to 0x003f are fixed channels, 0x0040 and up are opened by a connection response.
    fn pdu(&mut self, seen: Seen, cid: u16, payload: &[u8]) {
        if cid == SIGNALLING_CID {
            self.signals(seen, payload);
        } else if cid < FIRST_DYNAMIC_CID {
            bump(&mut self.found.counts.fixed_channel_pdus);
        } else {
            self.data(seen, cid, payload);
        }
    }

    fn data(&mut self, seen: Seen, cid: u16, payload: &[u8]) {
        let open = self
            .links
            .get(&seen.handle)
            .and_then(|link| link.receiving(seen.direction, cid))
            .and_then(|index| self.found.list.get_mut(index));
        if let Some(channel) = open {
            if channel.is_basic() {
                channel.sdus.push(Record {
                    at_ns: seen.at_ns,
                    direction: seen.direction,
                    sdu: payload.to_vec(),
                });
            } else {
                bump(&mut channel.not_basic_pdus);
            }
            return;
        }
        if let Some(unopened) = &mut self.unopened {
            return unopened.keep(seen, cid, payload, &mut self.found.counts);
        }
        let key = (seen.handle, cid, seen.direction == Direction::FromPods);
        let listed = self.unmapped_at.get(&key).copied();
        if let Some(entry) = listed.and_then(|index| self.found.unmapped.get_mut(index)) {
            entry.pdus = entry.pdus.saturating_add(1);
        } else if self.found.unmapped.len() >= MAX_UNMAPPED {
            bump(&mut self.found.counts.unmapped_over_limit);
        } else {
            self.unmapped_at.insert(key, self.found.unmapped.len());
            self.found.unmapped.push(Unmapped {
                handle: seen.handle,
                cid,
                direction: seen.direction,
                pdus: 1,
            });
        }
    }

    // Sample input: 02 01 04 00 01 10 40 00, one command: code, identifier, data length 4, then the data; a PDU may hold several.
    fn signals(&mut self, seen: Seen, mut rest: &[u8]) {
        while !rest.is_empty() {
            let header = (take::<2>(&mut rest), take::<2>(&mut rest));
            let (Some([code, _identifier]), Some(len)) = header else {
                return bump(&mut self.found.counts.malformed_signals);
            };
            let len = usize::from(u16::from_le_bytes(len));
            let Some((data, after)) = rest.split_at_checked(len) else {
                return bump(&mut self.found.counts.malformed_signals);
            };
            rest = after;
            let read = match code {
                CONNECTION_REQUEST => self.connection_request(seen, data),
                CONNECTION_RESPONSE => self.connection_response(seen, data),
                CONFIGURATION_REQUEST => self.configuration_request(seen, data),
                DISCONNECTION_REQUEST => self.disconnection_request(seen, data),
                _ => Some(()),
            };
            if read.is_none() {
                bump(&mut self.found.counts.malformed_signals);
            }
        }
    }

    // Sample input: 01 10 40 00, PSM 0x1001 and the identifier 0x0040 the sender chose, both little endian.
    fn connection_request(&mut self, seen: Seen, data: &[u8]) -> Option<()> {
        let [psm_low, psm_high, cid_low, cid_high] = <[u8; 4]>::try_from(data).ok()?;
        if self.found.requests.len() >= MAX_REQUESTS {
            bump(&mut self.found.counts.requests_over_limit);
            return Some(());
        }
        let link = link_on(&mut self.links, &mut self.links_started, seen.handle);
        link.waiting.push(self.found.requests.len());
        self.found.requests.push(Request {
            at_ns: seen.at_ns,
            handle: seen.handle,
            direction: seen.direction,
            psm: u16::from_le_bytes([psm_low, psm_high]),
            source_cid: u16::from_le_bytes([cid_low, cid_high]),
            result: None,
            channel: None,
        });
        Some(())
    }

    // Sample input: 50 00 40 00 00 00 00 00, the responder's identifier 0x0050, the requester's 0x0040, result success, status.
    fn connection_response(&mut self, seen: Seen, data: &[u8]) -> Option<()> {
        let mut rest = data;
        let responder_cid = u16::from_le_bytes(take(&mut rest)?);
        let requester_cid = u16::from_le_bytes(take(&mut rest)?);
        let result = u16::from_le_bytes(take(&mut rest)?);
        let _status: [u8; 2] = take(&mut rest)?;
        if !rest.is_empty() {
            return None;
        }
        let index = self.found.list.len();
        let requests = &mut self.found.requests;
        // Only a request of this link still waiting is answered, so a stale or settled one never lends its PSM.
        let waiting = self.links.get_mut(&seen.handle).and_then(|link| {
            let position = link.waiting.iter().rev().copied().find(|position| {
                requests.get(*position).is_some_and(|request| {
                    request.direction != seen.direction && request.source_cid == requester_cid
                })
            })?;
            if result != RESULT_PENDING {
                link.waiting.retain(|other| *other != position);
            }
            Some(position)
        });
        let Some(request) = waiting.and_then(|position| requests.get_mut(position)) else {
            bump(&mut self.found.counts.responses_without_request);
            return Some(());
        };
        request.result = Some(result);
        if result != RESULT_SUCCESS {
            return Some(());
        }
        request.channel = Some(index);
        let (host_cid, pods_cid) = match seen.direction {
            Direction::FromPods => (requester_cid, responder_cid),
            Direction::ToPods => (responder_cid, requester_cid),
        };
        let psm = request.psm;
        // An identifier given out again means the channel that held it is gone.
        for stale in [
            self.open(seen.handle, Direction::FromPods, host_cid),
            self.open(seen.handle, Direction::ToPods, pods_cid),
        ] {
            self.close(stale, seen);
        }
        let link = link_on(&mut self.links, &mut self.links_started, seen.handle);
        link.host_cids.insert(host_cid, index);
        link.pods_cids.insert(pods_cid, index);
        let link = link.number;
        self.found.list.push(Channel {
            handle: seen.handle,
            link,
            psm,
            host_cid,
            pods_cid,
            host_mtu: None,
            pods_mtu: None,
            host_mode: None,
            pods_mode: None,
            not_basic_pdus: 0,
            opened: seen.position,
            closed: None,
            sdus: Vec::new(),
        });
        Some(())
    }

    // Sample input: 50 00 00 00 01 02 a0 02, the receiver's identifier 0x0050, flags, then options: here the sender's MTU, 672.
    fn configuration_request(&mut self, seen: Seen, data: &[u8]) -> Option<()> {
        let mut rest = data;
        let cid = u16::from_le_bytes(take(&mut rest)?);
        let _flags: [u8; 2] = take(&mut rest)?;
        let (sent, whole) = options(rest);
        let channel = self
            .open(seen.handle, seen.direction, cid)
            .and_then(|index| self.found.list.get_mut(index));
        if let Some(channel) = channel {
            let (mtu, mode) = match seen.direction {
                Direction::ToPods => (&mut channel.host_mtu, &mut channel.host_mode),
                Direction::FromPods => (&mut channel.pods_mtu, &mut channel.pods_mode),
            };
            *mtu = sent.mtu.or(*mtu);
            *mode = sent.mode.or(*mode);
        }
        whole.then_some(())
    }

    // Sample input: 50 00 40 00, the receiver's identifier 0x0050, then the sender's 0x0040; both must name one channel.
    fn disconnection_request(&mut self, seen: Seen, data: &[u8]) -> Option<()> {
        let [receiver_low, receiver_high, sender_low, sender_high] =
            <[u8; 4]>::try_from(data).ok()?;
        let receiver_cid = u16::from_le_bytes([receiver_low, receiver_high]);
        let sender_cid = u16::from_le_bytes([sender_low, sender_high]);
        let named = self.open(seen.handle, seen.direction, receiver_cid);
        if named.is_some() && named == self.open(seen.handle, other(seen.direction), sender_cid) {
            self.close(named, seen);
        }
        Some(())
    }

    /// The open channel on `handle` whose end at the side receiving from `sender` has this identifier.
    fn open(&self, handle: u16, sender: Direction, cid: u16) -> Option<usize> {
        self.links.get(&handle)?.receiving(sender, cid)
    }

    fn close(&mut self, index: Option<usize>, seen: Seen) {
        let Some(index) = index else { return };
        let Some(channel) = self.found.list.get_mut(index) else {
            return;
        };
        channel.closed = Some(seen.position);
        if let Some(link) = self.links.get_mut(&seen.handle) {
            link.host_cids.retain(|_, open| *open != index);
            link.pods_cids.retain(|_, open| *open != index);
        }
    }

    /// The link is gone: its channels close and a PDU still being joined never finishes.
    fn end_link(&mut self, position: usize, handle: u16) {
        let Some(link) = self.links.remove(&handle) else {
            return;
        };
        self.count_unfinished(&link);
        for index in link.host_cids.values() {
            if let Some(channel) = self.found.list.get_mut(*index) {
                channel.closed = Some(position);
            }
        }
    }

    fn count_unfinished(&mut self, link: &Link) {
        for partial in [&link.from_host, &link.from_pods] {
            if partial.is_some() {
                bump(&mut self.found.counts.unfinished_pdus);
            }
        }
    }

    fn read(mut self, records: &[HciRecord]) -> Self {
        for (position, record) in records.iter().enumerate() {
            if let Some(direction) = sender(record.kind) {
                self.acl(position, record, direction);
            } else if let Kind::Disconnection { handle } = record.kind {
                self.end_link(position, handle);
            }
        }
        let links = std::mem::take(&mut self.links);
        for link in links.values() {
            self.count_unfinished(link);
        }
        self
    }
}

pub(crate) fn channels(records: &[HciRecord]) -> Channels {
    Reader::default().read(records).found
}

/// As `channels`, then what no response opened is mapped by content; one no rule matches stays unmapped.
pub(crate) fn channels_by_content(records: &[HciRecord]) -> Channels {
    let by_content = Reader {
        unopened: Some(Unopened::default()),
        ..Reader::default()
    };
    let Reader {
        unopened,
        mut found,
        ..
    } = by_content.read(records);
    if let Some(unopened) = unopened {
        unopened.settle(&mut found);
    }
    found
}

#[cfg(test)]
mod tests;
