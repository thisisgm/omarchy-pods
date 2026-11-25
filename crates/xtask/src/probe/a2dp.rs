//! The music stream inside a trace: the codecs AVDTP negotiated and, second by second, the media packets the host sent.

use std::collections::{BTreeMap, BTreeSet};

use pods_proto::sdulog::Direction;

use super::hci::{Channel, Channels, take};

/// The PSM of AVDTP (Bluetooth Assigned Numbers, Protocol and Service Multiplexers).
pub(crate) const AVDTP_PSM: u16 = 0x0019;
/// The transaction label is the top four bits of a signal's first byte (AVDTP 1.3 section 8.4, Table 8.1).
const LABEL_SHIFT: u32 = 4;
/// The packet type is the two bits below the label (AVDTP 1.3 section 8.4, Table 8.1).
const PACKET_TYPE_SHIFT: u32 = 2;
/// The packet type and the message type are two bits each.
const TWO_BITS: u8 = 0b11;
/// A signal that fits one packet; the other packet types are fragments (AVDTP 1.3 section 8.4.2, Table 8.4).
const SINGLE_PACKET: u8 = 0b00;
/// Message type of a command (AVDTP 1.3 section 8.4.3, Table 8.5).
pub(crate) const COMMAND: u8 = 0b00;
/// Message type of an accepted response (AVDTP 1.3 section 8.4.3, Table 8.5).
const RESPONSE_ACCEPT: u8 = 0b10;
/// Message type of a rejected response (AVDTP 1.3 section 8.4.3, Table 8.5).
const RESPONSE_REJECT: u8 = 0b11;
/// The signal identifier is the low six bits of a single packet's second byte (AVDTP 1.3 section 8.4, Table 8.1).
const SIGNAL_ID_MASK: u8 = 0b0011_1111;
/// `AVDTP_GET_CAPABILITIES` (AVDTP 1.3 section 8.5, Table 8.6).
const GET_CAPABILITIES: u8 = 0x02;
/// `AVDTP_SET_CONFIGURATION` (AVDTP 1.3 section 8.5, Table 8.6).
pub(crate) const SET_CONFIGURATION: u8 = 0x03;
/// `AVDTP_OPEN` (AVDTP 1.3 section 8.5, Table 8.6).
pub(crate) const OPEN: u8 = 0x06;
/// `AVDTP_START` (AVDTP 1.3 section 8.5, Table 8.6).
pub(crate) const START: u8 = 0x07;
/// `AVDTP_SUSPEND` (AVDTP 1.3 section 8.5, Table 8.6).
const SUSPEND: u8 = 0x09;
/// `AVDTP_GET_ALL_CAPABILITIES` (AVDTP 1.3 section 8.5, Table 8.6).
const GET_ALL_CAPABILITIES: u8 = 0x0c;
/// The Media Codec service category (AVDTP 1.3 section 8.21.1, Table 8.47).
const MEDIA_CODEC: u8 = 0x07;
/// The Media Codec Type of a vendor's codec (A2DP 1.3 section 4.7).
pub(crate) const VENDOR_CODEC: u8 = 0xff;
/// The RTP version is the top two bits of a packet's first byte (RFC 3550 section 5.1).
const RTP_VERSION_SHIFT: u32 = 6;
/// The RTP version every media packet carries (RFC 3550 section 5.1).
const RTP_VERSION: u8 = 2;
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Which codec a Media Codec capability names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodecKind {
    /// A codec of the Bluetooth Assigned Numbers list, by its Media Codec Type.
    Standard(u8),
    /// A vendor's codec: the vendor's company identifier and the vendor's own codec number.
    Vendor { vendor_id: u32, codec_id: u16 },
}

/// One Media Codec capability or configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Codec {
    pub kind: CodecKind,
    /// The codec specific information elements as sent, a vendor's identifiers included.
    pub bytes: Vec<u8>,
}

