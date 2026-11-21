//! Sorts a stretch of the log into the packets each analysis reads, and cuts the log at the marks.

use pods_proto::battery::BatteryReport;
use pods_proto::error::{HEADER_LEN, ParseError};
use pods_proto::sdulog::{Direction, Record};
use pods_proto::uplink::{self, OPAQUE_LEN, UplinkPacket};

use super::bands::max_sfb;
use super::input::{Mark, MarkKind};

/// Where the little endian opcode of a control channel packet starts.
const OPCODE_AT: usize = 4;
/// The opcode is two bytes.
const OPCODE_LEN: usize = 2;
/// A battery packet starts with these bytes.
const BATTERY_PREFIX: [u8; HEADER_LEN] = [0x04, 0x00, 0x04, 0x00, 0x04, 0x00];
/// A device information packet starts with these bytes.
const DEVICE_INFO_PREFIX: [u8; 5] = [0x04, 0x00, 0x04, 0x00, 0x1d];
/// The byte that closes each string of a device information packet.
const STRING_END: u8 = 0x00;

/// One access unit of an audio SDU, without its coded bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Unit {
    pub(crate) stamp: u32,
    /// Coded bytes in the unit.
    pub(crate) len: usize,
    /// The band limit the unit declares, `None` for a unit under two bytes.
    pub(crate) max_sfb: Option<u8>,
}

/// One audio SDU the pods sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AudioPacket {
    /// Its place among the records that were loaded, counted from 0.
    pub(crate) record: usize,
    pub(crate) at_ns: u64,
    pub(crate) opaque: [u8; OPAQUE_LEN],
    /// The whole units, in order.
    pub(crate) units: Vec<Unit>,
    /// Bytes in the SDU.
    pub(crate) len: usize,
    /// Bytes after the last whole unit.
    pub(crate) trailing: usize,
}

/// One uplink control packet the pods sent, kept whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ControlPacket {
    pub(crate) at_ns: u64,
    pub(crate) bytes: Vec<u8>,
    pub(crate) length_matches: bool,
}

/// What every packet from the pods is counted by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Seen {
    /// `None` for a packet too short to hold one.
    pub(crate) opcode: Option<u16>,
    pub(crate) len: usize,
    /// Whether the uplink parser took the packet as its own, well formed or not.
    pub(crate) uplink: bool,
}

/// The packets of one stretch of the log, sorted by what reads them.
#[derive(Debug, Default)]
pub(crate) struct Loaded {
    /// Every packet the pods sent, in log order.
    pub(crate) seen: Vec<Seen>,
    pub(crate) audio: Vec<AudioPacket>,
    pub(crate) controls: Vec<ControlPacket>,
    /// Every battery packet the battery parser read.
    pub(crate) batteries: Vec<BatteryReport>,
    /// Battery packets the battery parser refused.
    pub(crate) battery_unread: u32,
    /// The strings of each device information packet.
    pub(crate) device_info: Vec<Vec<String>>,
    /// Uplink packets whose subtype is neither audio nor control.
    pub(crate) unknown_subtype: u32,
    /// Uplink packets cut before their fixed fields.
    pub(crate) cut_short: u32,
    /// Packets the host sent, which no figure reads.
    pub(crate) to_pods: u32,
}

impl Loaded {
    /// Every whole unit, in order.
    pub(crate) fn units(&self) -> impl Iterator<Item = &Unit> {
        self.audio.iter().flat_map(|sdu| &sdu.units)
    }

    fn take(&mut self, index: usize, record: &Record) {
        let uplink = match UplinkPacket::parse(&record.sdu) {
            Ok(None) => {
                self.take_other(&record.sdu);
                false
            }
            Ok(Some(UplinkPacket::Audio(audio))) => {
                self.audio.push(read_audio(index, record, &audio));
                true
            }
            Ok(Some(UplinkPacket::Control(control))) => {
                self.controls.push(ControlPacket {
                    at_ns: record.at_ns,
                    bytes: record.sdu.clone(),
                    length_matches: control.length_matches(),
                });
                true
            }
            Ok(Some(UplinkPacket::Unknown { .. })) => {
                self.unknown_subtype = self.unknown_subtype.saturating_add(1);
                true
            }
            // The uplink parser refuses only a packet cut before its fixed fields, which is counted here.
            Err(_) => {
                self.cut_short = self.cut_short.saturating_add(1);
                true
            }
        };
        self.seen.push(Seen {
            opcode: opcode(&record.sdu),
            len: record.sdu.len(),
            uplink,
        });
    }

    fn take_other(&mut self, sdu: &[u8]) {
        let refused = match BatteryReport::parse(sdu) {
            Ok(report) => {
                self.batteries.push(report);
                false
            }
            // The parser refuses a short packet before it reads the header, so the header is read here.
            Err(ParseError::TooShort { .. }) => sdu.starts_with(&BATTERY_PREFIX),
            // Another header: not the battery parser's to read.
            Err(ParseError::BadHeader { .. }) => false,
            Err(
                ParseError::UnknownValue { .. }
                | ParseError::WrongLength { .. }
                | ParseError::EmptyList { .. },
            ) => true,
        };
        if refused {
            self.battery_unread = self.battery_unread.saturating_add(1);
        }
        if sdu.starts_with(&DEVICE_INFO_PREFIX) {
            self.device_info.push(device_strings(sdu));
        }
    }
}

