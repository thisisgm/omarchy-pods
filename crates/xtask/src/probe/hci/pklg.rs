//! Reads a trace Apple's `PacketLogger` wrote: records one after another, with no file header.

use std::fmt;

use pods_proto::sdulog::Direction;

use super::{HciRecord, Kind, NANOS_PER_MICRO, TraceError, event, keep, take};

/// What the length field counts besides the payload: seconds, microseconds and the type (Wireshark wiretap/packetlogger.c, `packetlogger_header_t` and the type byte behind it).
const COUNTED_LEN: u32 = 9;
/// The length field, the stamp and the type in front of every payload.
const HEADER_LEN: usize = 13;
/// An HCI event (Wireshark wiretap/packetlogger.c, `PKT_HCI_EVENT`).
const TYPE_EVENT: u8 = 0x01;
/// ACL data the host sent (Wireshark wiretap/packetlogger.c, `PKT_SENT_ACL_DATA`).
const TYPE_ACL_SENT: u8 = 0x02;
/// ACL data the host received (Wireshark wiretap/packetlogger.c, `PKT_RECV_ACL_DATA`).
const TYPE_ACL_RECEIVED: u8 = 0x03;
/// Synchronous data the host sent, with the audio left out; seen in the owner's macOS 26 traces, and not 0x08, the type Wireshark knows.
const TYPE_SCO_SENT: u8 = 0x18;
/// Synchronous data the host received, with the audio left out; not 0x09, the type Wireshark knows.
const TYPE_SCO_RECEIVED: u8 = 0x19;
/// Nanoseconds in one second.
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// What is wrong with the record a `PacketLogger` trace is refused at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fault {
    /// Fewer bytes are left than a length, a stamp and a type take.
    Residue,
    /// Its length field is smaller than the stamp and the type it counts.
    LengthTooSmall,
    /// The file ends inside its payload.
    CutShort,
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Residue => write!(
                f,
                "is residue: fewer bytes than the {HEADER_LEN} of a length, a stamp and a type"
            ),
            Self::LengthTooSmall => write!(
                f,
                "claims a length under the {COUNTED_LEN} bytes of its stamp and its type"
            ),
            Self::CutShort => f.write_str("is cut short"),
        }
    }
}

/// The fields in front of one payload.
struct Header {
    /// How many bytes follow the length field.
    counted: u32,
    seconds: u32,
    micros: u32,
    kind: u8,
}

// Sample input: `0e 00 00 00`, `40 36 00 6b`, `a0 0f 00 00`, `03`: 14 bytes follow the length, 1795176000 s and 4000 us since 1970, ACL received; the integers are little endian.
fn header(rest: &mut &[u8]) -> Option<Header> {
    let counted = u32::from_le_bytes(take(rest)?);
    let seconds = u32::from_le_bytes(take(rest)?);
    let micros = u32::from_le_bytes(take(rest)?);
    let [kind] = take(rest)?;
    Some(Header {
        counted,
        seconds,
        micros,
        kind,
    })
}

// Sample input: `0e 00 00 00 40 36 00 6b a0 0f 00 00 03`, then `0b 20 01 00 aa`: one record whose payload is the 5 bytes of an ACL packet; a trace is such records to its last byte.
pub(crate) fn records_pklg(bytes: &[u8]) -> Result<Vec<HciRecord>, TraceError> {
    if bytes.is_empty() {
        return Err(TraceError::NoRecord);
    }
    let mut kept = Vec::new();
    let mut rest = bytes;
    let mut record = 0_usize;
    while !rest.is_empty() {
        let offset = bytes.len().saturating_sub(rest.len());
        let refused = |fault| TraceError::PacketLogger {
            record,
            offset,
            fault,
        };
        let header = header(&mut rest).ok_or_else(|| refused(Fault::Residue))?;
        let payload_len = header.counted.checked_sub(COUNTED_LEN);
        let payload_len = payload_len.ok_or_else(|| refused(Fault::LengthTooSmall))?;
        // The payload is only ever a slice of the file, so no length the record claims is allocated.
        let split = usize::try_from(payload_len)
            .ok()
            .and_then(|len| rest.split_at_checked(len));
        let (payload, after) = split.ok_or_else(|| refused(Fault::CutShort))?;
        rest = after;
        let at_ns = u64::from(header.seconds)
            .saturating_mul(NANOS_PER_SECOND)
            .saturating_add(u64::from(header.micros).saturating_mul(NANOS_PER_MICRO));
        let kind = match header.kind {
            TYPE_EVENT => event(payload),
            TYPE_ACL_SENT => Some(Kind::AclSent),
            TYPE_ACL_RECEIVED => Some(Kind::AclReceived),
            TYPE_SCO_SENT => Some(Kind::SynchronousData(Direction::ToPods)),
            TYPE_SCO_RECEIVED => Some(Kind::SynchronousData(Direction::FromPods)),
            _ => None,
        };
        keep(&mut kept, at_ns, kind, payload);
        record = record.saturating_add(1);
    }
    Ok(kept)
}

#[cfg(test)]
mod tests;
