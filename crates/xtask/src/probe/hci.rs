//! Reads a Bluetooth trace written by `btmon -w`: the btsnoop file here and, in `l2cap`, the channels inside it.

mod export;
#[cfg(test)]
pub(crate) mod fixture;
mod l2cap;

pub(crate) use export::export;
pub(crate) use l2cap::{Channel, Channels, channels};

use std::fmt;

use pods_proto::sdulog::Direction;

/// The identification pattern that opens a btsnoop file (`BlueZ` src/shared/btsnoop.c, `btsnoop_id`).
const MAGIC: [u8; 8] = *b"btsnoop\0";
/// File header: the identification pattern, the version and the datalink type (`BlueZ` src/shared/btsnoop.c, struct `btsnoop_hdr`).
const FILE_HEADER_LEN: usize = 16;
/// The only btsnoop version there is (`BlueZ` src/shared/btsnoop.c, `btsnoop_version`).
const VERSION: u32 = 1;
/// The datalink type of a trace `btmon -w` writes (`BlueZ` src/shared/btsnoop.h, `BTSNOOP_FORMAT_MONITOR`).
const MONITOR_DATALINK: u32 = 2001;
/// The most data one record holds: `btsnoop_write` in `BlueZ` takes the size as a 16 bit number.
const MAX_RECORD_LEN: u32 = 65_535;
/// Microseconds from the btsnoop epoch, midnight of year 0, to 1970 (`BlueZ` src/shared/btsnoop.c: 0x00E03AB44A676000 less 946684800 seconds).
const YEAR_0_TO_1970_MICROS: u64 = 62_168_256_000_000_000;
/// Nanoseconds in one microsecond.
const NANOS_PER_MICRO: u64 = 1_000;
/// Monitor opcode of an HCI event (`BlueZ` src/shared/btsnoop.h, `BTSNOOP_OPCODE_EVENT_PKT`).
const OPCODE_EVENT: u16 = 3;
/// Monitor opcode of ACL data the host sent (`BlueZ` src/shared/btsnoop.h, `BTSNOOP_OPCODE_ACL_TX_PKT`).
const OPCODE_ACL_TX: u16 = 4;
/// Monitor opcode of ACL data the host received (`BlueZ` src/shared/btsnoop.h, `BTSNOOP_OPCODE_ACL_RX_PKT`).
const OPCODE_ACL_RX: u16 = 5;
/// HCI Disconnection Complete (`BlueZ` monitor/bt.h, `BT_HCI_EVT_DISCONNECT_COMPLETE`).
const EVENT_DISCONNECTION_COMPLETE: u8 = 0x05;
/// HCI Synchronous Connection Complete (`BlueZ` monitor/bt.h, `BT_HCI_EVT_SYNC_CONN_COMPLETE`).
const EVENT_SYNCHRONOUS_CONNECTION_COMPLETE: u8 = 0x2c;
/// The status of an event that reports success.
const STATUS_SUCCESS: u8 = 0x00;
/// A connection handle is the low 12 bits of the field that holds it (`BlueZ` lib/bluetooth/hci.h, `acl_handle`).
pub(crate) const HANDLE_MASK: u16 = 0x0fff;

/// What one kept record is; every other record of a trace is skipped and none of its bytes is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// An ACL data packet the host sent; `data` is the packet.
    AclSent,
    /// An ACL data packet the host received; `data` is the packet.
    AclReceived,
    /// A synchronous (SCO or eSCO) connection completed; `opened` is false when the controller reported a failure.
    SynchronousConnection { opened: bool },
    /// The link with this connection handle ended.
    Disconnection { handle: u16 },
}

/// One record of a trace that the tables need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HciRecord {
    /// When the monitor saw it, in nanoseconds since 1970.
    pub at_ns: u64,
    pub kind: Kind,
    /// The ACL data packet for the two ACL kinds, empty for the others.
    pub data: Vec<u8>,
}

/// Why a trace was refused: a rule its file header broke; none holds a value read from the trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum TraceError {
    #[error("btsnoop trace is {len} bytes, shorter than its {FILE_HEADER_LEN} byte file header")]
    NoFileHeader { len: usize },
    #[error("btsnoop trace does not start with the btsnoop identification pattern")]
    BadMagic,
    #[error("btsnoop trace has a version other than {VERSION}, the only one that exists")]
    BadVersion,
    #[error(
        "btsnoop trace has a datalink type other than the monitor datalink {MONITOR_DATALINK}, which btmon -w writes"
    )]
    UnknownDatalink,
}

/// What is wrong with the record a trace stops being read at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Damage {
    /// The file ends inside it, as it does when the recorder was stopped.
    CutShort,
    /// It claims more data than a record holds, so nothing says where the next record starts.
    TooLong,
    /// Its timestamp lies outside the years 1970 to 2554, which is all a nanosecond stamp holds.
    StampOutOfRange,
}

impl Damage {
    /// The key `links.txt` states it under.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::CutShort => "trace_cut_short",
            Self::TooLong => "trace_record_too_long",
            Self::StampOutOfRange => "trace_stamp_out_of_range",
        }
    }
}

impl fmt::Display for Damage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CutShort => f.write_str("is cut short"),
            Self::TooLong => write!(
                f,
                "claims more data than the {MAX_RECORD_LEN} bytes a record holds"
            ),
            Self::StampOutOfRange => f.write_str("has a timestamp outside the years 1970 to 2554"),
        }
    }
}

/// Where a trace stops being read; every record before it is whole and is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "btsnoop record {record} at byte offset {offset} {damage}, so the trace is read only up to the whole records before it"
)]
pub(crate) struct Cut {
    /// The index of the damaged record, which is also how many whole records come before it.
    pub record: usize,
    /// The byte offset the damaged record starts at.
    pub offset: usize,
    pub damage: Damage,
}