/// One second of the music stream; a second with no media packet and no signal has no row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Second {
    /// Whole seconds on the clock of the records.
    pub at_s: u64,
    /// Media packets the host sent.
    pub packets: u64,
    /// Their bytes, RTP header included.
    pub bytes: u64,
    /// Packets whose RTP sequence number does not follow the one before on the same channel, unless a start command came between them.
    pub gaps: u64,
    /// The longest wait between a media packet of this second and the media packet before it.
    pub largest_interval_ns: u64,
    /// `AVDTP_SUSPEND` commands, from either side.
    pub suspends: u64,
    /// `AVDTP_START` commands, from either side.
    pub starts: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Music {
    /// Every Media Codec capability the pods returned to Get Capabilities and Get All Capabilities, in trace order.
    pub capabilities: Vec<Codec>,
    /// The codec of the last Set Configuration command that the other side accepted.
    pub configuration: Option<Codec>,
    /// One row for each second that holds a media packet from the host or a start or suspend command.
    pub seconds: Vec<Second>,
    pub unread: Unread,
}

/// What the music tables could not read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Unread {
    /// Signalling packets that are a fragment of a longer signal, which is not joined.
    pub fragmented_signals: u64,
    /// Signals shorter than their header, and capability lists or codec elements cut short.
    pub malformed_signals: u64,
    /// Packets the host sent on a media channel that open with no RTP version 2 header.
    pub not_rtp: u64,
    /// AVDTP channels opened after the signalling and the media channel, for reporting or recovery.
    pub other_channels: u64,
}

/// What an AVDTP channel carries, told by the order the channels of a link open in (AVDTP 1.3 section 5.4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Signalling,
    Media,
    /// A reporting or recovery channel, which is not read.
    Other,
}

/// One signal that fits a single packet.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Signal<'a> {
    label: u8,
    pub(crate) message: u8,
    pub(crate) id: u8,
    body: &'a [u8],
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Packet<'a> {
    Single(Signal<'a>),
    Fragment,
    Malformed,
}

/// A Set Configuration command waiting for its response.
#[derive(Debug)]
struct Proposal {
    label: u8,
    codec: Option<Codec>,
}

/// Each side's Set Configuration command waiting for the other's response; both may share one label.
#[derive(Debug, Default)]
struct Proposals {
    from_host: Option<Proposal>,
    from_pods: Option<Proposal>,
}

impl Proposals {
    fn of(&mut self, sender: Direction) -> &mut Option<Proposal> {
        match sender {
            Direction::ToPods => &mut self.from_host,
            Direction::FromPods => &mut self.from_pods,
        }
    }
}

/// One media packet the host sent.
#[derive(Debug, Clone, Copy)]
struct MediaPacket {
    at_ns: u64,
    len: u64,
    /// Its sequence number does not follow the one before it on its channel.
    gap: bool,
}

#[derive(Debug, Default)]
struct Tables {
    capabilities: Vec<Codec>,
    configuration: Option<Codec>,
    seconds: BTreeMap<u64, Second>,
    packets: Vec<MediaPacket>,
    /// The stamp of every start command, after which a source may begin a new RTP sequence; ordered, so no lookup walks them all.
    starts: BTreeSet<u64>,
    unread: Unread,
}

fn bump(count: &mut u64) {
    *count = count.saturating_add(1);
}

// Sample input: 12 02 01 00, label 1, a single packet, response accept; then AVDTP_GET_CAPABILITIES and the body.
pub(crate) fn packet(bytes: &[u8]) -> Packet<'_> {
    let mut rest = bytes;
    let Some([first]) = take(&mut rest) else {
        return Packet::Malformed;
    };
    if (first >> PACKET_TYPE_SHIFT) & TWO_BITS != SINGLE_PACKET {
        return Packet::Fragment;
    }
    let Some([second]) = take(&mut rest) else {
        return Packet::Malformed;
    };
    Packet::Single(Signal {
        label: first >> LABEL_SHIFT,
        message: first & TWO_BITS,
        id: second & SIGNAL_ID_MASK,
        body: rest,
    })
}

