//! One connection of the named device in a trace: who began it, and when each step of it came.

use pods_proto::addr::BdAddr;
use pods_proto::packets::CONTROL_PSM;
use pods_proto::proximity::{APPLE_COMPANY_ID, MANUFACTURER_DATA, PROXIMITY_TYPE};
use pods_proto::sdulog::Direction;

use super::{MarksError, NANOS_PER_MICRO, Window};
use crate::probe::a2dp::{self, Packet, Role};
use crate::probe::hci::{
    self, Channels, HANDLE_MASK, HciRecord, Kind, OPCODE_ACL_RX, OPCODE_ACL_TX, OPCODE_COMMAND,
    OPCODE_EVENT, Raw, channels, take,
};

/// HCI Create Connection (Core 6.1 Vol 4 Part E 7.1.5).
const CREATE_CONNECTION: u16 = 0x0405;
/// HCI Connection Complete (Core 6.1 Vol 4 Part E 7.7.3).
const EVENT_CONNECTION_COMPLETE: u8 = 0x03;
/// HCI Connection Request (Core 6.1 Vol 4 Part E 7.7.4).
const EVENT_CONNECTION_REQUEST: u8 = 0x04;
/// HCI LE Meta, whose first parameter says which LE event it is (Core 6.1 Vol 4 Part E 7.7.65).
const EVENT_LE_META: u8 = 0x3e;
/// HCI LE Advertising Report (Core 6.1 Vol 4 Part E 7.7.65.2).
const LE_ADVERTISING_REPORT: u8 = 0x02;
/// HCI LE Extended Advertising Report (Core 6.1 Vol 4 Part E 7.7.65.13).
const LE_EXTENDED_ADVERTISING_REPORT: u8 = 0x0d;
/// Bytes before the data length and after the data in each report layout.
const LEGACY_REPORT: (usize, usize) = (8, 1);
const EXTENDED_REPORT: (usize, usize) = (23, 0);
/// The link type of an ACL link in a connection request and a connection complete (Core 6.1 Vol 4 Part E 7.7.3).
const LINK_TYPE_ACL: u8 = 0x01;
const STATUS_SUCCESS: u8 = 0x00;
/// The number `channels` gives the only link it is shown.
const ONLY_LINK: usize = 0;

/// A device address in the order a packet carries it, the reverse of how people read it.
type Wire = [u8; 6];

/// Who asked for the link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Began {
    /// The pods sent a connection request, which the host accepted.
    Pods,
    /// The host sent a Create Connection command.
    Host,
}

/// The marks of one connection; every time is in microseconds since 1970.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Connection {
    /// The first Apple proximity pairing advert the host heard in the window before the dial.
    pub(crate) advert_us: Option<u64>,
    /// When the ACL link came up.
    pub(crate) acl_us: u64,
    pub(crate) began: Began,
    /// AVDTP Set Configuration commands, from either side.
    pub(crate) set_configurations: u32,
    /// The first AVDTP Open command, from either side.
    pub(crate) open_us: Option<u64>,
    /// The first AVDTP Start command, from either side.
    pub(crate) start_us: Option<u64>,
    /// Each dial of the control channel by the host that the pods answered: when it was sent, and the result of the answer.
    pub(crate) control_dials: Vec<(u64, u16)>,
    /// The first media packet the host sent.
    pub(crate) media_us: Option<u64>,
    /// The link's recorded end.
    pub(crate) ended: Option<Disconnect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Disconnect {
    pub(crate) at_us: u64,
    pub(crate) reason: Option<u8>,
}

/// The address a Create Connection command asks for.
// Sample input: 05 04 0d e5 d4 c3 b2 a1 02 18 cc 01 00 00 00 01, opcode 0x0405 in little endian, parameter length 13, the address, then seven bytes of how to page.
fn created_for(command: &[u8]) -> Option<Wire> {
    let mut rest = command;
    let opcode = u16::from_le_bytes(take(&mut rest)?);
    let [_len] = take(&mut rest)?;
    let address = take(&mut rest)?;
    (opcode == CREATE_CONNECTION).then_some(address)
}

/// The address that asks for an ACL link.
// Sample input: e5 d4 c3 b2 a1 02 18 04 24 01, the address, three bytes of class, then the link type.
fn requested_by(parameters: &[u8]) -> Option<Wire> {
    let mut rest = parameters;
    let address = take(&mut rest)?;
    let [_class_low, _class_mid, _class_high, link_type] = take(&mut rest)?;
    (link_type == LINK_TYPE_ACL).then_some(address)
}