/// What `records` read from a trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Records {
    /// The records the tables need, in trace order.
    pub kept: Vec<HciRecord>,
    /// Set when a damaged record ends the reading before the end of the file.
    pub cut: Option<Cut>,
}

/// Takes the next `N` bytes off the front of `rest`; `None`, with nothing taken, when fewer are left.
pub(crate) fn take<const N: usize>(rest: &mut &[u8]) -> Option<[u8; N]> {
    let (head, tail) = rest.split_first_chunk::<N>()?;
    *rest = tail;
    Some(*head)
}

/// The fields of one record header that the reader uses.
struct RecordHeader {
    /// How many data bytes follow the header.
    included: u32,
    /// The low 16 bits of the flags, which the monitor datalink uses for what the record is.
    opcode: u16,
    /// Microseconds since midnight of year 0.
    micros: u64,
}

// Sample input: 00 00 00 07, 00 00 00 07, 00 00 00 04, 00 00 00 00, 00 e3 3e 67 0a f9 90 00, the 24 bytes of struct `btsnoop_pkt` in `BlueZ` src/shared/btsnoop.c, all big endian: 7 data bytes, adapter 0, ACL sent.
fn record_header(rest: &mut &[u8]) -> Option<RecordHeader> {
    let _original_len: [u8; 4] = take(rest)?;
    let included = u32::from_be_bytes(take(rest)?);
    // corner: one adapter; its index is not read, so equal connection handles on two adapters would be mixed.
    let _adapter: [u8; 2] = take(rest)?;
    let opcode = u16::from_be_bytes(take(rest)?);
    let _drops: [u8; 4] = take(rest)?;
    let micros = u64::from_be_bytes(take(rest)?);
    Some(RecordHeader {
        included,
        opcode,
        micros,
    })
}

// Sample input: 05 04 00 0b 00 13, a Disconnection Complete: code, parameter length 4, status, handle 0x000b in little endian, reason.
fn event(packet: &[u8]) -> Option<Kind> {
    let mut rest = packet;
    let [code, len] = take(&mut rest)?;
    let mut parameters = rest.get(..usize::from(len))?;
    let [status] = take(&mut parameters)?;
    match code {
        EVENT_DISCONNECTION_COMPLETE => {
            let handle = u16::from_le_bytes(take(&mut parameters)?) & HANDLE_MASK;
            (status == STATUS_SUCCESS).then_some(Kind::Disconnection { handle })
        }
        EVENT_SYNCHRONOUS_CONNECTION_COMPLETE => Some(Kind::SynchronousConnection {
            opened: status == STATUS_SUCCESS,
        }),
        _ => None,
    }
}

/// Which side sent an ACL record.
pub(crate) fn sender(kind: Kind) -> Option<Direction> {
    match kind {
        Kind::AclSent => Some(Direction::ToPods),
        Kind::AclReceived => Some(Direction::FromPods),
        Kind::SynchronousConnection { .. } | Kind::Disconnection { .. } => None,
    }
}

// Sample input: 62 74 73 6e 6f 6f 70 00, 00 00 00 01, 00 00 07 d1, then records: a 24 byte header and as many data bytes as it claims.
pub(crate) fn records(trace: &[u8]) -> Result<Records, TraceError> {
    let mut rest = trace;
    let header = (
        take::<8>(&mut rest),
        take::<4>(&mut rest),
        take::<4>(&mut rest),
    );
    let (Some(magic), Some(version), Some(datalink)) = header else {
        return Err(TraceError::NoFileHeader { len: trace.len() });
    };
    if magic != MAGIC {
        return Err(TraceError::BadMagic);
    }
    if u32::from_be_bytes(version) != VERSION {
        return Err(TraceError::BadVersion);
    }
    if u32::from_be_bytes(datalink) != MONITOR_DATALINK {
        return Err(TraceError::UnknownDatalink);
    }
    let mut kept = Vec::new();
    let mut cut = None;
    let mut record = 0_usize;
    while !rest.is_empty() {
        let offset = trace.len().saturating_sub(rest.len());
        let stop = |damage| {
            Some(Cut {
                record,
                offset,
                damage,
            })
        };
        let Some(header) = record_header(&mut rest) else {
            cut = stop(Damage::CutShort);
            break;
        };
        let len = usize::try_from(header.included).ok();
        let Some(len) = len.filter(|_| header.included <= MAX_RECORD_LEN) else {
            cut = stop(Damage::TooLong);
            break;
        };
        let Some((data, after)) = rest.split_at_checked(len) else {
            cut = stop(Damage::CutShort);
            break;
        };
        rest = after;
        let stamp = header
            .micros
            .checked_sub(YEAR_0_TO_1970_MICROS)
            .and_then(|micros| micros.checked_mul(NANOS_PER_MICRO));
        let Some(at_ns) = stamp else {
            cut = stop(Damage::StampOutOfRange);
            break;
        };
        let kind = match header.opcode {
            OPCODE_ACL_TX => Some(Kind::AclSent),
            OPCODE_ACL_RX => Some(Kind::AclReceived),
            OPCODE_EVENT => event(data),
            _ => None,
        };
        if let Some(kind) = kind {
            // An event is kept as its kind alone; only an ACL packet keeps bytes.
            let data = sender(kind).map_or_else(Vec::new, |_| data.to_vec());
            kept.push(HciRecord { at_ns, kind, data });
        }
        record = record.saturating_add(1);
    }
    Ok(Records { kept, cut })
}

#[cfg(test)]
mod tests;