// Sample input: 00 ff 4c 00 00 00 01 80 aa, audio, a vendor codec: vendor 0x0000004c, codec 0x8001, both little endian, one vendor byte.
fn codec(element: &[u8]) -> Option<Codec> {
    let mut rest = element;
    let [_media_type, codec_type] = take(&mut rest)?;
    let kind = if codec_type == VENDOR_CODEC {
        let mut identifiers = rest;
        let vendor_id = u32::from_le_bytes(take(&mut identifiers)?);
        let codec_id = u16::from_le_bytes(take(&mut identifiers)?);
        CodecKind::Vendor {
            vendor_id,
            codec_id,
        }
    } else {
        CodecKind::Standard(codec_type)
    };
    Some(Codec {
        kind,
        bytes: rest.to_vec(),
    })
}

// Sample input: 80 60 ff fe 00 00 00 00 00 00 00 00, version 2, payload type 96, sequence number 65534, then timestamp and source.
fn rtp_sequence(packet: &[u8]) -> Option<u16> {
    let mut rest = packet;
    let [first, _payload_type] = take(&mut rest)?;
    let sequence = u16::from_be_bytes(take(&mut rest)?);
    let _timestamp_and_source: [u8; 8] = take(&mut rest)?;
    (first >> RTP_VERSION_SHIFT == RTP_VERSION).then_some(sequence)
}

/// The first channel to open is signalling and the next is media.
pub(crate) fn roles(channels: &Channels, link: usize) -> Vec<(Role, &Channel)> {
    let mut roles: Vec<(Role, &Channel)> = Vec::new();
    for channel in &channels.list {
        if channel.psm != AVDTP_PSM || channel.link != link {
            continue;
        }
        let open = |role: Role| {
            roles.iter().any(|(earlier_role, earlier)| {
                *earlier_role == role && earlier.closed.is_none_or(|closed| closed > channel.opened)
            })
        };
        let role = if !open(Role::Signalling) {
            Role::Signalling
        } else if !open(Role::Media) {
            Role::Media
        } else {
            Role::Other
        };
        roles.push((role, channel));
    }
    roles
}

impl Tables {
    fn read(channels: &Channels, link: usize) -> Self {
        let mut tables = Self::default();
        let roles = roles(channels, link);
        // Every start command is known before the media is read, since a start decides whether a jump in sequence is a gap.
        for (role, channel) in &roles {
            if *role == Role::Signalling {
                tables.signalling(channel);
            }
        }
        for (role, channel) in &roles {
            match role {
                Role::Signalling => {}
                Role::Media => tables.media(channel),
                Role::Other => bump(&mut tables.unread.other_channels),
            }
        }
        tables.count_packets();
        tables
    }

    fn second(&mut self, at_ns: u64) -> &mut Second {
        let whole = at_ns / NANOS_PER_SECOND;
        self.seconds.entry(whole).or_insert(Second {
            at_s: whole,
            ..Second::default()
        })
    }

    // Sample input: 01 00 07 06 00 00 ff ff 02 35, Media Transport with no data, then Media Codec with 6 bytes: audio, SBC, four codec bytes.
    fn codecs(&mut self, mut rest: &[u8]) -> Vec<Codec> {
        let mut found = Vec::new();
        while !rest.is_empty() {
            let Some([category, len]) = take(&mut rest) else {
                bump(&mut self.unread.malformed_signals);
                break;
            };
            let Some((data, after)) = rest.split_at_checked(usize::from(len)) else {
                bump(&mut self.unread.malformed_signals);
                break;
            };
            rest = after;
            if category != MEDIA_CODEC {
                continue;
            }
            match codec(data) {
                Some(codec) => found.push(codec),
                None => bump(&mut self.unread.malformed_signals),
            }
        }
        found
    }

    // Sample input: 04 04 07 06 00 02 80 01 8c 84, the two stream end point bytes of a Set Configuration command, then its capabilities.
    fn proposal(&mut self, signal: Signal<'_>) -> Option<Proposal> {
        let mut rest = signal.body;
        let Some([_acceptor, _initiator]) = take(&mut rest) else {
            bump(&mut self.unread.malformed_signals);
            return None;
        };
        Some(Proposal {
            label: signal.label,
            codec: self.codecs(rest).into_iter().next(),
        })
    }

