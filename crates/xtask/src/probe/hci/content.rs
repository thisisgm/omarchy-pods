//! Channels no connection request named, as in a trace macOS wrote, told by what each direction of an identifier carries.

use std::collections::BTreeMap;

use pods_proto::dictation::{self, DictationPacket};
use pods_proto::packet::ACCESSORY_HEADER;
use pods_proto::sdulog::{Direction, Record};

use super::l2cap::{Channels, Counts, MAX_UNMAPPED, Seen, Unmapped, bump};

/// What content says a channel carries; the PSM it was opened on is not in the trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Carries {
    /// The packets of the accessory protocol, which the control channel carries.
    Accessory,
    /// Attribute protocol PDUs, the dictation stream among them.
    Attributes,
}

/// One direction of one channel identifier that content mapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Half {
    /// The identifier in the PDUs, which is the receiving side's.
    pub cid: u16,
    /// Who sent the PDUs.
    pub direction: Direction,
    pub pdus: u64,
}

/// Everything of one kind on one link: no request pairs the identifiers, so its halves are not paired into channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Carried {
    pub handle: u16,
    /// Which link of the trace it is on, as in `Channel::link`.
    pub link: usize,
    pub carries: Carries,
    /// The halves content mapped here, in the order each first sent.
    pub halves: Vec<Half>,
    /// Every PDU of those halves, in trace order.
    pub sdus: Vec<Record>,
}

/// One half while the trace is read: where it is, and the content that mapped it once a rule matched.
#[derive(Debug)]
struct Tracked {
    handle: u16,
    link: usize,
    half: Half,
    carries: Option<Carries>,
}

/// Every PDU on an identifier no response opened, kept until the end of the trace has shown what each half carried.
#[derive(Debug, Default)]
pub(super) struct Unopened {
    /// At most `MAX_UNMAPPED` halves, in the order each first sent.
    halves: Vec<Tracked>,
    /// Where each half sits in `halves`, by link, identifier and whether the pods sent.
    at: BTreeMap<(usize, u16, bool), usize>,
    /// Each PDU with the position of its half, in trace order; never more bytes than the trace holds.
    pdus: Vec<(usize, Record)>,
}

// Sample input: `04 00 04 00 4b 00 03 00 03 00 00` as the first PDU of a half maps it as accessory; of what a half carries later, `1b 0d 00 01 00 00 06 00 80 c8 00 00 01 b8` from the pods or `52 07 00 01 00` from the host maps it as attributes.
fn carried(half: Half, sdu: &[u8]) -> Option<Carries> {
    if half.pdus == 0 && sdu.starts_with(&ACCESSORY_HEADER) {
        return Some(Carries::Accessory);
    }
    let attributes = match half.direction {
        Direction::FromPods => {
            matches!(
                DictationPacket::parse(sdu),
                Ok(Some(DictationPacket::Audio(_)))
            )
        }
        Direction::ToPods => sdu == dictation::START,
    };
    attributes.then_some(Carries::Attributes)
}

impl Unopened {
    /// Keeps one PDU; its half is listed first when this is its first, and past the limit the PDU is counted instead.
    pub(super) fn keep(&mut self, seen: Seen, cid: u16, sdu: &[u8], counts: &mut Counts) {
        let key = (seen.link, cid, seen.direction == Direction::FromPods);
        let listed = self.halves.len();
        let index = match self.at.get(&key) {
            Some(index) => *index,
            None if listed >= MAX_UNMAPPED => return bump(&mut counts.unmapped_over_limit),
            None => {
                self.at.insert(key, listed);
                self.halves.push(Tracked {
                    handle: seen.handle,
                    link: seen.link,
                    half: Half {
                        cid,
                        direction: seen.direction,
                        pdus: 0,
                    },
                    carries: None,
                });
                listed
            }
        };
        let Some(tracked) = self.halves.get_mut(index) else {
            return;
        };
        // The first rule that matches stands, so a later PDU never moves a half from one log to another.
        tracked.carries = tracked.carries.or_else(|| carried(tracked.half, sdu));
        bump(&mut tracked.half.pdus);
        let record = Record {
            at_ns: seen.at_ns,
            direction: seen.direction,
            sdu: sdu.to_vec(),
        };
        self.pdus.push((index, record));
    }

    /// Lists every half as mapped, grouped by link and kind, or as unmapped, and hands the mapped ones their PDUs.
    pub(super) fn settle(self, found: &mut Channels) {
        let mut groups: BTreeMap<(usize, Carries), usize> = BTreeMap::new();
        let mut rows: BTreeMap<(u16, u16, bool), usize> = BTreeMap::new();
        let mut group_of = Vec::with_capacity(self.halves.len());
        for Tracked {
            handle,
            link,
            half,
            carries,
        } in self.halves
        {
            let Some(carries) = carries else {
                // One row per handle, identifier and direction, as the signalling reader lists them: a handle given out again adds to its row.
                let key = (handle, half.cid, half.direction == Direction::FromPods);
                let row = *rows.entry(key).or_insert_with(|| {
                    found.unmapped.push(Unmapped {
                        handle,
                        cid: half.cid,
                        direction: half.direction,
                        pdus: 0,
                    });
                    found.unmapped.len().saturating_sub(1)
                });
                if let Some(unmapped) = found.unmapped.get_mut(row) {
                    unmapped.pdus = unmapped.pdus.saturating_add(half.pdus);
                }
                group_of.push(None);
                continue;
            };
            let group = *groups.entry((link, carries)).or_insert_with(|| {
                found.by_content.push(Carried {
                    handle,
                    link,
                    carries,
                    halves: Vec::new(),
                    sdus: Vec::new(),
                });
                found.by_content.len().saturating_sub(1)
            });
            if let Some(carried) = found.by_content.get_mut(group) {
                carried.halves.push(half);
            }
            group_of.push(Some(group));
        }
        for (half, record) in self.pdus {
            let group = group_of.get(half).copied().flatten();
            if let Some(carried) = group.and_then(|group| found.by_content.get_mut(group)) {
                carried.sdus.push(record);
            }
        }
    }
}

#[cfg(test)]
mod tests;