/// How a page for an ACL link ended: whether the link is up, its handle and the address.
// Sample input: 00 0b 00 e5 d4 c3 b2 a1 02 01 00, status, handle 0x000b in little endian, the address, the link type, encryption.
fn completed(parameters: &[u8]) -> Option<(bool, u16, Wire)> {
    let mut rest = parameters;
    let [status] = take(&mut rest)?;
    let handle = u16::from_le_bytes(take(&mut rest)?) & HANDLE_MASK;
    let address = take(&mut rest)?;
    let [link_type, _encryption] = take(&mut rest)?;
    (link_type == LINK_TYPE_ACL).then_some((status == STATUS_SUCCESS, handle, address))
}

/// Whether an LE Meta event reports an Apple proximity pairing advertisement.
// Sample input: 02 01 00 00 00 00 00 00 00 00 07 06 ff 4c 00 07 01 00 c4, a legacy subevent with one report.
fn advertised(parameters: &[u8]) -> bool {
    let mut rest = parameters;
    let Some([subevent, reports]) = take(&mut rest) else {
        return false;
    };
    let layout = match subevent {
        LE_ADVERTISING_REPORT => LEGACY_REPORT,
        LE_EXTENDED_ADVERTISING_REPORT => EXTENDED_REPORT,
        _ => return false,
    };
    // A report cut short ends the reading; the reports before it were whole.
    (0..reports)
        .map_while(|_| report_data(&mut rest, layout))
        .any(proximity_data)
}

/// Takes one report off `rest` and gives its advertising data.
// Sample input: 00 00 00 00 00 00 00 00 07 06 ff 4c 00 07 01 00 c4, legacy event type, address type, address, data length and data, then RSSI.
fn report_data<'a>(rest: &mut &'a [u8], (head, tail): (usize, usize)) -> Option<&'a [u8]> {
    let (&data_len, payload) = rest.get(head..)?.split_first()?;
    let (data, trailer) = payload.split_at_checked(usize::from(data_len))?;
    *rest = trailer.get(tail..)?;
    Some(data)
}

/// The AD elements each have a length byte followed by their type and contents.
// Sample input: 02 01 1a 06 ff 4c 00 07 01 00, flags and Apple manufacturer data of proximity pairing type.
fn proximity_data(mut data: &[u8]) -> bool {
    let [apple_low, apple_high] = APPLE_COMPANY_ID.to_le_bytes();
    let proximity = [MANUFACTURER_DATA, apple_low, apple_high, PROXIMITY_TYPE];
    while let Some((&len, element)) = data.split_first() {
        let Some((element, rest)) = element.split_at_checked(usize::from(len)) else {
            return false;
        };
        if element.starts_with(&proximity) {
            return true;
        }
        if element.is_empty() {
            break;
        }
        data = rest;
    }
    false
}

/// The connection handle of an ACL packet.
// Sample input: 0b 20 05 00, handle 0x00b under the boundary flag 0b10, then the length of the data.
fn acl_handle(packet: &[u8]) -> Option<u16> {
    let mut rest = packet;
    Some(u16::from_le_bytes(take(&mut rest)?) & HANDLE_MASK)
}

/// Who dialled and the first eligible advert heard by then.
#[derive(Debug)]
struct Dial {
    began: Began,
    advert_us: Option<u64>,
}

/// The link the window's connection is on.
#[derive(Debug)]
struct Link {
    handle: u16,
    acl_us: u64,
    /// `None` when the trace holds neither the request nor the command before the link.
    dial: Option<Dial>,
    /// Its ACL packets until it ended, after which the handle may be another link's.
    packets: Vec<HciRecord>,
    ended: Option<Disconnect>,
}

/// The walk over the records of a trace.
#[derive(Debug)]
struct Reader<'a> {
    address: Wire,
    window: &'a Window,
    /// Who last asked for a link with the device, until a connection complete answers.
    asked: Option<Dial>,
    /// The first Apple proximity pairing advertisement heard in the window.
    advert_us: Option<u64>,
    link: Option<Link>,
}