/// Sorts `records`; nothing is dropped, and a packet no analysis reads is still counted.
pub(crate) fn load(records: &[Record]) -> Loaded {
    let mut loaded = Loaded::default();
    for (index, record) in records.iter().enumerate() {
        match record.direction {
            Direction::ToPods => loaded.to_pods = loaded.to_pods.saturating_add(1),
            Direction::FromPods => loaded.take(index, record),
        }
    }
    loaded
}

fn read_audio(index: usize, record: &Record, audio: &uplink::AudioSdu<'_>) -> AudioPacket {
    let mut walk = audio.units();
    let units = walk
        .by_ref()
        .map(|unit| Unit {
            stamp: unit.stamp,
            len: unit.data.len(),
            max_sfb: max_sfb(unit.data),
        })
        .collect();
    AudioPacket {
        record: index,
        at_ns: record.at_ns,
        opaque: *audio.opaque,
        units,
        len: record.sdu.len(),
        // Read only after the walk above ended: before that it counts the units not yet read too.
        trailing: walk.trailing(),
    }
}

// Sample input: 04 00 04 00 1d 00 ..., whose opcode reads 0x001d; a packet under six bytes has none.
fn opcode(sdu: &[u8]) -> Option<u16> {
    let bytes = sdu.get(OPCODE_AT..)?.first_chunk::<OPCODE_LEN>()?;
    Some(u16::from_le_bytes(*bytes))
}

// Sample input: 04 00 04 00 1d 00 02 ed 00 04 00 "Name" 00 "Model" 00; the strings can name the device, so no table holds them.
pub(crate) fn device_strings(sdu: &[u8]) -> Vec<String> {
    sdu.split(|&byte| byte == STRING_END)
        .filter_map(|piece| std::str::from_utf8(piece).ok())
        .filter(|text| is_printable(text))
        .map(str::to_owned)
        .collect()
}

/// Text with no control character: a name need not be ASCII, and a string never breaks its line in the file.
fn is_printable(text: &str) -> bool {
    !text.is_empty() && !text.chars().any(char::is_control)
}

/// The time one START covers: from its mark to the next stop or start mark, or to the end of the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Window {
    pub(crate) start_ns: u64,
    /// `None` when no mark closes the window.
    pub(crate) end_ns: Option<u64>,
}

impl Window {
    /// Whether a packet stamped `at_ns` falls in the window; the end itself is outside.
    pub(crate) fn holds(self, at_ns: u64) -> bool {
        at_ns >= self.start_ns && self.end_ns.is_none_or(|end_ns| at_ns < end_ns)
    }
}

/// One window for each start mark, in order.
pub(crate) fn windows(marks: &[Mark]) -> Vec<Window> {
    let mut found = Vec::new();
    let mut open: Option<u64> = None;
    for mark in marks {
        let starts = match mark.kind {
            MarkKind::Start => true,
            MarkKind::Stop => false,
            MarkKind::Truncated | MarkKind::Other => continue,
        };
        if let Some(start_ns) = open.take() {
            let end_ns = Some(mark.at_ns);
            found.push(Window { start_ns, end_ns });
        }
        if starts {
            open = Some(mark.at_ns);
        }
    }
    if let Some(start_ns) = open {
        let end_ns = None;
        found.push(Window { start_ns, end_ns });
    }
    found
}

/// For each start mark, in order: the window from the stop mark that ends it to the next start mark, `None` when no stop mark ends it.
pub(crate) fn tails(marks: &[Mark]) -> Vec<Option<Window>> {
    let mut found: Vec<Option<Window>> = Vec::new();
    for mark in marks {
        match mark.kind {
            MarkKind::Start => {
                if let Some(Some(tail)) = found.last_mut() {
                    tail.end_ns = Some(mark.at_ns);
                }
                found.push(None);
            }
            MarkKind::Stop => {
                // A second stop mark leaves the tail where the first one opened it.
                if let Some(tail @ None) = found.last_mut() {
                    let (start_ns, end_ns) = (mark.at_ns, None);
                    *tail = Some(Window { start_ns, end_ns });
                }
            }
            MarkKind::Truncated | MarkKind::Other => {}
        }
    }
    found
}

/// The records stamped inside `window`: a run of neighbours, because a log is in stamp order.
pub(crate) fn segment(records: &[Record], window: Window) -> &[Record] {
    let from = records.partition_point(|record| record.at_ns < window.start_ns);
    let until = window.end_ns.map_or(records.len(), |end_ns| {
        records.partition_point(|record| record.at_ns < end_ns)
    });
    records.get(from..until).unwrap_or(&[])
}

#[cfg(test)]
mod tests;