    fn signalling(&mut self, channel: &Channel) {
        let mut proposals = Proposals::default();
        for sdu in &channel.sdus {
            let signal = match packet(&sdu.sdu) {
                Packet::Single(signal) => signal,
                Packet::Fragment => {
                    bump(&mut self.unread.fragmented_signals);
                    continue;
                }
                Packet::Malformed => {
                    bump(&mut self.unread.malformed_signals);
                    continue;
                }
            };
            match (signal.id, signal.message) {
                (GET_CAPABILITIES | GET_ALL_CAPABILITIES, RESPONSE_ACCEPT)
                    if sdu.direction == Direction::FromPods =>
                {
                    let offered = self.codecs(signal.body);
                    self.capabilities.extend(offered);
                }
                (SET_CONFIGURATION, COMMAND) => {
                    *proposals.of(sdu.direction) = self.proposal(signal);
                }
                (SET_CONFIGURATION, RESPONSE_ACCEPT | RESPONSE_REJECT) => {
                    // Only the other side's answer with the command's own label settles a proposal.
                    let proposer = match sdu.direction {
                        Direction::ToPods => Direction::FromPods,
                        Direction::FromPods => Direction::ToPods,
                    };
                    let answered = proposals
                        .of(proposer)
                        .take_if(|proposal| proposal.label == signal.label);
                    let accepted = answered.filter(|_| signal.message == RESPONSE_ACCEPT);
                    if let Some(codec) = accepted.and_then(|proposal| proposal.codec) {
                        self.configuration = Some(codec);
                    }
                }
                (START, COMMAND) => {
                    self.starts.insert(sdu.at_ns);
                    bump(&mut self.second(sdu.at_ns).starts);
                }
                (SUSPEND, COMMAND) => bump(&mut self.second(sdu.at_ns).suspends),
                _ => {}
            }
        }
    }

    /// Whether a start command came after the packet stamped `before` and no later than the one stamped `now`.
    fn restarted(&self, before: u64, now: u64) -> bool {
        // A span that is empty or runs backwards holds no start, and a range may not end before it begins.
        let Some(after) = before.checked_add(1).filter(|after| *after <= now) else {
            return false;
        };
        self.starts.range(after..=now).next().is_some()
    }

    fn media(&mut self, channel: &Channel) {
        let mut previous: Option<(u16, u64)> = None;
        for sdu in &channel.sdus {
            if sdu.direction != Direction::ToPods {
                continue;
            }
            let Some(sequence) = rtp_sequence(&sdu.sdu) else {
                bump(&mut self.unread.not_rtp);
                continue;
            };
            let gap = previous.is_some_and(|(before, before_ns)| {
                sequence != before.wrapping_add(1) && !self.restarted(before_ns, sdu.at_ns)
            });
            previous = Some((sequence, sdu.at_ns));
            self.packets.push(MediaPacket {
                at_ns: sdu.at_ns,
                len: u64::try_from(sdu.sdu.len()).unwrap_or(u64::MAX),
                gap,
            });
        }
    }

    /// Fills the rows from the packets in time order, so an interval spans a change of channel.
    fn count_packets(&mut self) {
        let mut packets = std::mem::take(&mut self.packets);
        packets.sort_by_key(|packet| packet.at_ns);
        let mut previous: Option<u64> = None;
        for packet in packets {
            let interval = previous.map_or(0, |before| packet.at_ns.saturating_sub(before));
            previous = Some(packet.at_ns);
            let row = self.second(packet.at_ns);
            bump(&mut row.packets);
            row.bytes = row.bytes.saturating_add(packet.len);
            row.gaps = row.gaps.saturating_add(u64::from(packet.gap));
            row.largest_interval_ns = row.largest_interval_ns.max(interval);
        }
    }
}

/// Reads the AVDTP channels of the link numbered `link` in `Channel::link`, in one pass; no other link's stream is mixed in.
pub(crate) fn music(channels: &Channels, link: usize) -> Music {
    let tables = Tables::read(channels, link);
    Music {
        capabilities: tables.capabilities,
        configuration: tables.configuration,
        seconds: tables.seconds.into_values().collect(),
        unread: tables.unread,
    }
}

#[cfg(test)]
mod tests;