impl Reader<'_> {
    fn record(&mut self, raw: &Raw<'_>) {
        match raw.opcode {
            OPCODE_COMMAND => {
                if created_for(raw.data) == Some(self.address) {
                    self.dial(Began::Host);
                }
            }
            OPCODE_EVENT => self.event(raw.at_ns / NANOS_PER_MICRO, raw.data),
            OPCODE_ACL_TX => self.acl(raw, Kind::AclSent),
            OPCODE_ACL_RX => self.acl(raw, Kind::AclReceived),
            _ => {}
        }
    }

    fn event(&mut self, at_us: u64, packet: &[u8]) {
        let Some((code, parameters)) = hci::event_parts(packet) else {
            return;
        };
        match code {
            EVENT_CONNECTION_REQUEST => {
                if requested_by(parameters) == Some(self.address) {
                    self.dial(Began::Pods);
                }
            }
            EVENT_CONNECTION_COMPLETE => self.completed(at_us, parameters),
            EVENT_LE_META => {
                // corner: one pair of pods in range, so a matching advert is attributed to it despite its rotating address.
                if self.advert_us.is_none() && self.window.holds(at_us) && advertised(parameters) {
                    self.advert_us = Some(at_us);
                }
            }
            _ => {
                if let Some(Kind::Disconnection { handle, reason }) =
                    hci::event_kind(code, parameters)
                {
                    self.disconnected(handle, Disconnect { at_us, reason });
                }
            }
        }
    }

    fn dial(&mut self, began: Began) {
        self.asked = Some(Dial {
            began,
            advert_us: self.advert_us,
        });
    }

    /// The first link with the device that comes up inside the window is the window's connection.
    fn completed(&mut self, at_us: u64, parameters: &[u8]) {
        let Some((up, handle, _)) = completed(parameters).filter(|page| page.2 == self.address)
        else {
            return;
        };
        let dial = self.asked.take();
        if up && self.link.is_none() && self.window.holds(at_us) {
            self.link = Some(Link {
                handle,
                acl_us: at_us,
                dial,
                packets: Vec::new(),
                ended: None,
            });
        }
    }

    fn disconnected(&mut self, handle: u16, end: Disconnect) {
        let up = self.link.as_mut().filter(|link| link.ended.is_none());
        if let Some(link) = up
            && link.handle == handle
        {
            link.ended = Some(end);
        }
    }

    /// Only a packet on the link's own handle, while the link is up, is the connection's.
    fn acl(&mut self, raw: &Raw<'_>, kind: Kind) {
        let up = self.link.as_mut().filter(|link| link.ended.is_none());
        if let Some(link) = up
            && acl_handle(raw.data) == Some(link.handle)
        {
            link.packets.push(HciRecord {
                at_ns: raw.at_ns,
                kind,
                data: raw.data.to_vec(),
            });
        }
    }
}

/// Fills in what the AVDTP channels of the link say of its stream.
fn stream(channels: &Channels, marks: &mut Connection) {
    // The channels come in the order they opened, so the first mark kept of each kind is the earliest.
    for (role, channel) in a2dp::roles(channels, ONLY_LINK) {
        for sdu in &channel.sdus {
            let at_us = sdu.at_ns / NANOS_PER_MICRO;
            match role {
                Role::Signalling => {
                    // corner: a signal cut into fragments is not read; these commands are a few bytes and fit one packet.
                    let Packet::Single(signal) = a2dp::packet(&sdu.sdu) else {
                        continue;
                    };
                    match (signal.message, signal.id) {
                        (a2dp::COMMAND, a2dp::SET_CONFIGURATION) => {
                            marks.set_configurations = marks.set_configurations.saturating_add(1);
                        }
                        (a2dp::COMMAND, a2dp::OPEN) => {
                            marks.open_us = marks.open_us.or(Some(at_us));
                        }
                        (a2dp::COMMAND, a2dp::START) => {
                            marks.start_us = marks.start_us.or(Some(at_us));
                        }
                        _ => {}
                    }
                }
                Role::Media if sdu.direction == Direction::ToPods => {
                    marks.media_us = marks.media_us.or(Some(at_us));
                }
                Role::Media | Role::Other => {}
            }
        }
    }
}

/// The first connection of `device` that comes up inside `window`, or `None` when the trace holds none.
pub(crate) fn connection(
    trace: &[u8],
    device: BdAddr,
    window: &Window,
) -> Result<Option<Connection>, MarksError> {
    let mut address = device.octets();
    address.reverse();
    let mut reader = Reader {
        address,
        window,
        asked: None,
        advert_us: None,
        link: None,
    };
    // A trace cut short is read up to the cut, which the command names once for the whole table.
    let _cut = hci::walk(trace, |raw| reader.record(&raw))?;
    let Some(link) = reader.link else {
        return Ok(None);
    };
    let dial = link.dial.ok_or(MarksError::NoBeginning {
        trial: window.trial,
    })?;
    let channels = channels(&link.packets);
    let dialed = |request: &&hci::Request| {
        request.psm == CONTROL_PSM && request.direction == Direction::ToPods
    };
    // corner: a dial the pods never answered has no result code and is not listed.
    let answered =
        |request: &hci::Request| Some((request.at_ns / NANOS_PER_MICRO, request.result?));
    let dials = channels.requests.iter().filter(dialed);
    let mut marks = Connection {
        advert_us: dial.advert_us,
        acl_us: link.acl_us,
        began: dial.began,
        set_configurations: 0,
        open_us: None,
        start_us: None,
        control_dials: dials.filter_map(answered).collect(),
        media_us: None,
        ended: link.ended,
    };
    stream(&channels, &mut marks);
    Ok(Some(marks))
}
