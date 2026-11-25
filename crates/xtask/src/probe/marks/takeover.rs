//! The audio source and stream marks of a takeover, without device identifiers.

use std::path::PathBuf;

use clap::Args;
use pods_proto::addr::BdAddr;
use pods_proto::error::HEADER_LEN;
use pods_proto::packet::opcode_of;
use pods_proto::sdulog::Direction;
use pods_proto::uplink::state::STATE_OPCODE;

use super::connect::{Began, Disconnect, ONLY_LINK, takeover_channels};
use super::{MarksError, NANOS_PER_MICRO, Window};
use crate::probe::a2dp::{self, Packet, Role};

mod table;

pub(super) use table::table;

#[derive(Debug, Args)]
pub(super) struct TakeoverArgs {
    /// The private btsnoop trace for trial and stream marks.
    #[arg(long, requires_all = ["device_file", "trials"])]
    trace: Option<PathBuf>,
    /// The mode 0600 file for selecting the named device in the trace.
    #[arg(long, requires = "trace")]
    device_file: Option<PathBuf>,
    /// Trial starts in the same format as probe marks.
    #[arg(long, requires = "trace")]
    trials: Option<PathBuf>,
    /// Optional daemon journal in journalctl -o json format.
    #[arg(long, requires = "trace")]
    journal: Option<PathBuf>,
    /// Optional timestamped pactl list short sinks snapshots.
    #[arg(long, requires = "trace")]
    sinks: Option<PathBuf>,
    /// The private probe SDU log. Times are relative to the log without a trace. Joining trials reads meta.txt beside this file.
    #[arg(long, required_unless_present = "trace", requires = "host_file")]
    sdus: Option<PathBuf>,
    /// The mode 0600 file for identifying the host in source reports.
    #[arg(long, requires = "sdus")]
    host_file: Option<PathBuf>,
}

/// AACP Connected Devices, whose first payload byte reports the entry count.
const CONNECTED_DEVICES_OPCODE: u16 = 0x002e;
/// AVDTP Close (AVDTP 1.3 section 8.5, Table 8.6).
const CLOSE: u8 = 0x08;
/// AVDTP Suspend (AVDTP 1.3 section 8.5, Table 8.6).
const SUSPEND: u8 = 0x09;
/// AVDTP Response Accept (AVDTP 1.3 section 8.4.3, Table 8.5).
const RESPONSE_ACCEPT: u8 = 0b10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PeerClass {
    Host,
    Other,
    Zero,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Order {
    /// The order HCI uses on the wire.
    Wire,
    /// The octet order of the address as written.
    Reversed,
    Neither,
}

#[derive(Debug)]
pub(crate) struct SourceRow {
    pub(crate) at_us: u64,
    pub(crate) class: PeerClass,
    pub(crate) order: Order,
    pub(crate) kind: u8,
}

// Sample input: `04 00 04 00 0e 00 <six peer bytes> 22`; the final byte is kept whole.
pub(crate) fn sources(sdus: &[(u64, &[u8])], host: BdAddr) -> Vec<SourceRow> {
    let mut wire = host.octets();
    wire.reverse();
    let wire = BdAddr::from(wire);
    let zero = BdAddr::from([0; 6]);

    sdus.iter()
        .filter_map(|&(at_us, sdu)| {
            if opcode_of(sdu) != Some(STATE_OPCODE) {
                return None;
            }
            let (&kind, octets) = sdu.get(HEADER_LEN..)?.split_last()?;
            let octets: [u8; 6] = octets.try_into().ok()?;
            let peer = BdAddr::from(octets);
            let (class, order) = if peer == zero {
                (PeerClass::Zero, Order::Neither)
            } else if peer == wire {
                (PeerClass::Host, Order::Wire)
            } else if peer == host {
                (PeerClass::Host, Order::Reversed)
            } else {
                (PeerClass::Other, Order::Neither)
            };
            Some(SourceRow {
                at_us,
                class,
                order,
                kind,
            })
        })
        .collect()
}

// Sample input: `04 00 04 00 2e 00 00`, an empty connected-devices list; a missing count yields None.
pub(crate) fn connected_devices(sdus: &[(u64, &[u8])]) -> Vec<(u64, usize, Option<u8>)> {
    sdus.iter()
        .filter(|(_, sdu)| opcode_of(sdu) == Some(CONNECTED_DEVICES_OPCODE))
        .map(|&(at_us, sdu)| (at_us, sdu.len(), sdu.get(HEADER_LEN).copied()))
        .collect()
}

/// The first marks of the named ACL in the supplied window, in wall-clock microseconds.
#[derive(Debug)]
struct Stream {
    suspend_us: Option<(u64, Began)>,
    close_us: Option<(u64, Began)>,
    restart_us: Option<u64>,
    ended: Option<Disconnect>,
    lifetime: Window,
}

// Sample input: a signalling SDU `10 09 04` is a Suspend command for the first stream endpoint; `12 07` accepts a Start.
fn stream(
    trace: &[u8],
    device: BdAddr,
    window: &Window,
    handback_us: Option<u64>,
) -> Result<Option<Stream>, MarksError> {
    let Some(link) = takeover_channels(trace, device, window)? else {
        return Ok(None);
    };
    let mut marks = Stream {
        suspend_us: None,
        close_us: None,
        restart_us: None,
        ended: link.ended,
        lifetime: link.lifetime,
    };
    for (_, channel) in a2dp::roles(&link.channels, ONLY_LINK)
        .into_iter()
        .filter(|(role, _)| *role == Role::Signalling)
    {
        for sdu in &channel.sdus {
            let at_us = sdu.at_ns / NANOS_PER_MICRO;
            if !window.holds(at_us) {
                continue;
            }
            let Packet::Single(signal) = a2dp::packet(&sdu.sdu) else {
                continue;
            };
            let sender = match sdu.direction {
                Direction::ToPods => Began::Host,
                Direction::FromPods => Began::Pods,
            };
            match (signal.message, signal.id, sender) {
                (a2dp::COMMAND, SUSPEND, _) => {
                    marks.suspend_us = marks.suspend_us.or(Some((at_us, sender)));
                }
                (a2dp::COMMAND, CLOSE, _) => {
                    marks.close_us = marks.close_us.or(Some((at_us, sender)));
                }
                (RESPONSE_ACCEPT, a2dp::START, Began::Pods)
                    if handback_us.is_none_or(|from| at_us >= from) =>
                {
                    marks.restart_us = marks.restart_us.or(Some(at_us));
                }
                _ => {}
            }
        }
    }
    Ok(Some(marks))
}
